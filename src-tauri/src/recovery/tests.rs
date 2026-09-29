//! Tests for the wallet flows: a stub node for the logic here, and (ignored unless a freebankd is
//! given) the same code against a real node, in real_node.rs.

use super::job::{rescan, rescan_renewing, set_seed, unlock, Report, UNLOCK_SECS, WRONG_PASSPHRASE};
use crate::wallet::RelockGuard;
use super::ops::{self, change_passphrase, move_coins, move_plan, open_saved};
use super::*;
use crate::rpc::stub;
use crate::seed::{Chain, Kdf};
use serde_json::json;
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fbrecovery-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const QUICK: Kdf = Kdf { m_kib: 64, t: 1, p: 1 };

/// A seed file for fresh words, sealed under `pass`; returns the entropy and the HD seed's key id.
fn saved_words(app_dir: &Path, pass: &str, kdf: Kdf) -> (seed::Entropy, [u8; 20]) {
    let e = seed::new_entropy();
    let id = seed::key_id(&seed::freebank_hd_seed(&e).unwrap()).unwrap();
    seed::write_private(&seed::seed_path(app_dir), &seed::seal(&e, &id, pass, kdf).unwrap()).unwrap();
    (e, id)
}

#[test]
fn amounts() {
    assert_eq!(ecx(150_000_000), "1.50000000");
    assert_eq!(ecx(1), "0.00000001");
    assert_eq!(ecx(0), "0.00000000");
    assert_eq!(ecx(-2_500), "-0.00002500");
    assert_eq!(ecx(2_100_000_012_345_678), "21000000.12345678");
    assert_eq!(sats(&json!(1.5)), 150_000_000);
    assert_eq!(sats(&json!(0.00000001)), 1);
    assert_eq!(sats(&json!(21000000.12345678)), 2_100_000_012_345_678);
    assert_eq!(sats(&json!(0.1)), 10_000_000);
    assert_eq!(sats(&json!(null)), 0);
}

