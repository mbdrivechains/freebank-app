//! The eCash wallets against a stand-in eCash node (rpc::stub), which sees which wallet each call is for, and two
//! real-node tests (ignored). Since the security review (H1, H2): the node's wallets are watch-only, FreeBank checks and
//! signs every payment, and neither the passphrase nor a private key goes to the node.

use super::bmm::{self, Bids, Bmm, Coin, Did, Round};
use super::conn::Conn;
use super::keys::{Account, AccountKey, Root};
use super::sign::{self, Expect};
use super::wallet::{self, Quote};
use crate::rpc::stub;
use crate::seed::Chain;
use bitcoin::bip32::DerivationPath;
use bitcoin::consensus::encode;
use bitcoin::psbt::Psbt;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::str::FromStr;

const WORDS: [u8; 32] = [7u8; 32];

fn keys(chain: Chain) -> (AccountKey, AccountKey) {
    let r = Root::from_words(&WORDS, chain).unwrap();
    (r.account(chain, Account::Main).unwrap(), r.account(chain, Account::Bids).unwrap())
}

fn conn_for(c: &crate::rpc::FreeBankClient) -> Conn {
    Conn::for_test(c.url().unwrap(), Chain::Regtest)
}

fn methods(calls: &stub::Calls) -> Vec<String> {
    calls.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
}

fn params_of(calls: &stub::Calls, method: &str) -> Vec<Value> {
    calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
}

fn h(fill: &str) -> String {
    fill.repeat(32)
}

// ---- Wallets ----

#[tokio::test]
async fn the_wallets_are_watch_only_and_get_only_public_keys() {
    let (main, _) = keys(Chain::Regtest);
    let first = main.public.address(0, 0).unwrap();
    let imported = std::sync::Arc::new(std::sync::Mutex::new(false));
    let imp = imported.clone();
    let (c, calls) = stub::serve_paths(move |_, m, p| match m {
        "listwallets" => Ok(json!([])),
        "loadwallet" => Err((-18, "Path does not exist.".into())),
        "createwallet" => Ok(json!({"name": p[0]})),
        "importdescriptors" => {
            *imp.lock().unwrap() = true;
            Ok(json!([{"success": true}, {"success": true}]))
        }
        "getwalletinfo" => Ok(json!({"private_keys_enabled": false})),
        "getaddressinfo" => Ok(json!({"ismine": *imported.lock().unwrap() && p[0] == first})),
        _ => Err((-32601, "Method not found".into())),
    });
    wallet::create(&conn_for(&c), "freebank-ecash-x", &main.public).await.unwrap();
    // name, disable_private_keys, blank, passphrase (none: it holds nothing secret), avoid_reuse, descriptors, load_on_startup
    assert_eq!(params_of(&calls, "createwallet")[0], json!(["freebank-ecash-x", true, true, "", false, true, true]));
    let req = &params_of(&calls, "/wallet/freebank-ecash-x importdescriptors")[0][0];
    for d in req.as_array().unwrap() {
        let desc = d["desc"].as_str().unwrap();
        assert!(desc.contains("tpub") && !desc.contains("prv"), "public keys only: {desc}");
    }
    assert!(!methods(&calls).iter().any(|m| m.contains("walletpassphrase") || m.contains("encryptwallet")), "no passphrase");
}

#[tokio::test]
async fn a_wallet_of_that_name_that_can_sign_or_has_other_keys_is_left_alone() {
    let (main, _) = keys(Chain::Regtest);
    let (c, calls) = stub::serve_paths(|_, m, _| match m {
        "listwallets" => Ok(json!(["freebank-ecash-x"])),
        // A wallet with private keys (a v0.2.6 build before the review, or anyone's) isn't ours.
        "getwalletinfo" => Ok(json!({"private_keys_enabled": true})),
        "getaddressinfo" => Ok(json!({"ismine": true})),
        _ => Err((-32601, "Method not found".into())),
    });
    let e = wallet::create(&conn_for(&c), "freebank-ecash-x", &main.public).await.unwrap_err();
    assert!(e.contains("isn't this one") && e.contains("won't change it"), "{e}");
    assert!(!methods(&calls).iter().any(|m| m.contains("createwallet") || m.contains("importdescriptors")));
}

#[tokio::test]
async fn an_address_from_the_node_is_checked_against_the_key() {
    let (main, _) = keys(Chain::Regtest);
    let fp = main.public.fingerprint;
    let good = main.public.address(0, 5).unwrap();
    let good_desc = format!("wpkh([{}/84h/1h/0h/0/5]{})#x", fp, main.public.pubkey(0, 5).unwrap());
    let g = (good.clone(), good_desc.clone());
    let (c, _) = stub::serve_paths(move |_, m, p| match m {
        "getnewaddress" => Ok(json!(g.0)),
        "getaddressinfo" if p[0] == g.0 => Ok(json!({"desc": g.1})),
        _ => Err((-32601, "Method not found".into())),
    });
    assert_eq!(wallet::new_address(&conn_for(&c), "w", &main.public).await.unwrap(), good);
    // Someone gave the node other keys (review M3): its address doesn't match what the key gives for its place.
    let foreign = keys(Chain::Regtest).1.public.address(0, 5).unwrap();
    let f = (foreign.clone(), good_desc);
    let (c, _) = stub::serve_paths(move |_, m, p| match m {
        "getnewaddress" => Ok(json!(f.0)),
        "getaddressinfo" if p[0] == f.0 => Ok(json!({"desc": f.1})),
        _ => Err((-32601, "Method not found".into())),
    });
    let e = wallet::new_address(&conn_for(&c), "w", &main.public).await.unwrap_err();
    assert!(e.contains("isn't this wallet's"), "{e}");
}

// ---- Checking and signing a payment ----

/// A PSBT as a watch-only wallet would fund it: one coin of the account at (0, 3), to `to` for `sats`, change to (1, 2)
/// unless `change` says otherwise, `fee` left over.
fn parent(coin: TxOut) -> Transaction {
    Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint { txid: Txid::from_str(&h("ab")).unwrap(), vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut { value: Amount::from_sat(5_000), script_pubkey: ScriptBuf::new() }, coin],
    }
}

fn funded(acct: &AccountKey, to: ScriptBuf, sats: u64, fee: u64, change: Option<ScriptBuf>) -> String {
    let p = &acct.public;
    let coin_script = p.script(0, 3).unwrap();
    let value = 1_000_000u64;
    let change_script = change.unwrap_or_else(|| p.script(1, 2).unwrap());
    let prev = parent(TxOut { value: Amount::from_sat(value), script_pubkey: coin_script.clone() });
    let tx = Transaction {
        version: bitcoin::transaction::Version::TWO,
        lock_time: bitcoin::absolute::LockTime::from_consensus(super::REPLAY_LOCKTIME),
        input: vec![TxIn {
            previous_output: OutPoint { txid: prev.compute_txid(), vout: 1 },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xffff_fffd),
            witness: Witness::new(),
        }],
        output: vec![
            TxOut { value: Amount::from_sat(sats), script_pubkey: to },
            TxOut { value: Amount::from_sat(value - sats - fee), script_pubkey: change_script },
        ],
    };
    let mut psbt = Psbt::from_unsigned_tx(tx).unwrap();
    psbt.inputs[0].witness_utxo = Some(TxOut { value: Amount::from_sat(value), script_pubkey: coin_script });
    psbt.inputs[0].non_witness_utxo = Some(prev);
    let acct_n = p.account.index();
    let path = |b: u32, i: u32| DerivationPath::from_str(&format!("m/84'/1'/{}'/{}/{}", acct_n, b, i)).unwrap();
    psbt.inputs[0].bip32_derivation = BTreeMap::from([(p.pubkey(0, 3).unwrap().0, (p.fingerprint, path(0, 3)))]);
    psbt.outputs[1].bip32_derivation = BTreeMap::from([(p.pubkey(1, 2).unwrap().0, (p.fingerprint, path(1, 2)))]);
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(psbt.serialize())
}

