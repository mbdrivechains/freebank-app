//! The wallet flows against a real freebankd. Ignored unless asked for:
//!
//!   FB_TEST_FREEBANKD=<path to freebankd> [FB_TEST_DIR=<scratch folder>] \
//!     cargo test -j 2 recovery::real_node -- --ignored --test-threads=1 --nocapture
//!
//! Each test starts its own nodes on fresh data folders under FB_TEST_DIR (default: the system's temp
//! folder): main network (the app's WIF prefix), no peers, no eCash (the jsonrpc transport, pointed
//! nowhere), RPC on 127.0.0.1 at free ports. It stops them, by their own process, at the end.
//! Passphrases are random, kept in memory and never printed.

use super::job::{self, encrypt, rescan, set_seed, unlock, Job, Report};
use crate::wallet::RelockGuard;
use super::ops::{self, open_saved};
use super::*;
use crate::seed::Chain;
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroizing;

fn freebankd() -> PathBuf {
    PathBuf::from(std::env::var("FB_TEST_FREEBANKD").expect("set FB_TEST_FREEBANKD to a freebankd binary"))
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("FB_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("{}-{}-{}", name, std::process::id(), now()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn free_port() -> u16 {
    // Two free ports in a row: RPC, and the P2P port above it (never listened on, but the app checks it).
    loop {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        if p < 65_000 && std::net::TcpListener::bind(("127.0.0.1", p + 1)).is_ok() {
            return p;
        }
    }
}

fn random_pass() -> Zeroizing<String> {
    Zeroizing::new(format!("test-{:032x}", rand::random::<u128>()))
}

/// The standalone node's own arguments.
const STANDALONE: &[&str] = &[
    "-server=1",
    "-listen=0",
    "-connect=0",
    "-dnsseed=0",
    "-discover=0",
    "-upnp=0",
    "-rpcbind=127.0.0.1",
    "-rpcallowip=127.0.0.1",
    "-mainchaintransport=jsonrpc",
];

struct Node {
    child: Option<Child>,
    datadir: PathBuf,
    port: u16,
}

impl Node {
    fn start(datadir: &Path, port: u16) -> Node {
        std::fs::create_dir_all(datadir).unwrap();
        let child = Command::new(freebankd())
            .arg(format!("-datadir={}", datadir.display()))
            .arg(format!("-rpcport={}", port))
            .arg(format!("-port={}", port + 1))
            .args(STANDALONE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start freebankd");
        Node { child: Some(child), datadir: datadir.to_path_buf(), port }
    }

    fn restart(&mut self) {
        assert!(self.child.as_mut().unwrap().try_wait().unwrap().is_some(), "still running");
        *self = Node::start(&self.datadir.clone(), self.port);
    }

    /// A client for it, once it answers with its wallet.
    async fn client(&self) -> FreeBankClient {
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut c = FreeBankClient::with_http(http).with_timeout(LONG);
            if c.configure_local(&format!("http://127.0.0.1:{}", self.port), self.datadir.clone())
                && c.call_fresh("getwalletinfo", vec![]).await.is_ok()
            {
                return c;
            }
            assert!(Instant::now() < until, "the node didn't come up");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Wait for it to exit by itself (after encryptwallet).
    async fn exited(&mut self) {
        let until = Instant::now() + Duration::from_secs(180);
        while self.child.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "the node didn't stop after encryptwallet");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn stop(mut self) {
        let mut c = self.client().await;
        let _ = c.call_fresh("stop", vec![]).await;
        self.exited().await;
        self.child = None;
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // A test that failed half way: ask our node to shut down (SIGTERM), never harder.
        if let Some(c) = self.child.as_mut() {
            if c.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
                }
                let _ = c.wait();
            }
        }
    }
}

async fn call(c: &mut FreeBankClient, method: &str, params: Vec<Value>) -> Value {
    c.call_ui(method, params).await.unwrap_or_else(|e| panic!("{} failed: {}", method, e))
}

/// Encrypt a new wallet, wait for the node to stop by itself, start it again: passphrase first.
async fn passphrase_first(node: &mut Node, pass: &str) -> FreeBankClient {
    let mut c = node.client().await;
    assert!(call(&mut c, "getwalletinfo", vec![]).await.get("unlocked_until").is_none(), "a new wallet has no passphrase");
    encrypt(&mut c, pass).await.unwrap();
    node.exited().await;
    node.restart();
    let mut c = node.client().await;
    assert_eq!(call(&mut c, "getwalletinfo", vec![]).await["unlocked_until"], json!(0), "encrypted and locked");
    c
}

#[tokio::test]
#[ignore]
async fn real_node_words_to_wallet_and_back() {
    let dir = scratch("fbwallet-words");
    let (pass_a, pass_b, pass_new) = (random_pass(), random_pass(), random_pass());
    // The app's one relock guard: every unlock below goes through it.
    let guard = RelockGuard::default();

    // Node A: a new wallet, passphrase first, then the words' HD seed.
    let mut a = Node::start(&dir.join("a"), free_port());
    let mut c = passphrase_first(&mut a, &pass_a).await;
    let chain = Chain::from_name(call(&mut c, "getblockchaininfo", vec![]).await["chain"].as_str().unwrap()).unwrap();
    assert_eq!(chain, Chain::Main);
    // Locked, sethdseed is refused.
    let e = seed::new_entropy();
    assert!(set_seed(&mut c, &e, chain).await.unwrap_err().contains("walletpassphrase"));
    assert!(unlock(&mut c, &guard, "not it", 60).await.unwrap_err().contains("isn't the wallet's passphrase"));
    unlock(&mut c, &guard, &pass_a, 300).await.unwrap();
    let id = set_seed(&mut c, &e, chain).await.unwrap();
    let hd = seed::freebank_hd_seed(&e).unwrap();
    assert_eq!(call(&mut c, "getwalletinfo", vec![]).await["hdmasterkeyid"], json!(seed::key_id_hex(&id)));
    // The addresses the words predict, derived here the way Core does (BIP32 from the 32-byte seed).
    let first = call(&mut c, "getnewaddress", vec![json!(""), json!("legacy")]).await;
    let second = call(&mut c, "getnewaddress", vec![json!(""), json!("legacy")]).await;
    let change = call(&mut c, "getrawchangeaddress", vec![json!("legacy")]).await;
    assert_eq!(first, json!(seed::address(&hd, false, 0).unwrap()), "m/0'/0'/0'");
    assert_eq!(second, json!(seed::address(&hd, false, 1).unwrap()), "m/0'/0'/1'");
    assert_eq!(change, json!(seed::address(&hd, true, 0).unwrap()), "m/0'/1'/0'");
    // The xprv FreeBank shows is the one Core prints.
    let dump = dir.join("a-dump.txt");
    call(&mut c, "dumpwallet", vec![json!(dump.to_string_lossy())]).await;
    let text = std::fs::read_to_string(&dump).unwrap();
    std::fs::remove_file(&dump).unwrap();
    let core_xprv = text.lines().find_map(|l| l.strip_prefix("# extended private masterkey: ")).unwrap().to_string();
    assert_eq!(core_xprv, seed::master_xprv(&hd, chain).unwrap().as_str());
    // The node's own export of its HD seed (the " hdmaster=1" line) is the BIP85 HD-Seed WIF: what a
    // BIP85 tool shows for these words at index 0, and what sethdseed took.
    let exported = text
        .lines()
        .find(|l| l.split_whitespace().nth(2) == Some("hdmaster=1"))
        .and_then(|l| l.split_whitespace().next())
        .expect("dumpwallet lists the HD seed");
    assert!(exported == seed::wif(&hd, chain).as_str(), "the node's hdmaster WIF is the BIP85 WIF");
    drop(text);
    // The words go into the app's seed file, and the wallet locks again.
    let app_a = dir.join("app-a");
    job::save_words(&app_a, &e, &id, &pass_a, false).await.unwrap();
    crate::wallet::lock(&mut c).await.unwrap();
    assert_eq!(call(&mut c, "getwalletinfo", vec![]).await["unlocked_until"], json!(0));
    let p = protection_from(&call(&mut c, "getwalletinfo", vec![]).await, &app_a, false);
    assert!(p.protected && p.app_seed == AppSeed::Matches && !p.words_confirmed);
    // A backup made by the node, encrypted.
    let backup = dir.join("a-backup.dat");
    call(&mut c, "backupwallet", vec![json!(backup.to_string_lossy())]).await;
    let (enc, is_hd) = ops::inspect(&std::fs::read(&backup).unwrap()).unwrap();
    assert_eq!((enc, is_hd), (Some(true), true), "a backup of an encrypted HD wallet reads as such");

    // Node B: another new wallet, restored from the same words: the same addresses.
    let mut b = Node::start(&dir.join("b"), free_port());
    let mut cb = passphrase_first(&mut b, &pass_b).await;
    let typed = seed::words(&e).join(" ");
    let restored = seed::parse_words(&typed).unwrap();
    unlock(&mut cb, &guard, &pass_b, 300).await.unwrap();
    assert_eq!(set_seed(&mut cb, &restored, chain).await.unwrap(), id);
    rescan(&mut cb, Some((&pass_b, &guard)), &Report::default()).await.unwrap();
    assert_eq!(call(&mut cb, "getnewaddress", vec![json!(""), json!("legacy")]).await, first);
    assert_eq!(call(&mut cb, "getnewaddress", vec![json!(""), json!("legacy")]).await, second);
    // The same words can't go back into a wallet that has had them: B already has this seed, and
    // setting it again is a no-op, not an error.
    set_seed(&mut cb, &restored, chain).await.unwrap();
    crate::wallet::lock(&mut cb).await.unwrap();
    b.stop().await;

    // Change A's passphrase: the node takes the new one, and the words still open, with it.
    let changed = ops::change_passphrase(&app_a, &mut c, pass_a.clone(), pass_new.clone()).await.unwrap();
    assert_eq!(changed.seed_file, "updated");
    assert!(unlock(&mut c, &guard, &pass_a, 5).await.is_err(), "the old passphrase no longer unlocks");
    unlock(&mut c, &guard, &pass_new, 5).await.unwrap();
    crate::wallet::lock(&mut c).await.unwrap();
    let (opened, oid) = open_saved(&app_a, &pass_new).unwrap();
    assert_eq!((*opened, oid), (*e, id));
    assert!(open_saved(&app_a, &pass_a).is_err());
    let shown = ops::reveal(&app_a, &mut c, pass_new.clone(), "words").await.unwrap();
    assert_eq!(shown.words.as_deref().map(|w| w.join(" ")), Some(typed.clone()));
    assert_eq!(shown.matches_wallet, Some(true));
    let shown = ops::reveal(&app_a, &mut c, pass_new.clone(), "xprv").await.unwrap();
    assert_eq!(shown.xprv.as_deref(), Some(core_xprv.as_str()));
    // Nothing to move on a wallet whose only keys are the words'.
    assert_eq!(ops::move_plan(&mut c).await.unwrap().coins, 0);
    a.stop().await;
    std::fs::remove_dir_all(&dir).unwrap();
}

// ---- Through the app's own node manager ----

/// An app folder whose "installed release" runs the real freebankd standalone: a wrapper drops the
/// eCash arguments the app passes and adds the standalone ones. `exec` keeps the process the app's
/// child, so the app's start, stop and exit checks see freebankd itself.
fn app_with_release(dir: &Path) -> (Arc<NodeManager>, Settings) {
    use std::os::unix::fs::PermissionsExt;
    let app = dir.join("app");
    let bin = app.join("releases/vtest/freebank/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("freebankd");
    let mut script = String::from(
        "#!/usr/bin/env bash\nkeep=()\nfor a in \"$@\"; do\n  case \"$a\" in\n    -mainchaintransport=*|-enforceraddr=*|-mainchainrest=*|-mainchainchain=*|-mainchainblockpin=*|-grpcurlbin=*) ;;\n    *) keep+=(\"$a\") ;;\n  esac\ndone\n",
    );
    script.push_str(&format!("exec '{}' \"${{keep[@]}}\"", freebankd().display()));
    for a in STANDALONE.iter().filter(|a| !a.starts_with("-server")) {
        script.push_str(&format!(" {}", a));
    }
    script.push('\n');
    std::fs::write(&wrapper, script).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The app starts only a release its installer checked (node/install.rs's VERIFIED mark); the
    // binary behind this stand-in is the verified v0.2.16.
    std::fs::write(app.join("releases/vtest/.verified"), "test\n").unwrap();
    let grpcurl = dir.join("grpcurl");
    std::fs::write(&grpcurl, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&grpcurl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let port = free_port();
    let datadir = dir.join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    let s = Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        rpc_port: port,
        p2p_port: port + 1,
        installed_tag: Some("vtest".into()),
        grpcurl: Some(grpcurl.to_string_lossy().into_owned()),
        ..Default::default()
    };
    std::fs::write(app.join("settings.json"), serde_json::to_vec(&s).unwrap()).unwrap();
    (Arc::new(NodeManager::new(app)), s)
}

async fn app_client(mgr: &NodeManager, s: &Settings) -> FreeBankClient {
    let until = Instant::now() + Duration::from_secs(120);
    loop {
        if let Ok(mut c) = local_client(mgr, s, LONG) {
            if c.call_fresh("getwalletinfo", vec![]).await.is_ok() {
                return c;
            }
        }
        assert!(Instant::now() < until, "the app's node didn't come up");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tokio::test]
#[ignore]
async fn real_node_through_the_app() {
    let dir = scratch("fbwallet-app");
    let (mgr, s) = app_with_release(&dir);
    let (pass, pass2) = (random_pass(), random_pass());
    let guard = RelockGuard::default();
    crate::node::process::start(&mgr).await.unwrap();
    let mut c = app_client(&mgr, &s).await;
    let p = protection(&mut c, &mgr).await.unwrap();
    assert!(!p.encrypted && !p.protected && p.new_wallet && p.node_is_ours);
    // A new wallet's addresses wait (the screens and a paired phone's receive) until it is protected.
    assert!(addresses_held(&call(&mut c, "getwalletinfo", vec![]).await, &mgr.app_dir));

    // First run: passphrase first, new words. The app restarts its own node after encryptwallet.
    let report = Report::default();
    let words = job::run(&mgr, &guard, Job::Setup { passphrase: pass.clone(), restore: None, fresh: false }, &report)
        .await
        .unwrap()
        .expect("new words");
    assert_eq!(words.len(), 24);
    assert_eq!(report.get().stages, vec!["encrypt", "restart", "seed", "save"]);
    assert!(crate::node::process::child_alive(&mgr).await, "the app's node runs again");
    let mut c = app_client(&mgr, &s).await;
    let info = call(&mut c, "getwalletinfo", vec![]).await;
    assert_eq!(info["unlocked_until"], json!(0), "encrypted, and locked again");
    let p = protection(&mut c, &mgr).await.unwrap();
    assert!(p.protected && p.app_seed == AppSeed::Matches && p.backup_due && !p.words_confirmed);
    assert!(!addresses_held(&call(&mut c, "getwalletinfo", vec![]).await, &mgr.app_dir), "protected: addresses again");
    let e = seed::parse_words(&words.join(" ")).unwrap();
    let hd = seed::freebank_hd_seed(&e).unwrap();
    let first = call(&mut c, "getnewaddress", vec![json!(""), json!("legacy")]).await;
    assert_eq!(first, json!(seed::address(&hd, false, 0).unwrap()));
    assert!(mgr.activity.lock().unwrap().is_none(), "the Node tab isn't left busy");
    // Protected already: the setup is refused... (by the command; the job itself would redo nothing)
    // A backup, recorded.
    let docs = dir.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let saved = ops::backup(&mgr, &mut c, &docs).await.unwrap();
    assert_eq!(saved.len(), 1);
    let p = protection(&mut c, &mgr).await.unwrap();
    assert!(!p.backup_due && p.backup_at.is_some());

    // Restore that backup from a file: the wallet moves aside, the copy goes in, the node restarts.
    let f = ops::take_upload(&mgr.app_dir, &std::fs::read(&saved[0]).unwrap()).unwrap();
    assert_eq!((f.encrypted, f.hd), (Some(true), true));
    let upload = ops::claim_upload(&f.token).unwrap();
    let report = Report::default();
    job::run(&mgr, &guard, Job::RestoreFile { upload: upload.clone() }, &report).await.unwrap();
    assert!(!upload.exists(), "the screen's copy is gone");
    let aside = PathBuf::from(report.get().moved_aside.unwrap());
    assert!(aside.is_file());
    let aside_dir = aside.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
    assert!(aside_dir.starts_with("node.old-"), "{}", aside_dir);
    // Recorded like setup's moves, so Obliterate and the Security panel treat it as FreeBank's own.
    let recorded = mgr.settings.lock().await.moved_aside.clone();
    assert_eq!(recorded, vec![aside.parent().unwrap().to_string_lossy().into_owned()]);
    let mut c = app_client(&mgr, &s).await;
    let p = protection(&mut c, &mgr).await.unwrap();
    assert!(p.protected, "the same wallet, the same words");
    assert_eq!(crate::node::wallet_files(Path::new(&s.datadir)).len(), 1);

    // A file the node can't use: the wallet goes back, the node runs again.
    let mut junk = vec![0u8; 16384];
    junk[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
    let f = ops::take_upload(&mgr.app_dir, &junk).unwrap();
    let report = Report::default();
    let err = job::run(&mgr, &guard, Job::RestoreFile { upload: ops::claim_upload(&f.token).unwrap() }, &report).await.unwrap_err();
    assert!(err.contains("put your wallet back"), "{}", err);
    assert_eq!(mgr.settings.lock().await.moved_aside, recorded, "the folder put back isn't recorded");
    let mut c = app_client(&mgr, &s).await;
    assert!(protection(&mut c, &mgr).await.unwrap().protected, "the wallet is back");
    assert!(crate::node::process::child_alive(&mgr).await);

    // Restore from the words into a new wallet (the current one moves aside first).
    let report = Report::default();
    let typed = Zeroizing::new(words.join(" "));
    let back = job::run(
        &mgr,
        &guard,
        Job::Setup { passphrase: pass2.clone(), restore: Some(seed::parse_words(&typed).unwrap()), fresh: true },
        &report,
    )
    .await
    .unwrap();
    assert!(back.is_none(), "restored words aren't shown again");
    let pr = report.get();
    assert_eq!(pr.stages, vec!["aside", "start", "encrypt", "restart", "seed", "save", "scan"]);
    assert!(pr.moved_aside.is_some() && pr.scan_to.is_some());
    assert_eq!(mgr.settings.lock().await.moved_aside.len(), 2, "each restore's folder recorded");
    let mut c = app_client(&mgr, &s).await;
    let p = protection(&mut c, &mgr).await.unwrap();
    assert!(p.protected && p.words_confirmed, "typing all the words in counts as having them");
    assert!(p.new_wallet);
    assert_eq!(call(&mut c, "getnewaddress", vec![json!(""), json!("legacy")]).await, first, "the same first address");
    assert!(open_saved(&mgr.app_dir, &pass2).is_ok(), "the words, sealed under the new wallet's passphrase");

    crate::node::process::stop(&mgr).await.unwrap();
    assert!(!crate::node::process::child_alive(&mgr).await);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A second wallet from the same words, through the app (several wallets, v0.2.6; node v0.2.19's createwallet): the
/// app makes it, encrypts it (its own node stops and starts again), and gives it the words' seed for index 1. The main
/// wallet answers by name beside it, and after a restart the new one opens on its first call.
#[tokio::test]
#[ignore]
async fn real_node_wallet_from_the_words() {
    let dir = scratch("fbwallet-words");
    let (mgr, s) = app_with_release(&dir);
    let pass = random_pass();
    let guard = RelockGuard::default();
    crate::node::process::start(&mgr).await.unwrap();
    app_client(&mgr, &s).await;
    let words = job::run(&mgr, &guard, Job::Setup { passphrase: pass.clone(), restore: None, fresh: false }, &Report::default())
        .await
        .unwrap()
        .expect("new words");
    let e = seed::parse_words(&words.join(" ")).unwrap();
    let client: crate::commands::ClientState = Default::default();

    let views = crate::wallets::add_words(&mgr, &client, "Savings".into(), pass.clone()).await.unwrap();
    assert_eq!(views.iter().map(|v| (v.label.as_str(), v.kind.as_str())).collect::<Vec<_>>(), vec![("Main", "main"), ("Savings", "words")]);
    assert!(views.iter().all(|v| v.encrypted == Some(true)), "{:?}", views.iter().map(|v| v.encrypted).collect::<Vec<_>>());
    assert!(crate::node::process::child_alive(&mgr).await, "the app's node runs again after encrypting");
    let s2 = mgr.settings.lock().await.clone();
    assert_eq!(s2.extra_wallets[0].name, "words-1");
    assert_eq!(s2.extra_wallets[0].index, Some(1));

    let hd1 = seed::hd_seed_at(&e, 1).unwrap();
    let mut w = app_client(&mgr, &s).await;
    w.set_wallet(Some("words-1".into()));
    let info = call(&mut w, "getwalletinfo", vec![]).await;
    assert_eq!(info["hdmasterkeyid"], json!(seed::key_id_hex(&seed::key_id(&hd1).unwrap())), "the words' seed for index 1");
    assert_eq!(info["unlocked_until"], json!(0), "encrypted and locked");
    assert_eq!(call(&mut w, "getnewaddress", vec![json!(""), json!("legacy")]).await, json!(seed::address(&hd1, false, 0).unwrap()));
    // The main wallet, named: still the words' index 0.
    let hd0 = seed::freebank_hd_seed(&e).unwrap();
    let mut m = app_client(&mgr, &s).await;
    m.set_wallet(s2.main_wallet.clone());
    assert_eq!(call(&mut m, "getwalletinfo", vec![]).await["hdmasterkeyid"], json!(seed::key_id_hex(&seed::key_id(&hd0).unwrap())));

    // A restart: only the main wallet opens by itself; the new one opens on its first call.
    crate::node::process::stop(&mgr).await.unwrap();
    crate::node::process::start(&mgr).await.unwrap();
    let mut w = app_client(&mgr, &s).await;
    w.set_wallet(Some("words-1".into()));
    assert_eq!(call(&mut w, "getwalletinfo", vec![]).await["hdmasterkeyid"], info["hdmasterkeyid"]);

    crate::rpc::set_main_wallet(None);
    crate::node::process::stop(&mgr).await.unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A wallet kept for someone else's phone (v0.2.8, `phone::hosted`), made in the app's own node: created, encrypted (the
/// node stops and the app starts it again), given fresh words' key; its first address is the member address. Then
/// those words, restored through the app on a second node, make the same wallet: the shopkeeper's move home.
#[tokio::test]
#[ignore]
async fn real_node_hosted_wallet_and_its_move_home() {
    use crate::phone::hosted::{HostedPhone, Step};
    struct NoEvents;
    impl crate::phone::Events for NoEvents {
        fn emit(&self, _: &str, _: Value) {}
    }
    let dir = scratch("fbwallet-hosted");
    let (mgr, s) = app_with_release(&dir);
    let mgr_arc = mgr.clone();
    crate::node::process::start(&mgr).await.unwrap();
    let c = app_client(&mgr, &s).await;
    let client = Arc::new(tokio::sync::Mutex::new(c));
    let (phone, _rx) = crate::phone::Phone::new(
        &mgr.app_dir,
        Arc::new(crate::phone::commands::NodeRpc(client.clone())),
        Arc::new(NoEvents),
        Arc::new(crate::phone::unix_now),
    )
    .unwrap();
    phone.set_maker(Arc::new(crate::wallets::HostedMaker { mgr: mgr_arc }));
    let hid = "testshop00000000".to_string();
    phone.host_for_test(HostedPhone {
        id: hid.clone(),
        name: "Shop".into(),
        p_pub: "x".into(),
        house: 5,
        house_name: "Bank of the Stall".into(),
        by: "Owner".into(),
        wallet: format!("hosted-{hid}"),
        step: Step::Setup,
        ..Default::default()
    });
    phone.clone().make_wallet(hid.clone()).await;
    let h = phone.hosted_list().into_iter().find(|h| h.id == hid).unwrap();
    assert_eq!(h.step, Step::Words, "{:?}", h.why);
    let member = h.member.clone().unwrap();
    assert!(crate::node::process::child_alive(&mgr).await, "the app's node runs again after encrypting");
    // The words the phone will show, and their first address.
    let keys: Value = serde_json::from_slice(&std::fs::read(mgr.app_dir.join("phone/hosted-keys.json")).unwrap()).unwrap();
    let entropy: [u8; 32] = hex::decode(keys["keys"][0]["entropy"].as_str().unwrap()).unwrap().try_into().unwrap();
    let hd = seed::freebank_hd_seed(&entropy).unwrap();
    assert_eq!(seed::address(&hd, false, 0).unwrap(), member);
    // The hosted wallet: encrypted and locked, with the words' key; the main wallet untouched and named from now on.
    let mut w = app_client(&mgr, &s).await;
    w.set_wallet(Some(format!("hosted-{hid}")));
    let info = call(&mut w, "getwalletinfo", vec![]).await;
    assert_eq!(info["unlocked_until"], json!(0));
    assert_eq!(info["hdmasterkeyid"], json!(seed::key_id_hex(&seed::key_id(&hd).unwrap())));
    assert!(mgr.settings.lock().await.main_wallet.is_some());
    crate::rpc::set_main_wallet(None);
    crate::node::process::stop(&mgr).await.unwrap();

    // Home: the same words restored through the app on another node give the same first address.
    let dir2 = scratch("fbwallet-home");
    let (mgr2, s2) = app_with_release(&dir2);
    crate::node::process::start(&mgr2).await.unwrap();
    app_client(&mgr2, &s2).await;
    let guard = RelockGuard::default();
    let words = seed::words(&entropy).join(" ");
    let typed = Zeroizing::new(words);
    job::run(
        &mgr2,
        &guard,
        Job::Setup { passphrase: random_pass(), restore: Some(seed::parse_words(&typed).unwrap()), fresh: false },
        &Report::default(),
    )
    .await
    .unwrap();
    let mut c2 = app_client(&mgr2, &s2).await;
    assert_eq!(call(&mut c2, "getnewaddress", vec![json!(""), json!("legacy")]).await, json!(member), "the same wallet at home");
    crate::node::process::stop(&mgr2).await.unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&dir2).unwrap();
}