#[test]
fn the_wallet_folder_follows_core() {
    let d = temp("walletdir");
    assert_eq!(default_wallet(&d), d.join("wallet.dat"));
    std::fs::create_dir_all(d.join("wallets")).unwrap();
    assert_eq!(default_wallet(&d), d.join("wallets/wallet.dat"));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn protection_reads_the_wallet_and_the_seed_file() {
    let d = temp("protection");
    let plain = json!({"walletname": "wallet.dat", "txcount": 0, "hdmasterkeyid": "aa"});
    let p = protection_from(&plain, &d, true);
    assert!(!p.encrypted && !p.protected && p.new_wallet && p.node_is_ours && p.backup_due);
    assert_eq!(p.app_seed, AppSeed::None);

    // Words saved, and the wallet's HD seed is theirs: protected once it has a passphrase.
    let (_, id) = saved_words(&d, "pass", QUICK);
    let hd = seed::key_id_hex(&id);
    let enc = json!({"txcount": 3, "hdmasterkeyid": hd, "unlocked_until": 0});
    let p = protection_from(&enc, &d, false);
    assert!(p.encrypted && p.protected && !p.new_wallet && !p.node_is_ours);
    assert_eq!(p.app_seed, AppSeed::Matches);
    assert!(!p.words_confirmed, "nothing recorded yet");
    let unenc = json!({"txcount": 3, "hdmasterkeyid": hd});
    assert!(!protection_from(&unenc, &d, true).protected, "no passphrase: not protected");

    // The words belong to another seed (a restored file, or sethdseed by hand).
    let other = json!({"txcount": 3, "hdmasterkeyid": "bb", "unlocked_until": 0});
    let p = protection_from(&other, &d, true);
    assert_eq!(p.app_seed, AppSeed::Other);
    assert!(!p.protected);

    // What the app recorded counts only for the seed it recorded it for.
    Record {
        seed_id: Some(hd.clone()),
        seed_set_at: Some(10),
        words_confirmed_at: Some(20),
        backup_at: Some(30),
        backup_seed_id: Some(hd.clone()),
        backup_path: Some("/b".into()),
    }
    .save(&d)
    .unwrap();
    let p = protection_from(&enc, &d, true);
    assert!(p.words_confirmed && !p.backup_due);
    assert_eq!((p.seed_set_at, p.backup_at), (Some(10), Some(30)));
    let p = protection_from(&other, &d, true);
    assert!(!p.words_confirmed && p.backup_due && p.seed_set_at.is_none());

    // A seed file that can't be read counts as none, and says why.
    std::fs::write(seed::seed_path(&d), b"garbage").unwrap();
    let p = protection_from(&enc, &d, true);
    assert_eq!(p.app_seed, AppSeed::None);
    assert!(p.seed_file_problem.unwrap().contains("isn't a FreeBank seed file"));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn addresses_wait_for_a_new_wallet_only() {
    let d = temp("held");
    // New and unprotected: held, with or without a passphrase.
    assert!(addresses_held(&json!({"txcount": 0, "hdmasterkeyid": "aa"}), &d));
    assert!(addresses_held(&json!({"txcount": 0, "hdmasterkeyid": "aa", "unlocked_until": 0}), &d));
    // An older wallet (it has transactions): its addresses show, and its banner asks for the passphrase.
    assert!(!addresses_held(&json!({"txcount": 2, "hdmasterkeyid": "aa"}), &d));
    // Protected: a passphrase, and FreeBank's words give its seed.
    let (_, id) = saved_words(&d, "pass", QUICK);
    let hd = seed::key_id_hex(&id);
    assert!(!addresses_held(&json!({"txcount": 0, "hdmasterkeyid": hd, "unlocked_until": 0}), &d));
    assert!(addresses_held(&json!({"txcount": 0, "hdmasterkeyid": hd}), &d), "no passphrase yet");
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn a_restores_folder_is_recorded_like_setups() {
    let d = temp("recordaside");
    let mgr = crate::node::NodeManager::new(d.join("app"));
    let aside = d.join(".freebank.old-1790700000");
    job::record_moved_aside(&mgr, &aside).await.unwrap();
    job::record_moved_aside(&mgr, &aside).await.unwrap();
    let want = vec![aside.to_string_lossy().into_owned()];
    assert_eq!(mgr.settings.lock().await.moved_aside, want, "once");
    // Written to settings.json, as setup's records are.
    assert_eq!(crate::node::NodeManager::new(d.join("app")).settings.lock().await.moved_aside, want);
    job::forget_moved_aside(&mgr, &aside).await.unwrap();
    assert!(mgr.settings.lock().await.moved_aside.is_empty());
    assert!(crate::node::NodeManager::new(d.join("app")).settings.lock().await.moved_aside.is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_record_is_private() {
    let d = temp("record");
    assert_eq!(Record::load(&d), Record::default());
    let r = Record { backup_at: Some(5), ..Default::default() };
    r.save(&d).unwrap();
    assert_eq!(Record::load(&d), r);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(Record::path(&d)).unwrap().permissions().mode() & 0o777, 0o600);
    }
    std::fs::remove_dir_all(&d).unwrap();
}

/// A node whose HD seed sethdseed changes, with getaddressinfo answering for the seed's first
/// address. `twist` makes it derive something else.
fn seed_node(twist: bool) -> (FreeBankClient, stub::Calls, Arc<Mutex<Option<String>>>) {
    let current: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("0000".into())));
    let hd_first: Arc<Mutex<Option<(String, String)>>> = Arc::default();
    let cur = current.clone();
    let (c, calls) = stub::serve(move |m, p| match m {
        "getwalletinfo" => Ok(json!({"hdmasterkeyid": cur.lock().unwrap().clone(), "unlocked_until": 99})),
        "sethdseed" => {
            let wif = p[1].as_str().unwrap();
            let raw = bitcoin::base58::decode_check(wif).unwrap();
            let hd: [u8; 32] = raw[1..33].try_into().unwrap();
            if raw[0] != 128 || raw[33] != 1 || p[0] != json!(true) {
                return Err((-5, "Invalid private key encoding".into()));
            }
            let id = seed::key_id_hex(&seed::key_id(&hd).unwrap());
            if cur.lock().unwrap().as_deref() == Some("seen-before") {
                return Err((-5, "Already have this key (either as an HD seed or as a loose private key)".into()));
            }
            *cur.lock().unwrap() = Some(id.clone());
            *hd_first.lock().unwrap() = Some((seed::address(&hd, false, 0).unwrap(), id));
            Ok(Value::Null)
        }
        "getaddressinfo" => {
            let (addr, id) = hd_first.lock().unwrap().clone().unwrap();
            if p[0] != json!(addr) {
                return Ok(json!({"ismine": false}));
            }
            let path = if twist { "m/0'/0'/1'" } else { "m/0'/0'/0'" };
            Ok(json!({"ismine": true, "hdkeypath": path, "hdmasterkeyid": id}))
        }
        _ => Err((-32601, "Method not found".into())),
    });
    (c, calls, current)
}

#[tokio::test]
async fn the_seed_is_set_and_checked() {
    let (mut c, calls, current) = seed_node(false);
    let e = seed::new_entropy();
    let want = seed::key_id(&seed::freebank_hd_seed(&e).unwrap()).unwrap();
    assert_eq!(set_seed(&mut c, &e, Chain::Main).await.unwrap(), want);
    assert_eq!(current.lock().unwrap().clone(), Some(seed::key_id_hex(&want)));
    // Set again with the same words: nothing to do, and no second sethdseed.
    set_seed(&mut c, &e, Chain::Main).await.unwrap();
    assert_eq!(calls.lock().unwrap().iter().filter(|(m, _)| m == "sethdseed").count(), 1);

    // A node that derives another address from the seed: stop.
    let (mut c, _, _) = seed_node(true);
    assert!(set_seed(&mut c, &seed::new_entropy(), Chain::Main).await.unwrap_err().contains("different addresses"));
    // A seed the wallet had before.
    let (mut c, _, current) = seed_node(false);
    *current.lock().unwrap() = Some("seen-before".into());
    let err = set_seed(&mut c, &e, Chain::Main).await.unwrap_err();
    assert!(err.contains("can't go back to an earlier seed"), "{}", err);
}

#[tokio::test]
async fn the_scan_covers_every_block_once() {
    let ranges: Arc<Mutex<Vec<(u64, u64)>>> = Arc::default();
    let unlocks: Arc<Mutex<u32>> = Arc::default();
    let (r, u) = (ranges.clone(), unlocks.clone());
    let tip = Arc::new(Mutex::new(12_345u64));
    let t = tip.clone();
    let (mut c, _) = stub::serve(move |m, p| match m {
        "getblockcount" => {
            let h = *t.lock().unwrap();
            // A block arrives during the scan: the second question sees it.
            *t.lock().unwrap() = 12_347;
            Ok(json!(h))
        }
        "rescanblockchain" => {
            r.lock().unwrap().push((p[0].as_u64().unwrap(), p[1].as_u64().unwrap()));
            Ok(json!({"start_height": p[0], "stop_height": p[1]}))
        }
        "walletpassphrase" => {
            assert_eq!(p[1], json!(UNLOCK_SECS), "the longest unlock the app asks for");
            *u.lock().unwrap() += 1;
            Ok(Value::Null)
        }
        "getwalletinfo" => Ok(json!({"unlocked_until": 99})),
        _ => Err((-32601, "Method not found".into())),
    });
    let report = Report::default();
    let guard = RelockGuard::default();
    rescan(&mut c, Some(("pass", &guard)), &report).await.unwrap();
    let got = ranges.lock().unwrap().clone();
    let mut next = 0;
    for (a, b) in &got {
        assert_eq!(*a, next, "{:?}", got);
        assert!(b >= a);
        next = b + 1;
    }
    assert_eq!(next, 12_348, "every block up to the new tip, once");
    assert!(got.len() > 2 && got.len() < 40, "chunked: {} calls", got.len());
    assert_eq!(*unlocks.lock().unwrap(), 1, "unlocked once, through the guard, before the first chunk");
    let p = report.get();
    assert_eq!((p.scan_at, p.scan_to), (Some(12_347), Some(12_347)));

    // Without a passphrase it never unlocks.
    let (mut c, calls) = stub::serve(|m, p| match m {
        "getblockcount" => Ok(json!(10)),
        "rescanblockchain" => Ok(json!({"start_height": p[0], "stop_height": p[1]})),
        _ => Err((-32601, "Method not found".into())),
    });
    rescan(&mut c, None, &Report::default()).await.unwrap();
    assert!(calls.lock().unwrap().iter().all(|(m, _)| m != "walletpassphrase"));
}

#[tokio::test]
async fn a_long_scan_unlocks_again_only_when_due() {
    let unlocks: Arc<Mutex<u32>> = Arc::default();
    let u = unlocks.clone();
    let (mut c, calls) = stub::serve(move |m, p| match m {
        "getblockcount" => Ok(json!(3_000)),
        "rescanblockchain" => Ok(json!({"start_height": p[0], "stop_height": p[1]})),
        "walletpassphrase" => {
            *u.lock().unwrap() += 1;
            Ok(Value::Null)
        }
        "getwalletinfo" => Ok(json!({"unlocked_until": 99})),
        _ => Err((-32601, "Method not found".into())),
    });
    // Renewing at once: an unlock before every chunk, and every one of them for 300 s.
    rescan_renewing(&mut c, Some(("pass", &RelockGuard::default())), &Report::default(), Duration::ZERO).await.unwrap();
    let chunks = calls.lock().unwrap().iter().filter(|(m, _)| m == "rescanblockchain").count();
    assert_eq!(*unlocks.lock().unwrap() as usize, chunks);
    assert!(calls.lock().unwrap().iter().filter(|(m, _)| m == "walletpassphrase").all(|(_, p)| p[1] == json!(300)));
}

#[tokio::test]
async fn unlocks_go_through_the_guard_and_say_a_wrong_passphrase_plainly() {
    let (mut c, calls) = stub::serve(|m, p| match m {
        "walletpassphrase" if p[0] == json!("right") => Ok(Value::Null),
        "walletpassphrase" => Err((-14, "Error: The wallet passphrase entered was incorrect.".into())),
        "getwalletinfo" => Ok(json!({"unlocked_until": 99})),
        _ => Err((-32601, "Method not found".into())),
    });
    let guard = RelockGuard::default();
    assert_eq!(unlock(&mut c, &guard, "wrong", UNLOCK_SECS).await.unwrap_err(), WRONG_PASSPHRASE);
    unlock(&mut c, &guard, "right", UNLOCK_SECS).await.unwrap();
    // A second unlock well before the first one's relock isn't held back.
    let t = std::time::Instant::now();
    unlock(&mut c, &guard, "right", UNLOCK_SECS).await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(1));
    assert!(unlock(&mut c, &guard, "", UNLOCK_SECS).await.is_err());
    let asked: Vec<Value> = calls.lock().unwrap().iter().filter(|(m, _)| m == "walletpassphrase").map(|(_, p)| p[1].clone()).collect();
    assert_eq!(asked, vec![json!(300), json!(300), json!(300)], "the empty one never reached the node");
}