#[test]
fn a_payment_is_signed_only_as_asked() {
    let (main, bids) = keys(Chain::Regtest);
    let to = bids.public.script(0, 0).unwrap();
    let expect = Expect { to: to.clone(), sats: 300_000, fee: 1_410, max_fee: wallet::MAX_FEE, max_rate: 11 };
    let psbt = funded(&main, to.clone(), 300_000, 1_410, None);
    let checked = sign::check(&psbt, &main.public, &expect).unwrap();
    assert_eq!((checked.fee, checked.spends.len(), checked.spends[0].place), (1_410, 1, (0, 3)));
    // Signed here: one witness of two items, the second the coin's key.
    let hex = sign::sign(checked.tx, &checked.spends, &main).unwrap();
    let tx: Transaction = encode::deserialize_hex(&hex).unwrap();
    assert_eq!(tx.input[0].witness.len(), 2);
    assert_eq!(tx.input[0].witness.nth(1).unwrap(), &main.public.pubkey(0, 3).unwrap().to_bytes()[..]);
    assert_eq!(tx.lock_time.to_consensus_u32(), 499_999_999);
    // Its ceiling is just above its own rate.
    let ceiling: f64 = sign::max_fee_rate(&hex, 1_410).unwrap().parse().unwrap();
    assert!(ceiling > 1_410.0 / tx.vsize() as f64 * 1e-5 && ceiling < 1e-3, "{ceiling}");
    assert_eq!(sign::max_fee_rate(&hex, 1_410).unwrap().split('.').nth(1).unwrap().len(), 8, "8 decimals, as Core takes");

    // Refused: change to someone else, another amount, another fee, a coin that isn't this account's.
    let foreign = bids.public.script(1, 0).unwrap();
    let bad = funded(&main, to.clone(), 300_000, 1_410, Some(foreign));
    assert!(sign::check(&bad, &main.public, &expect).unwrap_err().contains("pays somewhere it shouldn't"));
    let bad = funded(&main, to.clone(), 299_000, 2_410, None);
    assert!(sign::check(&bad, &main.public, &expect).unwrap_err().contains("isn't the amount"));
    let bad = funded(&main, to.clone(), 300_000, 50_000, None);
    assert!(sign::check(&bad, &main.public, &expect).unwrap_err().contains("another fee"));
    let theirs = funded(&bids, to.clone(), 300_000, 1_410, None);
    assert!(sign::check(&theirs, &main.public, &expect).unwrap_err().contains("isn't this wallet's"));
}

#[test]
fn a_payment_must_carry_the_replay_stamp_no_timelock_and_the_rate_asked() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let (main, bids) = keys(Chain::Regtest);
    let to = bids.public.script(0, 0).unwrap();
    let expect = Expect { to: to.clone(), sats: 300_000, fee: 1_410, max_fee: wallet::MAX_FEE, max_rate: 11 };
    let good = funded(&main, to.clone(), 300_000, 1_410, None);
    let altered = |f: &dyn Fn(&mut Psbt)| {
        let mut p = Psbt::deserialize(&STANDARD.decode(&good).unwrap()).unwrap();
        f(&mut p);
        STANDARD.encode(p.serialize())
    };
    // Re-review L-B: no replay stamp (replayable on Bitcoin), or a sequence that is final or a relative lock.
    let unstamped = altered(&|p| p.unsigned_tx.lock_time = bitcoin::absolute::LockTime::ZERO);
    assert!(sign::check(&unstamped, &main.public, &expect).unwrap_err().contains("replay stamp"));
    for seq in [0xffff_ffffu32, 0xffff_fffe, 10] {
        let held = altered(&|p| p.unsigned_tx.input[0].sequence = Sequence(seq));
        assert!(sign::check(&held, &main.public, &expect).unwrap_err().contains("timelock"), "{seq:x}");
    }
    // Re-review L-C: the rate is bounded here, from the transaction's size: 1,410 sats on ~141 vB is 10 sat/vB.
    let vsize = {
        let p = Psbt::deserialize(&STANDARD.decode(&good).unwrap()).unwrap();
        sign::signed_vsize(&p.unsigned_tx)
    };
    assert!((140..=142).contains(&vsize), "{vsize}");
    let tight = Expect { max_rate: 9, ..expect.clone() };
    assert!(sign::check(&good, &main.public, &tight).unwrap_err().contains("higher fee rate"));
    // A quote asks for its rate, and its payment may be no higher, plus one for rounding.
    let q = Quote { psbt: good.clone(), address: String::new(), sats: 300_000, fee: 1_410, rate: 10 };
    assert_eq!(q.expect(to.clone()).max_rate, 11);
}

#[test]
fn an_address_index_far_beyond_use_isnt_this_wallets() {
    // Re-review L-E: coins there would be stranded from a restore with any usual gap limit.
    let (main, _) = keys(Chain::Regtest);
    let p = &main.public;
    let path = |i: u32| DerivationPath::from_str(&format!("m/84'/1'/0'/0/{i}")).unwrap();
    assert_eq!(p.place(p.fingerprint, &path(super::keys::MAX_INDEX)), Some((0, super::keys::MAX_INDEX)));
    assert_eq!(p.place(p.fingerprint, &path(super::keys::MAX_INDEX + 1)), None);
    assert_eq!(p.place(p.fingerprint, &path(0x7fff_ffff)), None);
}

#[test]
fn an_ecash_address_is_host_and_port_only() {
    // Re-review L-D: reqwest reads "[::1]@" as a login and goes to the host after it.
    let at = |r: &str| super::conn::rpc_endpoint(&crate::node::Settings { l1_rpc: Some(r.into()), ..Default::default() });
    for bad in ["[::1]@evil.example:80", "[::1]:18302@evil.example", "127.0.0.1:1@evil.example", "u:p@127.0.0.1:22120",
                "127.0.0.1:22120/x", "127.0.0.1:22120?a=b", "127.0.0.1:22120#f", "https://127.0.0.1:1"] {
        assert!(at(bad).is_err(), "{bad}");
    }
    assert_eq!(at("127.0.0.1:22120").unwrap(), "127.0.0.1:22120");
    assert_eq!(at("http://127.0.0.1:22120/").unwrap(), "127.0.0.1:22120");
    assert_eq!(at("[::1]:8332").unwrap(), "[::1]:8332");
    assert_eq!(at("localhost").unwrap(), "localhost");
    assert!(super::conn::is_local(&at("[::1]:8332").unwrap()));
    assert!(!super::conn::is_local(&at("100.64.0.20:8332").unwrap()));
}