/// An encrypted wallet whose passphrase walletpassphrasechange moves from the first to the second.
fn passphrase_node(pass: &str) -> (FreeBankClient, Arc<Mutex<String>>) {
    let current = Arc::new(Mutex::new(pass.to_string()));
    let cur = current.clone();
    let (c, _) = stub::serve(move |m, p| match m {
        "getwalletinfo" => Ok(json!({"unlocked_until": 0, "hdmasterkeyid": "aa"})),
        "walletpassphrasechange" | "walletpassphrase" if p[0] != json!(*cur.lock().unwrap()) => {
            Err((-14, "Error: The wallet passphrase entered was incorrect.".into()))
        }
        "walletpassphrasechange" => {
            *cur.lock().unwrap() = p[1].as_str().unwrap().to_string();
            Ok(Value::Null)
        }
        "walletpassphrase" | "walletlock" => Ok(Value::Null),
        _ => Err((-32601, "Method not found".into())),
    });
    (c, current)
}

#[tokio::test]
async fn changing_the_passphrase_never_unlocks() {
    // Checking the current passphrase is walletpassphrasechange's job: no walletpassphrase, so no
    // relock timer, whether the words' copy opens or not.
    let d = temp("nounlock");
    saved_words(&d, "something else", QUICK);
    let (mut c, calls) = stub::serve(|m, p| match m {
        "getwalletinfo" => Ok(json!({"unlocked_until": 0})),
        "walletpassphrasechange" if p[0] == json!("old horse") => Ok(Value::Null),
        "walletpassphrasechange" => Err((-14, "Error: The wallet passphrase entered was incorrect.".into())),
        _ => Err((-32601, "Method not found".into())),
    });
    assert_eq!(change_passphrase(&d, &mut c, z("typo"), z("new horse")).await.unwrap_err(), "That isn't your current passphrase.");
    assert_eq!(change_passphrase(&d, &mut c, z("old horse"), z("new horse")).await.unwrap().seed_file, "kept");
    assert!(calls.lock().unwrap().iter().all(|(m, _)| m != "walletpassphrase"));
    std::fs::remove_dir_all(&d).unwrap();
}