#[cfg(unix)]
#[test]
fn the_wallet_files_are_owner_only_and_a_loose_key_file_is_said() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("fb-ecash-perm-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("wallet")).unwrap();
    let r = Root::from_words(&[7u8; 32], Chain::Regtest).unwrap();
    let (main, bids) = (r.account(Chain::Regtest, Account::Main).unwrap(), r.account(Chain::Regtest, Account::Bids).unwrap());
    let fp = r.fingerprint().to_string();
    let (mn, bn) = wallet::names(&fp);
    let rec = wallet::Record {
        key_id: "k".into(),
        chain: "regtest".into(),
        fingerprint: fp,
        main_name: mn,
        bids_name: bn,
        main_xpub: main.public.to_record().0,
        bids_xpub: bids.public.to_record().0,
    };
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    wallet::write_record(&dir, &rec).unwrap();
    assert_eq!(mode(&wallet::record_path(&dir)), 0o600);
    bmm::save(&dir, &Bmm::default()).unwrap();
    assert_eq!(mode(&bmm::path(&dir)), 0o600);
    wallet::write_bids_key(&dir, &bids).unwrap();
    let key = wallet::bids_key_path(&dir);
    assert_eq!(mode(&key), 0o600);
    assert!(wallet::read_bids_key(&dir, &rec).is_ok());
    // Readable by others: made private again, and said once.
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(wallet::read_bids_key(&dir, &rec).err().unwrap().contains("made it private again"));
    assert_eq!(mode(&key), 0o600);
    assert!(wallet::read_bids_key(&dir, &rec).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_coins_value_is_checked_against_its_parent_transaction() {
    // Re-review M-A: a segwit v0 signature commits only to its own input's value, so a lie about a coin's value must
    // be caught from the coin's parent transaction, which its txid vouches for.
    use base64::{engine::general_purpose::STANDARD, Engine};
    let (main, bids) = keys(Chain::Regtest);
    let to = bids.public.script(0, 0).unwrap();
    let expect = Expect { to: to.clone(), sats: 300_000, fee: 1_410, max_fee: wallet::MAX_FEE, max_rate: 11 };
    let good = funded(&main, to.clone(), 300_000, 1_410, None);
    let mut p = Psbt::deserialize(&STANDARD.decode(&good).unwrap()).unwrap();
    // The value lied about (higher than the parent's output): refused.
    p.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(2_000_000);
    let lied = STANDARD.encode(p.serialize());
    assert!(sign::check(&lied, &main.public, &expect).is_err());
    // No parent transaction at all: refused.
    let mut p = Psbt::deserialize(&STANDARD.decode(&good).unwrap()).unwrap();
    p.inputs[0].non_witness_utxo = None;
    let bare = STANDARD.encode(p.serialize());
    assert!(sign::check(&bare, &main.public, &expect).unwrap_err().contains("history"));
    // Another parent than the one the input names: refused.
    let mut p = Psbt::deserialize(&STANDARD.decode(&good).unwrap()).unwrap();
    p.inputs[0].non_witness_utxo = Some(parent(TxOut { value: Amount::from_sat(1_000_000), script_pubkey: ScriptBuf::new() }));
    let other = STANDARD.encode(p.serialize());
    assert!(sign::check(&other, &main.public, &expect).unwrap_err().contains("wrongly"));
}

#[tokio::test]
async fn a_send_goes_out_signed_here_with_a_fee_ceiling() {
    let (main, bids) = keys(Chain::Regtest);
    let to_addr = bids.public.address(0, 0).unwrap();
    let psbt = funded(&main, bids.public.script(0, 0).unwrap(), 300_000, 1_410, None);
    // The node answers with the payment's own txid.
    let unsigned_txid = {
        use base64::{engine::general_purpose::STANDARD, Engine};
        Psbt::deserialize(&STANDARD.decode(&psbt).unwrap()).unwrap().unsigned_tx.compute_txid().to_string()
    };
    let answer = unsigned_txid.clone();
    let (c, calls) = stub::serve_paths(move |_, m, _| match m {
        "sendrawtransaction" => Ok(json!(answer)),
        _ => Err((-32601, "Method not found".into())),
    });
    let q = Quote { psbt: psbt.clone(), address: to_addr.clone(), sats: 300_000, fee: 1_410, rate: 10 };
    let txid = wallet::sign_and_send(&conn_for(&c), &q, &main, None).await.unwrap();
    assert_eq!(txid, unsigned_txid);
    // Nothing but the finished transaction: no walletpassphrase, no walletprocesspsbt.
    assert_eq!(methods(&calls), ["sendrawtransaction"]);
    let p = &params_of(&calls, "sendrawtransaction")[0];
    assert!(p[1].as_str().unwrap().parse::<f64>().unwrap() > 0.0, "a fee-rate ceiling, not 0");

    // Re-review M-B: the node takes the signed payment but answers with an error. It may have gone out: said so,
    // with its coins, so the caller keeps it counted and a retry must spend one of them.
    let (c, _) = stub::serve_paths(|_, m, _| match m {
        "sendrawtransaction" => Err((-26, "bad-txns".into())),
        _ => Err((-32601, "Method not found".into())),
    });
    let f = wallet::sign_and_send(&conn_for(&c), &q, &main, None).await.unwrap_err();
    assert!(f.sent && f.message.contains("may have gone out") && f.inputs.len() == 1, "{f:?}");
    // A retry that doesn't spend one of those coins is refused before signing.
    let other = bitcoin::OutPoint { txid: Txid::from_str(&h("cd")).unwrap(), vout: 0 };
    let f2 = wallet::sign_and_send(&conn_for(&c), &q, &main, Some(&[other])).await.unwrap_err();
    assert!(!f2.sent && f2.message.contains("may still go out"), "{f2:?}");
    // One that does goes on to the node.
    let f3 = wallet::sign_and_send(&conn_for(&c), &q, &main, Some(&f.inputs)).await.unwrap_err();
    assert!(f3.sent, "{f3:?}");
}

#[tokio::test]
async fn everything_means_the_balance_less_the_fee() {
    let (c, calls) = stub::serve_paths(|_, m, p| match m {
        "validateaddress" => Ok(json!({"isvalid": p[0] != "nonsense"})),
        "getbalances" => Ok(json!({"mine": {"trusted": 1.0, "untrusted_pending": 0.5, "immature": 0}})),
        "estimatesmartfee" => Ok(json!({"errors": ["Insufficient data or no feerate found"], "blocks": 6})),
        "walletcreatefundedpsbt" => Ok(json!({"psbt": "cHNidP8=", "fee": 0.0000141, "changepos": -1})),
        _ => Err((-32601, "Method not found".into())),
    });
    let conn = conn_for(&c);
    let q = wallet::quote(&conn, "w", "bcrt1qexample", None).await.unwrap();
    assert_eq!((q.sats, q.fee), (100_000_000 - 1410, 1410));
    let p = &params_of(&calls, "/wallet/w walletcreatefundedpsbt")[0];
    assert_eq!(p[1], json!([{"bcrt1qexample": "1.00000000"}]));
    assert_eq!(p[2], json!(499_999_999));
    assert_eq!(p[3], json!({"fee_rate": 2.0, "replaceable": true, "subtractFeeFromOutputs": [0]}));
    // With the key paths in, so the payment can be checked here.
    assert_eq!(p[4], json!(true));
    assert_eq!(wallet::quote(&conn, "w", "nonsense", Some(1)).await.unwrap_err(), "That isn't an eCash address.");
}

#[test]
fn a_phone_is_told_what_leaves_and_where() {
    use super::commands::approve_text;
    assert_eq!(approve_text(false, 50_000_000, "bc1qxyz"), "Send 0.50000000 ECX to bc1qxyz");
    assert_eq!(approve_text(true, 1_000_000, "bc1qbid"), "Move 0.01000000 ECX into the bidding wallet (bc1qbid)");
}

// ---- Bidding ----

#[test]
fn the_m8_carries_the_tag_the_slot_h_star_and_the_tip_reversed() {
    let critical = format!("aa{}", "00".repeat(31));
    let tip = format!("{}ff", "00".repeat(31));
    let d = bmm::m8_data(&critical, &tip).unwrap();
    assert_eq!(d, format!("00bf0082{}ff{}", critical, "00".repeat(31)));
    assert_eq!(d.len() / 2, 0x44);
    assert!(bmm::m8_data("zz", &tip).is_err());
}

fn no_keep(_: Vec<bmm::Round>) -> bmm::Kept {
    Box::pin(async { Ok(()) })
}

fn no_stop() -> bool {
    true
}

/// The eCash node for a bid: tip T, coins of the bidding wallet (one too small, one not its own), a change address;
/// it takes the bid (answering with its txid).
fn ecash_for_bid(tip: String, bids: &AccountKey) -> (crate::rpc::FreeBankClient, stub::Calls) {
    ecash_for_bid_sending(tip, bids, true)
}

/// As ecash_for_bid, the node answering the bid with its txid (`takes`) or an error.
fn ecash_for_bid_sending(tip: String, bids: &AccountKey, takes: bool) -> (crate::rpc::FreeBankClient, stub::Calls) {
    let p = bids.public.clone();
    let coin = |b: u32, i: u32, txid: &str, amount: f64| {
        json!({"txid": txid, "vout": 1, "amount": amount,
               "desc": format!("wpkh([{}/84h/1h/1h/{}/{}]{})#x", p.fingerprint, b, i, p.pubkey(b, i).unwrap()),
               "scriptPubKey": p.script(b, i).unwrap().to_hex_string()})
    };
    let mut foreign = coin(0, 9, &h("04"), 0.0005);
    foreign["scriptPubKey"] = json!(keys(Chain::Regtest).0.public.script(0, 9).unwrap().to_hex_string());
    let unspent = json!([coin(0, 1, &h("01"), 0.0000105), coin(0, 2, &h("02"), 0.001), coin(1, 0, &h("03"), 0.5), foreign]);
    let change = p.address(1, 4).unwrap();
    let change_desc = format!("wpkh([{}/84h/1h/1h/1/4]{})#x", p.fingerprint, p.pubkey(1, 4).unwrap());
    stub::serve_paths(move |_, m, a| match m {
        "getbestblockhash" => Ok(json!(tip)),
        "listunspent" => Ok(unspent.clone()),
        "getrawchangeaddress" => Ok(json!(change)),
        "getaddressinfo" if a[0] == change => Ok(json!({"desc": change_desc})),
        "sendrawtransaction" if takes => {
            let tx: Transaction = encode::deserialize_hex(a[0].as_str().unwrap()).unwrap();
            Ok(json!(tx.compute_txid().to_string()))
        }
        "sendrawtransaction" => Err((-26, "bad-txns".into())),
        _ => Err((-32601, "Method not found".into())),
    })
}

fn freebank_template(tip: String) -> (crate::rpc::FreeBankClient, stub::Calls) {
    stub::serve(move |m, _| match m {
        "get_block_template" => Ok(json!({
            "critical_hash": h("cc"),
            "block": {"prev_main_hash": tip, "height": 9, "hex": "00"},
            "fees_sats": 0
        })),
        _ => Err((-32601, "Method not found".into())),
    })
}

#[tokio::test]
async fn a_bid_is_built_and_signed_here_from_the_smallest_own_coin_that_covers_it() {
    let (_, bids) = keys(Chain::Regtest);
    let tip = h("0a");
    let (e, ecalls) = ecash_for_bid(tip.clone(), &bids);
    let (fb, _) = freebank_template(tip.clone());
    let conn = conn_for(&e);
    let mut b = Bmm { on: true, ..Bmm::default() };
    let bw = Bids { name: "bw", key: &bids };
    let did = bmm::tick(&mut b, &conn, &fb, &bw, 1_000_000, &no_stop, &no_keep).await.unwrap();
    let sent = &params_of(&ecalls, "sendrawtransaction")[0];
    let tx: Transaction = encode::deserialize_hex(sent[0].as_str().unwrap()).unwrap();
    assert_eq!(did, Did::Bid { txid: tx.compute_txid().to_string(), height: 9 });
    // The 0.001 coin: the 1,050-sat one can't keep its change, the 0.0005 one isn't its own, the 0.5 one is bigger.
    assert_eq!(tx.input[0].previous_output.txid.to_string(), h("02"));
    assert_eq!(tx.input[0].witness.len(), 2, "signed here");
    // The M8 first, the change (the checked change address) after, the bid the fee, the replay stamp.
    let m8 = bmm::m8_data(&h("cc"), &tip).unwrap();
    assert_eq!(tx.output[0].script_pubkey.to_hex_string(), format!("6a44{}", m8));
    assert_eq!(tx.output[1].script_pubkey, bids.public.script(1, 4).unwrap());
    assert_eq!(tx.output[1].value.to_sat(), 100_000 - 10_000);
    assert_eq!(tx.lock_time.to_consensus_u32(), 499_999_999);
    assert!(sent[1].as_str().unwrap().parse::<f64>().unwrap() > 0.0, "a fee-rate ceiling");
    assert!(!methods(&ecalls).iter().any(|m| m.contains("signrawtransactionwithwallet") || m.contains("createrawtransaction")));
    let r = &b.rounds[0];
    assert_eq!((r.outcome.as_str(), r.fee, r.coin.as_ref().unwrap().value), ("live", 10_000, 100_000));
    // The same tip again: no second bid.
    assert_eq!(bmm::tick(&mut b, &conn, &fb, &bw, 1_000_010, &no_stop, &no_keep).await.unwrap(), Did::Nothing);
    // Switched off just before it would go out: it doesn't.
    let (e2, ecalls2) = ecash_for_bid(h("0b"), &bids);
    let (fb2, _) = freebank_template(h("0b"));
    let mut b2 = Bmm { on: true, ..Bmm::default() };
    assert_eq!(bmm::tick(&mut b2, &conn_for(&e2), &fb2, &bw, 1_000_000, &|| false, &no_keep).await.unwrap(), Did::Nothing);
    assert!(params_of(&ecalls2, "sendrawtransaction").is_empty());
}

fn round(tip: &str, outcome: &str) -> Round {
    Round {
        main_tip: tip.into(),
        critical: h("cc"),
        height: 9,
        block: json!({"prev_main_hash": tip, "height": 9, "hex": "00"}),
        txid: h("ee"),
        fee: 10_000,
        at: 1_000_000,
        outcome: outcome.into(),
        main_block: None,
        tries: 0,
        misses: 0,
        last_tip: tip.into(),
        paid: false,
        freed: false,
        coin: Some(Coin { txid: h("0f"), vout: 1, value: 100_000, branch: 0, index: 2 }),
    }
}

#[tokio::test]
async fn the_cap_counts_every_paid_bid() {
    let (_, bids) = keys(Chain::Regtest);
    let tip = h("0a");
    let (e, _) = ecash_for_bid(tip.clone(), &bids);
    let (fb, fcalls) = freebank_template(tip.clone());
    let conn = conn_for(&e);
    let bw = Bids { name: "bw", key: &bids };
    // A "lost" bid that was paid (seen confirmed) counts, as won and live ones do (review M2).
    let mut paid = round(&h("09"), "lost");
    paid.paid = true;
    paid.freed = true;
    paid.fee = 495_000;
    paid.at = 1_000_000 - 3600;
    let mut b = Bmm { on: true, rounds: vec![paid], ..Bmm::default() };
    match bmm::tick(&mut b, &conn, &fb, &bw, 1_000_000, &no_stop, &no_keep).await.unwrap() {
        Did::Waiting(w) => assert!(w.contains("reached your cap of 0.00500000"), "{w}"),
        d => panic!("{d:?}"),
    }
    // Off: no template asked for.
    let n = fcalls.lock().unwrap().len();
    let mut b = Bmm { on: false, ..Bmm::default() };
    assert_eq!(bmm::tick(&mut b, &conn, &fb, &bw, 2_000_000, &no_stop, &no_keep).await.unwrap(), Did::Nothing);
    assert_eq!(fcalls.lock().unwrap().len(), n);
}

#[test]
fn the_cap_counts_the_bids_on_one_coin_once_at_the_largest_fee() {
    // Re-review L-A: every bid of the last day counts, whatever the node says of it, but of bids on one coin only one
    // can confirm.
    let coin = |t: &str| Some(Coin { txid: h(t), vout: 1, value: 100_000, branch: 0, index: 2 });
    let at = |r: &mut Round, at: u64, fee: u64, c: &str| {
        r.at = at;
        r.fee = fee;
        r.coin = coin(c);
    };
    let (mut a, mut a2, mut b2, mut old) = (round(&h("0a"), "lost"), round(&h("0b"), "live"), round(&h("0c"), "lost"), round(&h("09"), "won"));
    at(&mut a, 1_000_000, 10_000, "0f");
    at(&mut a2, 1_000_100, 10_190, "0f"); // a's replacement: one coin
    at(&mut b2, 1_000_200, 10_000, "0e"); // another coin
    at(&mut old, 1_000_000 - 86_400, 400_000, "0d"); // more than a day before
    let b = Bmm { rounds: vec![old, a, a2, b2], ..Bmm::default() };
    assert_eq!(b.spent_today(1_000_300), 10_190 + 10_000);
}

#[test]
fn rounds_of_the_last_day_are_never_pruned() {
    let mut b = Bmm::default();
    for i in 0..250u64 {
        let mut r = round(&h("0a"), "lost");
        r.at = 1_000_000 + i;
        b.rounds.push(r);
    }
    b.prune(1_000_300);
    assert_eq!(b.rounds.len(), 250, "all within the day");
    for r in b.rounds.iter_mut().take(240) {
        r.at -= 86_400; // 240 older than a day, 10 newer
    }
    b.prune(1_000_300);
    assert_eq!(b.rounds.len(), 200);
    assert_eq!(b.rounds.iter().filter(|r| r.at + 86_400 > 1_000_300).count(), 10);
}

#[tokio::test]
async fn a_bid_is_recorded_before_it_goes_out_and_counts_if_the_node_errs() {
    let (_, bids) = keys(Chain::Regtest);
    let tip = h("0a");
    let (fb, _) = freebank_template(tip.clone());
    let bw = Bids { name: "bw", key: &bids };
    // The node gives an error for the bid: it may have gone out, so it stays recorded, live and counted.
    let (e, ecalls) = ecash_for_bid_sending(tip.clone(), &bids, false);
    let mut b = Bmm { on: true, ..Bmm::default() };
    let err = bmm::tick(&mut b, &conn_for(&e), &fb, &bw, 1_000_000, &no_stop, &no_keep).await.unwrap_err();
    assert!(err.contains("may have gone out"), "{err}");
    assert_eq!(params_of(&ecalls, "sendrawtransaction").len(), 1);
    let sent: Transaction = encode::deserialize_hex(params_of(&ecalls, "sendrawtransaction")[0][0].as_str().unwrap()).unwrap();
    assert_eq!((b.rounds.len(), b.rounds[0].outcome.as_str()), (1, "live"));
    assert_eq!(b.rounds[0].txid, sent.compute_txid().to_string());
    assert_eq!(b.spent_today(1_000_000), 10_000);
    // The rounds can't be kept: nothing is sent, nothing recorded.
    let (e, ecalls) = ecash_for_bid(tip.clone(), &bids);
    let refuse = |_: Vec<Round>| -> bmm::Kept { Box::pin(async { Err("disk full".to_string()) }) };
    let mut b = Bmm { on: true, ..Bmm::default() };
    let err = bmm::tick(&mut b, &conn_for(&e), &fb, &bw, 1_000_000, &no_stop, &refuse).await.unwrap_err();
    assert!(err.contains("wasn't sent") && err.contains("disk full"), "{err}");
    assert!(params_of(&ecalls, "sendrawtransaction").is_empty());
    assert!(b.rounds.is_empty());
    // Kept: the keeper sees the new round before the node does.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let record = move |r: Vec<Round>| -> bmm::Kept {
        s2.lock().unwrap().push(r.len());
        Box::pin(async { Ok(()) })
    };
    let (e, _) = ecash_for_bid(tip.clone(), &bids);
    let mut b = Bmm { on: true, ..Bmm::default() };
    assert!(matches!(bmm::tick(&mut b, &conn_for(&e), &fb, &bw, 1_000_000, &no_stop, &record).await.unwrap(), Did::Bid { .. }));
    assert_eq!(*seen.lock().unwrap(), vec![1]);
}

#[tokio::test]
async fn a_won_round_is_connected_with_the_block_that_carried_it() {
    let (_, bids) = keys(Chain::Regtest);
    let (e, _) = stub::serve_paths(|_, m, _| match m {
        "getbestblockhash" => Ok(json!(h("0b"))),
        // Won and confirmed: nothing more to watch.
        "gettransaction" => Ok(json!({"confirmations": 1, "blockhash": h("0b")})),
        _ => Err((-32601, "Method not found".into())),
    });
    let (fb, fcalls) = stub::serve(|m, _| match m {
        "get_bmm_inclusions" => Ok(json!([h("0b")])),
        "connect_block" => Ok(json!(true)),
        _ => Err((-32601, "Method not found".into())),
    });
    let mut b = Bmm { on: false, rounds: vec![round(&h("0a"), "live")], ..Bmm::default() };
    bmm::tick(&mut b, &conn_for(&e), &fb, &Bids { name: "bw", key: &bids }, 1_000_600, &no_stop, &no_keep).await.unwrap();
    let r = &b.rounds[0];
    assert_eq!((r.outcome.as_str(), r.main_block.as_deref(), r.paid), ("won", Some(h("0b").as_str()), true));
    assert_eq!(r.block, Value::Null);
    assert_eq!(params_of(&fcalls, "connect_block")[0], json!([{"prev_main_hash": h("0a"), "height": 9, "hex": "00"}, h("0b")]));
}

#[tokio::test]
async fn no_inclusion_is_not_yet_until_three_new_tips() {
    let (_, bids) = keys(Chain::Regtest);
    let (fb, _) = stub::serve(|m, _| match m {
        "get_bmm_inclusions" => Ok(json!([])),
        _ => Err((-32601, "Method not found".into())),
    });
    let mut b = Bmm { on: false, rounds: vec![round(&h("0a"), "live")], ..Bmm::default() };
    for (n, tip) in ["0b", "0c", "0d"].iter().enumerate() {
        let t = h(tip);
        let (e, _) = stub::serve_paths(move |_, m, _| match m {
            "getbestblockhash" => Ok(json!(t)),
            "gettransaction" => Ok(json!({"confirmations": 0, "details": [{"abandoned": false}]})),
            "getmempoolentry" => Ok(json!({"vsize": 190})),
            _ => Err((-32601, "Method not found".into())),
        });
        let bw = Bids { name: "bw", key: &bids };
        bmm::tick(&mut b, &conn_for(&e), &fb, &bw, 1_000_600, &no_stop, &no_keep).await.unwrap();
        // The same tip again counts once.
        bmm::tick(&mut b, &conn_for(&e), &fb, &bw, 1_000_601, &no_stop, &no_keep).await.unwrap();
        let want = if n < 2 { "live" } else { "lost" };
        assert_eq!(b.rounds[0].outcome, want, "after tip {}", n + 1);
    }
    assert_eq!(b.spent_today(1_000_600), 10_000, "a lost bid counts all the same (re-review L-A)");
}

#[tokio::test]
async fn a_round_whose_height_the_freebank_chain_has_is_lost_at_once() {
    // The walk-through's second run: after a rival won block 9, bidding waited three eCash blocks.
    let (_, bids) = keys(Chain::Regtest);
    let (fb, _) = stub::serve(|m, _| match m {
        "get_bmm_inclusions" => Ok(json!([])),
        "getblockcount" => Ok(json!(9)),
        _ => Err((-32601, "Method not found".into())),
    });
    let (e, _) = stub::serve_paths(|_, m, _| match m {
        "getbestblockhash" => Ok(json!(h("0b"))),
        "gettransaction" => Ok(json!({"confirmations": 0, "details": [{"abandoned": false}]})),
        "getmempoolentry" => Ok(json!({"vsize": 190})),
        _ => Err((-32601, "Method not found".into())),
    });
    let mut b = Bmm { on: false, rounds: vec![round(&h("0a"), "live")], ..Bmm::default() };
    bmm::tick(&mut b, &conn_for(&e), &fb, &Bids { name: "bw", key: &bids }, 1_000_600, &no_stop, &no_keep).await.unwrap();
    assert_eq!(b.rounds[0].outcome, "lost");
}

#[tokio::test]
async fn a_lost_bid_that_confirms_after_all_is_paid_and_connected() {
    let (_, bids) = keys(Chain::Regtest);
    let (e, _) = stub::serve_paths(|_, m, _| match m {
        "getbestblockhash" => Ok(json!(h("0c"))),
        "gettransaction" => Ok(json!({"confirmations": 1, "blockhash": h("0b")})),
        _ => Err((-32601, "Method not found".into())),
    });
    let (fb, fcalls) = stub::serve(|m, _| match m {
        "connect_block" => Ok(json!(true)),
        _ => Err((-32601, "Method not found".into())),
    });
    let mut b = Bmm { on: false, rounds: vec![round(&h("0a"), "lost")], ..Bmm::default() };
    bmm::tick(&mut b, &conn_for(&e), &fb, &Bids { name: "bw", key: &bids }, 1_000_600, &no_stop, &no_keep).await.unwrap();
    let r = &b.rounds[0];
    assert_eq!((r.outcome.as_str(), r.paid, r.freed), ("won", true, true));
    assert_eq!(params_of(&fcalls, "connect_block")[0][1], json!(h("0b")));
    assert_eq!(b.spent_today(1_000_600), 10_000);
}

#[tokio::test]
async fn a_lost_bid_out_of_the_mempool_is_abandoned_so_its_coin_bids_again() {
    let (_, bids) = keys(Chain::Regtest);
    let (e, ecalls) = stub::serve_paths(|_, m, _| match m {
        "getbestblockhash" => Ok(json!(h("0b"))),
        "getmempoolentry" => Err((-5, "Transaction not in mempool".into())),
        "gettransaction" => Ok(json!({"confirmations": 0, "details": [{"abandoned": false}]})),
        "abandontransaction" => Ok(Value::Null),
        _ => Err((-32601, "Method not found".into())),
    });
    let (fb, _) = stub::serve(|_, _| Err((-32601, "Method not found".into())));
    let mut b = Bmm { on: false, rounds: vec![round(&h("0a"), "lost")], ..Bmm::default() };
    bmm::tick(&mut b, &conn_for(&e), &fb, &Bids { name: "bw", key: &bids }, 1_000_600, &no_stop, &no_keep).await.unwrap();
    assert!(b.rounds[0].freed);
    assert_eq!(params_of(&ecalls, "/wallet/bw abandontransaction")[0], json!([h("ee")]));
}

#[tokio::test]
async fn a_lost_bid_still_held_by_the_ecash_node_is_replaced_from_its_recorded_coin() {
    let (_, bids) = keys(Chain::Regtest);
    let tip = h("0b");
    let p = bids.public.clone();
    let change = p.address(1, 4).unwrap();
    let change_desc = format!("wpkh([{}/84h/1h/1h/1/4]{})#x", p.fingerprint, p.pubkey(1, 4).unwrap());
    let t = tip.clone();
    let (e, ecalls) = stub::serve_paths(move |_, m, a| match m {
        "getbestblockhash" => Ok(json!(t)),
        // The lost bid: unconfirmed, still in the mempool.
        "gettransaction" => Ok(json!({"confirmations": 0, "details": []})),
        "getmempoolentry" => Ok(json!({"fees": {"base": 0.0002}, "vsize": 190})),
        "getrawchangeaddress" => Ok(json!(change)),
        "getaddressinfo" if a[0] == change => Ok(json!({"desc": change_desc})),
        "sendrawtransaction" => {
            let tx: Transaction = encode::deserialize_hex(a[0].as_str().unwrap()).unwrap();
            Ok(json!(tx.compute_txid().to_string()))
        }
        _ => Err((-32601, "Method not found".into())),
    });
    let (fb, _) = freebank_template(tip.clone());
    let mut lost = round(&h("09"), "lost");
    lost.fee = 20_000;
    let mut b = Bmm { on: true, bid: 20_000, rounds: vec![lost], ..Bmm::default() };
    bmm::tick(&mut b, &conn_for(&e), &fb, &Bids { name: "bw", key: &bids }, 1_000_000, &no_stop, &no_keep).await.unwrap();
    let tx: Transaction = encode::deserialize_hex(params_of(&ecalls, "sendrawtransaction")[0][0].as_str().unwrap()).unwrap();
    // The recorded coin (not the node's figures), the fee 20,000 + 190 + 1 (BIP125), the change what's left.
    assert_eq!(tx.input[0].previous_output.txid.to_string(), h("0f"));
    assert_eq!(tx.output[1].value.to_sat(), 100_000 - 20_191);
    assert_eq!((b.rounds[0].outcome.as_str(), b.rounds[0].freed), ("replaced", true));
    assert_eq!((b.rounds[1].outcome.as_str(), b.rounds[1].fee), ("live", 20_191));
    // One coin: the two count once, at the larger fee.
    assert_eq!(b.spent_today(1_000_000), 20_191);
}

// ---- Real nodes (ignored) ----

/// Against a real eCash node on regtest (FB_ECASH_RPC=host:port, FB_ECASH_DIR=the folder with its .cookie): set up both
/// watch-only wallets, mine to the main one, move eCash into the bidding wallet (signed here) and back (signed with the
/// bidding key), and the node takes both.
#[tokio::test]
#[ignore]
async fn ecash_real_node() {
    let (Ok(at), Ok(dir)) = (std::env::var("FB_ECASH_RPC"), std::env::var("FB_ECASH_DIR")) else {
        panic!("set FB_ECASH_RPC and FB_ECASH_DIR");
    };
    let s = crate::node::Settings { rest: at, l1_datadir: Some(dir), ..Default::default() };
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let conn = super::conn::connect(&http, &s, None).await.unwrap();
    assert_eq!(conn.chain, Chain::Regtest);
    let (main, bids) = keys(Chain::Regtest);
    let fp = main.public.fingerprint.to_string();
    let (mname, bname) = wallet::names(&fp);
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    wallet::create(&conn, &bname, &bids.public).await.unwrap();
    // Again: watch-only and the account's, so used as they are.
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    let info = conn.wallet(&mname).call_typed("getwalletinfo", vec![]).await.unwrap();
    assert_eq!(info["private_keys_enabled"], json!(false), "{info}");

    let mine_to = |a: String, n: u64| {
        let node = conn.node();
        async move { node.call_typed("generatetoaddress", vec![json!(n), json!(a)]).await.unwrap() }
    };
    let main_addr = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    mine_to(main_addr.clone(), 101).await;
    assert!(wallet::balance(&conn, &mname).await.unwrap().trusted >= 50 * 100_000_000);

    let to = wallet::new_address(&conn, &bname, &bids.public).await.unwrap();
    let q = wallet::quote(&conn, &mname, &to, Some(100_000_000)).await.unwrap();
    // Core's walletcreatefundedpsbt gives each coin's parent transaction (re-review M-A needs it).
    {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let p = Psbt::deserialize(&STANDARD.decode(&q.psbt).unwrap()).unwrap();
        assert!(p.inputs.iter().all(|i| i.non_witness_utxo.is_some()), "non_witness_utxo on every input");
    }
    let txid = wallet::sign_and_send(&conn, &q, &main, None).await.unwrap();
    let tx = conn.node().call_typed("getrawtransaction", vec![json!(txid), json!(true)]).await.unwrap();
    assert_eq!(tx["locktime"], json!(499_999_999));
    mine_to(main_addr.clone(), 1).await;
    assert_eq!(wallet::balance(&conn, &bname).await.unwrap().trusted, 100_000_000);

    let back = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    let q = wallet::quote(&conn, &bname, &back, None).await.unwrap();
    assert_eq!(q.sats + q.fee, 100_000_000);
    wallet::sign_and_send(&conn, &q, &bids, None).await.unwrap();
    mine_to(main_addr, 1).await;
    assert_eq!(wallet::balance(&conn, &bname).await.unwrap().trusted, 0);
    let r = wallet::Record {
        key_id: "k".into(),
        chain: "regtest".into(),
        fingerprint: fp,
        main_name: mname,
        bids_name: bname,
        main_xpub: main.public.to_record().0,
        bids_xpub: bids.public.to_record().0,
    };
    let h = wallet::history(&conn, &r).await.unwrap();
    assert!(h.iter().any(|t| t.wallet == "bids" && t.category == "receive" && t.sats == 100_000_000), "{h:?}");
}

/// Bidding end to end on freebankd's standing stack with a second node as rival (FB_BMM_L1, FB_BMM_FB, FB_BMM_RIVAL,
/// FB_BMM_ENFORCER, FB_GRPCURL; the stack's test login t/t): won; lost to the rival, replaced; a later bid won.
#[tokio::test]
#[ignore]
async fn bmm_real_stack() {
    let get = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (l1, fb_at, rival_at, enf, grpcurl) =
        (get("FB_BMM_L1"), get("FB_BMM_FB"), get("FB_BMM_RIVAL"), get("FB_BMM_ENFORCER"), get("FB_GRPCURL"));
    let dir = std::env::temp_dir().join(format!("fb-bmm-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bitcoin.conf"), "rpcuser=t\nrpcpassword=t\n").unwrap();
    let s = crate::node::Settings { rest: l1, l1_datadir: Some(dir.to_string_lossy().into_owned()), ..Default::default() };
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let conn = super::conn::connect(&http, &s, None).await.unwrap();
    let r = Root::from_words(&[9u8; 32], Chain::Regtest).unwrap();
    let (main, bids) = (r.account(Chain::Regtest, Account::Main).unwrap(), r.account(Chain::Regtest, Account::Bids).unwrap());
    let (mname, bname) = wallet::names(&r.fingerprint().to_string());
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    wallet::create(&conn, &bname, &bids.public).await.unwrap();
    let mut fb = crate::rpc::FreeBankClient::with_http(http.clone()).with_timeout(std::time::Duration::from_secs(200));
    fb.configure(&format!("http://{}", fb_at), "t", "t");
    let mut rival = crate::rpc::FreeBankClient::with_http(http.clone());
    rival.configure(&format!("http://{}", rival_at), "t", "t");
    let l1_mine = |n: u64, to: String| {
        let (enf, grpcurl) = (enf.clone(), grpcurl.clone());
        async move {
            let out = std::process::Command::new(&grpcurl)
                .args(["-plaintext", "-d", &format!("{{\"blocks\":{},\"address\":\"{}\"}}", n, to), &enf])
                .arg("cusf.mainchain.v1.MiningService/GenerateToAddress")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    };
    let fb_height = || {
        let fb = &fb;
        async move { fb.call_typed("getblockcount", vec![]).await.unwrap().as_u64().unwrap() }
    };
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let bw = Bids { name: &bname, key: &bids };

    // Ten mature coins in the bidding wallet.
    let bids_addr = wallet::new_address(&conn, &bname, &bids.public).await.unwrap();
    let main_addr = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    l1_mine(110, bids_addr).await;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let h0 = fb_height().await;
    let mut b = Bmm { on: true, bid: 20_000, daily_cap: 1_000_000, ..Bmm::default() };

    // Round 1: the bid goes out (signed here) and the eCash node takes it; a block carries it; won.
    let mut did = Did::Nothing;
    for _ in 0..20 {
        did = bmm::tick(&mut b, &conn, &fb, &bw, now(), &no_stop, &no_keep).await.unwrap();
        if matches!(did, Did::Bid { .. }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    let Did::Bid { txid, .. } = did.clone() else { panic!("no bid: {did:?}") };
    let pooled = conn.node().call_typed("getmempoolentry", vec![json!(txid)]).await.unwrap();
    assert_eq!(pooled["fees"]["base"], json!(0.0002), "the whole bid is the fee");
    l1_mine(1, main_addr.clone()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    for _ in 0..20 {
        bmm::tick(&mut b, &conn, &fb, &bw, now(), &no_stop, &no_keep).await.unwrap();
        if b.rounds[0].outcome != "live" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    assert_eq!(b.rounds[0].outcome, "won", "{:?}", b.rounds[0]);
    assert_eq!(fb_height().await, h0 + 1);

    // Round 2 against a rival's 0.001 bid for its own block: ours isn't included. It stays live ("not yet") for three
    // tips; the rival connects its block meanwhile. Then it's lost, a later bid replaces it, and that one wins.
    let r2 = b.rounds.iter().position(|r| r.outcome == "live").expect("a second bid on the new tip");
    let rb = rival.call_typed("refreshbmm", vec![json!(0.001)]).await.unwrap();
    assert_ne!(rb["txid"].as_str().unwrap_or(""), "", "the rival bid: {rb}");
    l1_mine(1, main_addr.clone()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let _ = rival.call_typed("refreshbmm", vec![json!(0.001), json!(false)]).await;
    for _ in 0..20 {
        if fb_height().await == h0 + 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    for _ in 0..16 {
        bmm::tick(&mut b, &conn, &fb, &bw, now(), &no_stop, &no_keep).await.unwrap();
        if b.rounds[r2].outcome == "replaced" && b.rounds.iter().filter(|r| r.outcome == "won").count() >= 2 {
            break;
        }
        // Each pass with a live bid gets a block, so tips move on.
        if b.rounds.last().is_some_and(|r| r.outcome == "live") {
            l1_mine(1, main_addr.clone()).await;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }
    eprintln!("rounds: {:?}", b.rounds.iter().map(|r| (r.height, r.outcome.clone(), r.paid, r.freed)).collect::<Vec<_>>());
    assert!(matches!(b.rounds[r2].outcome.as_str(), "replaced" | "lost"), "{:?}", b.rounds[r2]);
    assert!(b.rounds.iter().filter(|r| r.outcome == "won").count() >= 2, "a later bid won too");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Deposit at par end to end on freebankd's standing stack (FB_IO_L1, FB_IO_FB, FB_IO_ENFORCER, FB_GRPCURL,
/// FB_IO_BMM: a command that makes one FreeBank block; the stack's test login t/t; scripts/inout-real-chain.sh sets it
/// up): the app's main eCash wallet funded, a deposit built and signed here, the treasury grown by it, FreeBank credits
/// the address; then a second deposit on the new treasury output.
#[tokio::test]
#[ignore]
async fn deposit_real_stack() {
    use super::deposit;
    let get = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (l1, fb_at, enf, grpcurl, bmm) = (get("FB_IO_L1"), get("FB_IO_FB"), get("FB_IO_ENFORCER"), get("FB_GRPCURL"), get("FB_IO_BMM"));
    let dir = std::env::temp_dir().join(format!("fb-deposit-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bitcoin.conf"), "rpcuser=t\nrpcpassword=t\n").unwrap();
    let s = crate::node::Settings { rest: l1, l1_datadir: Some(dir.to_string_lossy().into_owned()), ..Default::default() };
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let conn = super::conn::connect(&http, &s, None).await.unwrap();
    let r = Root::from_words(&[7u8; 32], Chain::Regtest).unwrap();
    let main = r.account(Chain::Regtest, Account::Main).unwrap();
    let (mname, _) = wallet::names(&r.fingerprint().to_string());
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    let mut fb = crate::rpc::FreeBankClient::with_http(http.clone()).with_timeout(std::time::Duration::from_secs(200));
    fb.configure(&format!("http://{}", fb_at), "t", "t");
    let l1_mine = |n: u64, to: String| {
        let (enf, grpcurl) = (enf.clone(), grpcurl.clone());
        async move {
            let out = std::process::Command::new(&grpcurl)
                .args(["-plaintext", "-d", &format!("{{\"blocks\":{},\"address\":\"{}\"}}", n, to), &enf])
                .arg("cusf.mainchain.v1.MiningService/GenerateToAddress")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    };
    let fb_block = || {
        let out = std::process::Command::new(&bmm).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    };
    let credited = |addr: String, sats: u64| {
        let fb = &fb;
        async move { deposit::credited_in(&fb.call_typed("listtransactions", vec![json!("*"), json!(1000)]).await.unwrap(), &addr, sats) }
    };

    // Coins for the main wallet: mined to it, then matured.
    let to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    l1_mine(101, to).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let coins = deposit::coins(&conn, &mname, &main.public).await.unwrap();
    assert!(!coins.is_empty(), "the mined coins are the wallet's");

    for (round, sats) in [(1, 150_000_000u64), (2, 25_000_000)] {
        let ctip = deposit::treasury(&http, &enf, &conn).await.unwrap();
        let answer = fb.call_typed("getdepositaddress", vec![]).await.unwrap();
        let address = deposit::plain_deposit_address(answer.as_str().unwrap()).unwrap();
        let coins = deposit::coins(&conn, &mname, &main.public).await.unwrap();
        let (change_addr, _) = wallet::change_address(&conn, &mname, &main.public).await.unwrap();
        let change = conn.script_of(&change_addr).unwrap();
        let built = deposit::build(&ctip, &address, sats, &coins, 2, &change).unwrap();
        let txid = built.tx.compute_txid().to_string();
        let hex = sign::sign(built.tx.clone(), &built.spends, &main).unwrap();
        let ceiling = sign::max_fee_rate(&hex, built.fee).unwrap();
        let sent = conn.node().call_typed("sendrawtransaction", vec![json!(hex), json!(ceiling)]).await;
        assert_eq!(sent.unwrap().as_str(), Some(txid.as_str()), "round {round}: the eCash node took the deposit");
        // While it waits, the treasury output is spent in the mempool: no second deposit on it.
        let busy = deposit::treasury(&http, &enf, &conn).await.unwrap_err();
        assert!(busy.contains("already waiting"), "{busy}");
        // FreeBank blocks until it is credited.
        let mut done = false;
        for _ in 0..8 {
            fb_block();
            done = credited(address.clone(), sats).await;
            if done {
                break;
            }
        }
        assert!(done, "round {round}: FreeBank credited the deposit at par, less its fee");
        let after = deposit::treasury(&http, &enf, &conn).await.unwrap();
        assert_eq!(after.value, ctip.value + sats, "round {round}: the treasury grew by the deposit");
        assert_eq!(after.outpoint.txid.to_string(), txid);
        eprintln!("round {round}: {} ECX deposited, fee {} sats, treasury now {}", super::to_coins(sats), built.fee, after.value);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Withdraw at par on freebankd's standing stack (as deposit_real_stack): to a fresh address of the app's eCash wallet,
/// with createwithdrawal's arguments as the app gives them; it waits for a bundle ("Unspent"); cancelled, it is
/// refunded in the next FreeBank blocks.
#[tokio::test]
#[ignore]
async fn withdraw_real_stack() {
    let get = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (l1, fb_at, bmm) = (get("FB_IO_L1"), get("FB_IO_FB"), get("FB_IO_BMM"));
    let dir = std::env::temp_dir().join(format!("fb-withdraw-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bitcoin.conf"), "rpcuser=t\nrpcpassword=t\n").unwrap();
    let s = crate::node::Settings { rest: l1, l1_datadir: Some(dir.to_string_lossy().into_owned()), ..Default::default() };
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let conn = super::conn::connect(&http, &s, None).await.unwrap();
    let r = Root::from_words(&[8u8; 32], Chain::Regtest).unwrap();
    let main = r.account(Chain::Regtest, Account::Main).unwrap();
    let (mname, _) = wallet::names(&r.fingerprint().to_string());
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    let mut fb = crate::rpc::FreeBankClient::with_http(http.clone()).with_timeout(std::time::Duration::from_secs(200));
    fb.configure(&format!("http://{}", fb_at), "t", "t");
    let fb_block = || {
        let out = std::process::Command::new(&bmm).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    };
    let status = |id: String| {
        let fb = &fb;
        async move { fb.call_typed("getwithdrawal", vec![json!(id)]).await.unwrap()["status"].as_str().unwrap().to_string() }
    };

    let to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    let refund = fb.call_typed("getnewaddress", vec![json!(""), json!("legacy")]).await.unwrap().as_str().unwrap().to_string();
    let v = fb.call_typed("createwithdrawal", crate::withdraw::create_args(&to, &refund, 50_000_000, crate::withdraw::FREEBANK_FEE, 10_000)).await.unwrap();
    let id = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["destination"].as_str(), Some(to.as_str()), "freebankd takes the eCash wallet's bech32 address as it is");
    fb_block();
    assert_eq!(status(id.clone()).await, "Unspent");
    assert_eq!(crate::withdraw::state_of(Some("Unspent"), None, None), "waiting");
    let mine = fb.call_typed("listmywithdrawals", vec![]).await.unwrap();
    assert!(mine.as_array().unwrap().iter().any(|x| x["id"].as_str() == Some(id.as_str())));
    // What left the wallet: the amount and the eCash fee, plus the FreeBank fee.
    let t = fb.call_typed("gettransaction", vec![v["txid"].clone()]).await.unwrap();
    assert_eq!(super::sats_of(&json!(-t["amount"].as_f64().unwrap())).unwrap(), 50_010_000, "{t}");
    assert_eq!(super::sats_of(&json!(-t["fee"].as_f64().unwrap())).unwrap(), crate::withdraw::FREEBANK_FEE, "{t}");

    // Cancelled while it waits: refunded.
    fb.call_typed("createwithdrawalrefundrequest", vec![json!(id)]).await.unwrap();
    let mut refunded = false;
    for _ in 0..6 {
        fb_block();
        if status(id.clone()).await == "Spent" {
            refunded = true;
            break;
        }
    }
    assert!(refunded, "the cancel took");
    assert_eq!(crate::withdraw::state_of(Some("Spent"), Some(true), None), "refunded");
    // The refund: a coinbase output to the refund address, of the amount and the eCash fee.
    let list = fb.call_typed("listtransactions", vec![json!("*"), json!(1000)]).await.unwrap();
    assert!(super::deposit::credited_in(&list, &refund, 50_010_000), "the amount and the eCash fee came back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The money changer end to end (scripts/changer-real-chain.sh: the stack, then the changer bot with wallets of its
/// own; FB_CHANGER_URL, FB_CHANGER_KEY besides deposit_real_stack's): sell 1 sECX for ECX, buy sECX with 0.5 ECX, and
/// a pay-in after the quote expired refunded. Every quote checked as the app checks it.
#[tokio::test]
#[ignore]
async fn changer_real_stack() {
    use crate::changer::{check, Asked, Quote};
    let get = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let (l1, fb_at, enf, grpcurl, bmm) = (get("FB_IO_L1"), get("FB_IO_FB"), get("FB_IO_ENFORCER"), get("FB_GRPCURL"), get("FB_IO_BMM"));
    let (url, key) = (get("FB_CHANGER_URL"), get("FB_CHANGER_KEY"));
    let dir = std::env::temp_dir().join(format!("fb-changer-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bitcoin.conf"), "rpcuser=t\nrpcpassword=t\n").unwrap();
    let s = crate::node::Settings { rest: l1, l1_datadir: Some(dir.to_string_lossy().into_owned()), ..Default::default() };
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let conn = super::conn::connect(&http, &s, None).await.unwrap();
    let r = Root::from_words(&[6u8; 32], Chain::Regtest).unwrap();
    let main = r.account(Chain::Regtest, Account::Main).unwrap();
    let (mname, _) = wallet::names(&r.fingerprint().to_string());
    wallet::create(&conn, &mname, &main.public).await.unwrap();
    // The user's FreeBank wallet: the stack's first node (the changer has the second).
    let mut fb = crate::rpc::FreeBankClient::with_http(http.clone()).with_timeout(std::time::Duration::from_secs(200));
    fb.configure(&format!("http://{}", fb_at), "t", "t");
    let l1_mine = |n: u64, to: String| {
        let (enf, grpcurl) = (enf.clone(), grpcurl.clone());
        async move {
            let out = std::process::Command::new(&grpcurl)
                .args(["-plaintext", "-d", &format!("{{\"blocks\":{},\"address\":\"{}\"}}", n, to), &enf])
                .arg("cusf.mainchain.v1.MiningService/GenerateToAddress")
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
    };
    let fb_block = || {
        let out = std::process::Command::new(&bmm).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    };
    let fb_call = |m: &'static str, p: Vec<Value>| {
        let fb = &fb;
        async move { fb.call_typed(m, p).await.unwrap() }
    };
    let ask = |side: &'static str, amount: u64, payout_to: String, refund_to: String| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            let v: Value = http
                .post(format!("{url}/v1/quote"))
                .json(&json!({"side": side, "amount": amount, "payout_to": payout_to, "refund_to": refund_to}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            serde_json::from_value::<Quote>(v.clone()).unwrap_or_else(|_| panic!("a quote: {v}"))
        }
    };
    let order_state = |id: String| {
        let (http, url) = (http.clone(), url.clone());
        async move { http.get(format!("{url}/v1/order/{id}")).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let genesis = fb_call("getblockhash", vec![json!(0)]).await.as_str().unwrap().to_string();

    // eCash for the app's wallet. 101 eCash blocks with no FreeBank block look like a BMM stall to the changer (it
    // stops quoting): a FreeBank block after them, and a pass of the changer's, as on a live chain.
    let to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    l1_mine(101, to).await;
    fb_block();
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // OUT: sell 1 sECX for ECX.
    let payout_to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    let refund_to = fb_call("getnewaddress", vec![json!(""), json!("legacy")]).await.as_str().unwrap().to_string();
    let q = ask("out", 100_000_000, payout_to.clone(), refund_to.clone()).await;
    let height = fb_call("getblockcount", vec![]).await.as_u64().unwrap();
    check(&q, &key, &Asked { side: "out", amount: 100_000_000, payout_to: &payout_to, refund_to: &refund_to, genesis: &genesis, height })
        .unwrap();
    assert_eq!(q.discount_bps, 100, "the beta default: 1% out");
    let v = fb_call("validateaddress", vec![json!(q.pay_in)]).await;
    assert!(v["isvalid"].as_bool().unwrap() && !v["ismine"].as_bool().unwrap_or(false));
    fb_call("sendtoaddress", vec![json!(q.pay_in), json!(super::to_coins(q.amount)), json!(format!("fb-changer {}", q.id))]).await;
    let mut paid = Value::Null;
    for _ in 0..15 {
        fb_block();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        paid = order_state(q.id.clone()).await;
        if paid["state"] == "paid" {
            break;
        }
    }
    assert_eq!(paid["state"], "paid", "the changer paid out: {paid}");
    l1_mine(1, payout_to.clone()).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let got = super::sats_of(&conn.wallet(&mname).call_typed("getreceivedbyaddress", vec![json!(payout_to), json!(1)]).await.unwrap()).unwrap();
    assert_eq!(got, q.payout, "the eCash payout arrived");
    eprintln!("out: sold 1 sECX, got {} ECX ({} bps, fee {})", super::to_coins(got), q.discount_bps, q.fee);

    // IN: buy sECX with 0.5 ECX.
    let payout_to = fb_call("getnewaddress", vec![json!(""), json!("legacy")]).await.as_str().unwrap().to_string();
    let refund_to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    let q = ask("in", 50_000_000, payout_to.clone(), refund_to.clone()).await;
    let height = conn.node().call_typed("getblockcount", vec![]).await.unwrap().as_u64().unwrap();
    check(&q, &key, &Asked { side: "in", amount: 50_000_000, payout_to: &payout_to, refund_to: &refund_to, genesis: &genesis, height })
        .unwrap();
    let ec_quote = wallet::quote(&conn, &mname, &q.pay_in, Some(q.amount)).await.unwrap();
    wallet::sign_and_send(&conn, &ec_quote, &main, None).await.unwrap();
    let mut paid = Value::Null;
    for _ in 0..15 {
        l1_mine(1, refund_to.clone()).await;
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        paid = order_state(q.id.clone()).await;
        if paid["state"] == "paid" {
            break;
        }
    }
    assert_eq!(paid["state"], "paid", "the changer paid out: {paid}");
    // The payout comes from the changer's node: blocks until it is in one.
    let mut got = 0;
    for _ in 0..6 {
        fb_block();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        got = super::sats_of(&fb_call("getreceivedbyaddress", vec![json!(payout_to), json!(1)]).await).unwrap();
        if got > 0 {
            break;
        }
    }
    assert_eq!(got, q.payout, "the FreeBank payout arrived");
    eprintln!("in: paid 0.5 ECX, got {} sECX ({} bps, fee {})", super::to_coins(got), q.discount_bps, q.fee);

    // LATE: a pay-in after the quote expired is refunded, less the refund's fee.
    let payout_to = wallet::new_address(&conn, &mname, &main.public).await.unwrap();
    let refund_to = fb_call("getnewaddress", vec![json!(""), json!("legacy")]).await.as_str().unwrap().to_string();
    let q = ask("out", 20_000_000, payout_to, refund_to.clone()).await;
    for _ in 0..7 {
        fb_block();
    }
    fb_call("sendtoaddress", vec![json!(q.pay_in), json!(super::to_coins(q.amount))]).await;
    let mut done = Value::Null;
    for _ in 0..15 {
        fb_block();
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        done = order_state(q.id.clone()).await;
        if done["state"] == "refunded" {
            break;
        }
    }
    assert_eq!(done["state"], "refunded", "a late pay-in is refunded: {done}");
    // The refund comes from the changer's node: blocks until it is in one.
    let mut back = 0;
    for _ in 0..6 {
        fb_block();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        back = super::sats_of(&fb_call("getreceivedbyaddress", vec![json!(refund_to), json!(1)]).await).unwrap();
        if back > 0 {
            break;
        }
    }
    assert_eq!(back, q.amount - 20_000, "all of it back, less the refund's fee");
    eprintln!("late: refunded {}", super::to_coins(back));
    let _ = std::fs::remove_dir_all(&dir);
}