fn z(s: &str) -> Zeroizing<String> {
    Zeroizing::new(s.to_string())
}

#[tokio::test]
async fn changing_the_passphrase_reseals_the_words() {
    let d = temp("change");
    let (e, id) = saved_words(&d, "old horse", seed::KDF);
    let (mut c, node_pass) = passphrase_node("old horse");

    // Wrong current passphrase: nothing changes, nothing is left behind.
    let err = change_passphrase(&d, &mut c, z("old hors"), z("new horse")).await.unwrap_err();
    assert_eq!(err, "That isn't your current passphrase.");
    assert_eq!(*node_pass.lock().unwrap(), "old horse");
    assert!(!d.join("wallet/seed.enc.new").exists());
    assert!(change_passphrase(&d, &mut c, z("old horse"), z("old horse")).await.is_err());
    assert!(change_passphrase(&d, &mut c, z("old horse"), z("")).await.is_err());

    let r = change_passphrase(&d, &mut c, z("old horse"), z("new horse")).await.unwrap();
    assert_eq!(r, ops::Changed { seed_file: "updated", note: None });
    assert_eq!(*node_pass.lock().unwrap(), "new horse");
    let (opened, oid) = open_saved(&d, "new horse").unwrap();
    assert_eq!((*opened, oid), (*e, id));
    assert!(open_saved(&d, "old horse").unwrap_err().contains("doesn't open"));
    let names: Vec<String> = std::fs::read_dir(d.join("wallet")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["seed.enc".to_string()]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn a_node_that_refuses_the_change_leaves_the_words_alone() {
    let d = temp("refuse");
    saved_words(&d, "old horse", QUICK);
    let before = std::fs::read(seed::seed_path(&d)).unwrap();
    let (mut c, _) = stub::serve(|m, _| match m {
        "getwalletinfo" => Ok(json!({"unlocked_until": 0})),
        "walletpassphrasechange" => Err((-4, "Error: The wallet could not be changed.".into())),
        _ => Err((-32601, "Method not found".into())),
    });
    assert!(change_passphrase(&d, &mut c, z("old horse"), z("new horse")).await.is_err());
    assert_eq!(std::fs::read(seed::seed_path(&d)).unwrap(), before);
    assert!(!d.join("wallet/seed.enc.new").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

#[tokio::test]
async fn changing_without_saved_words_or_with_words_under_another_passphrase() {
    let d = temp("nowords");
    let (mut c, _) = passphrase_node("old horse");
    let r = change_passphrase(&d, &mut c, z("old horse"), z("new horse")).await.unwrap();
    assert_eq!(r.seed_file, "none");
    // The words were saved under another passphrase: the wallet's still changes, the copy stays.
    saved_words(&d, "something else", QUICK);
    let r = change_passphrase(&d, &mut c, z("new horse"), z("third horse")).await.unwrap();
    assert_eq!(r.seed_file, "kept");
    assert!(r.note.unwrap().contains("saved under a different one"));
    assert!(open_saved(&d, "something else").is_ok());
    // An unencrypted wallet has no passphrase to change.
    let (mut plain, _) = stub::serve(|m, _| match m {
        "getwalletinfo" => Ok(json!({"txcount": 0})),
        _ => Err((-32601, "Method not found".into())),
    });
    assert!(change_passphrase(&d, &mut plain, z("a"), z("b")).await.unwrap_err().contains("no passphrase yet"));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_change_stopped_half_way_is_finished_on_the_next_open() {
    let d = temp("halfway");
    let (e, id) = saved_words(&d, "old horse", QUICK);
    // The new copy was written, the node took the new passphrase, the swap didn't happen.
    let path = seed::seed_path(&d);
    std::fs::write(path.with_file_name("seed.enc.new"), seed::seal(&e, &id, "new horse", QUICK).unwrap()).unwrap();
    assert!(open_saved(&d, "old horse").is_ok(), "the old one still opens with the old passphrase");
    let (opened, _) = open_saved(&d, "new horse").unwrap();
    assert_eq!(*opened, *e);
    assert!(!path.with_file_name("seed.enc.new").exists(), "swapped in");
    assert!(open_saved(&d, "new horse").is_ok());
    let empty = temp("nofile");
    assert!(open_saved(&empty, "x").unwrap_err().contains("no recovery words saved"));
    std::fs::remove_dir_all(&d).unwrap();
    std::fs::remove_dir_all(&empty).unwrap();
}

#[tokio::test]
async fn reveal_needs_the_passphrase() {
    let d = temp("reveal");
    let (e, id) = saved_words(&d, "pass horse", QUICK);
    let hd = seed::key_id_hex(&id);
    let (mut c, _) = stub::serve(move |m, _| match m {
        "getwalletinfo" => Ok(json!({"hdmasterkeyid": hd, "unlocked_until": 0})),
        "getblockchaininfo" => Ok(json!({"chain": "main"})),
        _ => Err((-32601, "Method not found".into())),
    });
    assert!(ops::reveal(&d, &mut c, z("wrong"), "words").await.unwrap_err().contains("doesn't open"));
    assert!(ops::reveal(&d, &mut c, z(""), "words").await.is_err());
    assert!(ops::reveal(&d, &mut c, z("pass horse"), "private keys").await.is_err());
    let w = ops::reveal(&d, &mut c, z("pass horse"), "words").await.unwrap();
    assert_eq!(w.words.as_ref().unwrap().join(" "), seed::words(&e).join(" "));
    assert_eq!((w.xprv.is_none(), w.matches_wallet), (true, Some(true)));
    let x = ops::reveal(&d, &mut c, z("pass horse"), "xprv").await.unwrap();
    let hd_seed = seed::freebank_hd_seed(&e).unwrap();
    assert_eq!(x.xprv.as_deref(), Some(seed::master_xprv(&hd_seed, Chain::Main).unwrap().as_str()));
    assert!(x.words.is_none());
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn backup_files_are_looked_at_before_restoring() {
    let mut page = vec![0u8; 8192];
    page[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
    assert_eq!(ops::inspect(&page).unwrap(), (None, false));
    let mut enc = page.clone();
    enc[5000..5005].copy_from_slice(b"\x04mkey");
    enc[6000..6008].copy_from_slice(b"\x07hdchain");
    assert_eq!(ops::inspect(&enc).unwrap(), (Some(true), true));
    let mut plain = page.clone();
    plain[5000..5004].copy_from_slice(b"\x03key");
    assert_eq!(ops::inspect(&plain).unwrap(), (Some(false), false));
    // "ckey" (an encrypted key) isn't "key".
    let mut ckey = page.clone();
    ckey[5000..5005].copy_from_slice(b"\x04ckey");
    assert_eq!(ops::inspect(&ckey).unwrap().0, None);
    assert!(ops::inspect(&page[..4000]).unwrap_err().contains("too small"));
    let mut not_bdb = page.clone();
    not_bdb[12] = 0;
    assert!(ops::inspect(&not_bdb).is_err());

    let d = temp("upload");
    let f = ops::take_upload(&d, &enc).unwrap();
    assert_eq!((f.size, f.encrypted, f.hd), (8192, Some(true), true));
    assert!(ops::claim_upload("nope").is_err());
    let path = ops::claim_upload(&f.token).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), enc);
    assert!(path.starts_with(ops::upload_dir(&d)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(ops::claim_upload(&f.token).is_err(), "once");
    // A newer upload replaces an older one, whose copy goes.
    let a = ops::take_upload(&d, &page).unwrap();
    let b = ops::take_upload(&d, &plain).unwrap();
    assert!(ops::claim_upload(&a.token).is_err());
    assert_eq!(std::fs::read(ops::claim_upload(&b.token).unwrap()).unwrap(), plain);
    assert!(ops::take_upload(&d, b"short").is_err());
    // Copies nobody restored (an earlier run of the app) go; the pending one stays.
    std::fs::write(ops::upload_dir(&d).join("upload-0000000000000001.dat"), b"stale").unwrap();
    let pending = ops::take_upload(&d, &page).unwrap();
    std::fs::write(ops::upload_dir(&d).join("upload-0000000000000002.dat"), b"stale").unwrap();
    ops::forget_uploads(&d);
    let left: Vec<PathBuf> = std::fs::read_dir(ops::upload_dir(&d)).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(left, vec![ops::claim_upload(&pending.token).unwrap()]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_replaced_wallet_moves_beside_the_data_folder() {
    let base = temp("aside");
    let datadir = base.join(".freebank");
    std::fs::create_dir_all(&datadir).unwrap();
    let mut w = vec![0u8; 4096];
    w[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
    std::fs::write(datadir.join("wallet.dat"), &w).unwrap();
    let moved = job::move_wallet_aside(&datadir, &datadir.join("wallet.dat")).unwrap();
    assert!(!datadir.join("wallet.dat").exists());
    assert_eq!(std::fs::read(&moved).unwrap(), w);
    let dir = moved.parent().unwrap();
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    // <datadir>.old-<digits>, the way setup moves a data folder aside, so Back up and Obliterate see it.
    let digits = name.strip_prefix(".freebank.old-").unwrap();
    assert!(!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()), "{}", name);
    assert!(moved.file_name().unwrap().to_string_lossy().starts_with("wallet.dat.old-"));
    // The node's folder no longer counts it as one of its wallets (Back up while running needs one).
    assert!(crate::node::wallet_files(&datadir).is_empty());
    assert_eq!(crate::node::wallet_files(dir), vec![moved.clone()]);
    // A second one in the same second gets its own folder.
    std::fs::write(datadir.join("wallet.dat"), &w).unwrap();
    let again = job::move_wallet_aside(&datadir, &datadir.join("wallet.dat")).unwrap();
    assert_ne!(again.parent(), moved.parent());
    std::fs::remove_dir_all(&base).unwrap();
}

/// A wallet with coins on an old seed's addresses, on the current seed's, an imported key's, and one
/// it can't spend.
fn move_node(unlocked: bool, mempool_bytes: u64, twist: bool) -> (FreeBankClient, stub::Calls) {
    stub::serve(move |m, p| match m {
        "getwalletinfo" => Ok(json!({"hdmasterkeyid": "new", "unlocked_until": if unlocked { 99 } else { 0 }})),
        "listunspent" => {
            if *p != json!([0, 9_999_999, [], false]) {
                return Err((-8, "unexpected listunspent arguments".into()));
            }
            Ok(json!([
                {"txid": "t1", "vout": 0, "address": "Xold1", "amount": 1.5, "spendable": true},
                {"txid": "t2", "vout": 1, "address": "Xold2", "amount": 0.25, "spendable": true},
                {"txid": "t3", "vout": 0, "address": "Xnew1", "amount": 2.0, "spendable": true},
                {"txid": "t4", "vout": 0, "address": "Xwatch", "amount": 9.0, "spendable": false},
                {"txid": "t5", "vout": 2, "address": "Ximported", "amount": 0.01, "spendable": true}
            ]))
        }
        "getaddressinfo" => Ok(match p[0].as_str().unwrap() {
            "Xold1" | "Xold2" => json!({"ismine": true, "hdmasterkeyid": "old"}),
            "Xnew1" | "Xto" => json!({"ismine": true, "hdmasterkeyid": "new"}),
            _ => json!({"ismine": true}),
        }),
        "listmynotes" => Ok(json!([{"house_id": 1}])),
        "getnewaddress" => Ok(json!("Xto")),
        "createrawtransaction" => Ok(json!("raw")),
        "getmempoolinfo" => Ok(json!({"size": 10, "bytes": mempool_bytes})),
        "estimatesmartfee" => Ok(json!({"feerate": 0.00004321, "blocks": 2})),
        "fundrawtransaction" => Ok(json!({"hex": "funded", "fee": 0.00000500, "changepos": -1})),
        "decoderawtransaction" => Ok(json!({
            "vin": [{"txid": "t1", "vout": 0}, {"txid": "t2", "vout": 1}, {"txid": "t5", "vout": 2}],
            "vout": [{"value": if twist { 1.0 } else { 1.75999500 }, "scriptPubKey": {"addresses": ["Xto"]}}]
        })),
        "signrawtransactionwithwallet" => Ok(json!({"hex": "signed", "complete": true})),
        "sendrawtransaction" => Ok(json!("f".repeat(64))),
        _ => Err((-32601, "Method not found".into())),
    })
}

#[tokio::test]
async fn coins_move_to_one_new_address() {
    let (mut c, calls) = move_node(true, 1_000, false);
    let plan = move_plan(&mut c).await.unwrap();
    assert_eq!(plan, ops::MovePlan { coins: 3, total_sats: 176_000_000, later: 0, has_notes: true });
    calls.lock().unwrap().clear();

    let m = move_coins(&mut c).await.unwrap();
    assert_eq!(m, ops::Moved { txid: "f".repeat(64), coins: 3, sent_sats: 175_999_500, fee_sats: 500, to: "Xto".into(), later: 0 });
    let calls = calls.lock().unwrap().clone();
    let get = |name: &str| calls.iter().find(|(m, _)| m == name).map(|(_, p)| p.clone()).unwrap();
    // The old seed's coins and the imported one, not the new seed's or the unspendable one, all of
    // it to one output, in exact decimals.
    assert_eq!(
        get("createrawtransaction"),
        json!([[{"txid": "t1", "vout": 0}, {"txid": "t2", "vout": 1}, {"txid": "t5", "vout": 2}], {"Xto": "1.76000000"}])
    );
    // The fee comes out of the amount, at 1 sat/vB while the mempool is under a block.
    assert_eq!(get("fundrawtransaction"), json!(["raw", {"subtractFeeFromOutputs": [0], "feeRate": "0.00001000"}]));
    assert_eq!(get("sendrawtransaction"), json!(["signed"]));
    assert!(calls.iter().all(|(m, _)| m != "estimatesmartfee"));
}

#[tokio::test]
async fn a_busy_mempool_pays_the_estimate() {
    let (mut c, calls) = move_node(true, 2_500_000, false);
    move_coins(&mut c).await.unwrap();
    let fund = calls.lock().unwrap().iter().find(|(m, _)| m == "fundrawtransaction").unwrap().1.clone();
    assert_eq!(fund[1]["feeRate"], json!("0.00004321"));
}

#[tokio::test]
async fn a_locked_wallet_is_asked_to_unlock_before_anything_is_made() {
    let (mut c, calls) = move_node(false, 1_000, false);
    let e = move_coins(&mut c).await.unwrap_err();
    assert!(e.starts_with("RPC error -13: "), "{}", e);
    assert!(calls.lock().unwrap().iter().all(|(m, _)| m == "getwalletinfo"), "no address made");
}

#[tokio::test]
async fn a_transaction_that_isnt_what_was_asked_for_is_never_signed() {
    let (mut c, calls) = move_node(true, 1_000, true);
    assert!(move_coins(&mut c).await.unwrap_err().contains("different transaction"));
    assert!(calls.lock().unwrap().iter().all(|(m, _)| m != "signrawtransactionwithwallet" && m != "sendrawtransaction"));
}

#[tokio::test]
async fn nothing_to_move() {
    let (mut c, _) = stub::serve(|m, _| match m {
        "getwalletinfo" => Ok(json!({"hdmasterkeyid": "new", "unlocked_until": 99})),
        "listunspent" => Ok(json!([{"txid": "t3", "vout": 0, "address": "Xnew1", "amount": 2.0, "spendable": true}])),
        "getaddressinfo" => Ok(json!({"ismine": true, "hdmasterkeyid": "new"})),
        "listmynotes" => Ok(json!([])),
        _ => Err((-32601, "Method not found".into())),
    });
    assert_eq!(move_plan(&mut c).await.unwrap(), ops::MovePlan { coins: 0, total_sats: 0, later: 0, has_notes: false });
    assert!(move_coins(&mut c).await.unwrap_err().starts_with("Nothing to move"));
}
