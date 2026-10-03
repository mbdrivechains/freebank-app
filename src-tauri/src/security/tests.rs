use super::*;

/// A fresh folder for one test, under the system's temp folder.
fn base(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fbsec-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A Berkeley DB btree page (the magic at byte 12, as a wallet starts), `extra` inside it.
fn bdb(extra: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 8192];
    b[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());
    b[5000..5000 + extra.len()].copy_from_slice(extra);
    b
}

#[cfg(unix)]
fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

// ---- rpc_call's allowlist ----

/// Every .ts and .svelte file under the frontend's src/.
fn frontend_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            frontend_files(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("ts" | "svelte")) {
            out.push(p);
        }
    }
}

/// The quoted name at the start of `s` ('x' or "x"), if any.
fn quoted(s: &str) -> Option<String> {
    let q = s.chars().next()?;
    (q == '\'' || q == '"').then(|| s[1..].chars().take_while(|c| *c != q).collect())
}

/// Every method a screen sends through rpc_call, in any file of the frontend: fbCall("…"), and
/// rpc_call invoked with a literal method ({ method: "…" }).
fn frontend_calls() -> Vec<String> {
    let mut files = Vec::new();
    frontend_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../src"), &mut files);
    assert!(files.len() > 20, "the frontend's files: {:?}", files);
    let mut found = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let mut names: Vec<String> = src.split("fbCall(").skip(1).filter_map(quoted).collect();
        for part in src.split("rpc_call").skip(1) {
            let near: String = part.chars().take(300).collect();
            if let Some(after) = near.split("method:").nth(1) {
                names.extend(quoted(after.trim_start()));
            }
        }
        for n in names {
            if !found.contains(&n) {
                found.push(n);
            }
        }
    }
    found
}

#[test]
fn allowlist_covers_every_screen_call() {
    let calls = frontend_calls();
    assert!(calls.len() >= 26, "found only {:?}", calls);
    for m in &calls {
        assert!(allow_rpc(m).is_ok(), "{} is sent by the screens but refused", m);
    }
    for m in ["getdepositaddress", "getwalletinfo", "getblockcount", "gettransaction", "listmynotes", "claimbillescrow"] {
        assert!(calls.iter().any(|c| c == m), "{} not found in the frontend", m);
    }
}

#[test]
fn allowlist_keeps_the_receipt_reads() {
    // TxReceipt, TxDetails and the receipts' block ticker (lib/receipts.ts) read these through rpc_call.
    for m in ["gettransaction", "getblockcount", "getblockheader"] {
        assert!(allow_rpc(m).is_ok(), "{}", m);
    }
}

#[test]
fn allowlist_refuses_the_wallet_sensitive_calls() {
    for m in [
        "walletpassphrase",
        "walletpassphrasechange",
        "encryptwallet",
        "sethdseed",
        "dumpprivkey",
        "dumpwallet",
        "importprivkey",
        "importwallet",
        "backupwallet",
        "signrawtransactionwithwallet",
        "bumpfee",
        "rescanblockchain",
        "stop",
        "signmessage",
        // and anything else that moves coins, changes the wallet or the node
        "sendtoaddress",
        "sendmany",
        "sendfrom",
        "sendrawtransaction",
        "walletlock",
        "getnewaddress",
        "keypoolrefill",
        "importaddress",
        "importmulti",
        "importpubkey",
        "lockunspent",
        "settxfee",
        "abandontransaction",
        "createwithdrawal",
        "refreshbmm",
        "setcoinbasetag",
        "setban",
        "addnode",
        "invalidateblock",
        "logging",
        "setnetworkactive",
        "help",
        "",
    ] {
        let e = allow_rpc(m).expect_err(m);
        assert!(e.contains("isn't on the app's list of allowed calls"), "{}", e);
    }
    // Names match exactly, as the node matches them.
    for m in ["DumpPrivKey", "dumpprivkey ", " getbalance", "getbalance\n", "getbalance/x"] {
        assert!(allow_rpc(m).is_err(), "{:?}", m);
    }
    let long = "x".repeat(10_000);
    assert!(allow_rpc(&long).unwrap_err().len() < 200, "the error quotes at most 64 characters");
}

#[test]
fn allowlist_has_no_sensitive_or_duplicate_names() {
    let mut seen = std::collections::HashSet::new();
    for m in RPC_ALLOWED {
        assert!(seen.insert(m), "{} listed twice", m);
        assert!(!m.is_empty() && m.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "{}", m);
        assert!(
            !["dump", "import", "passphrase", "encrypt", "sethdseed", "backup", "sign", "bump", "rescan", "stop", "send"]
                .iter()
                .any(|bad| m.contains(bad)),
            "{} looks wallet-sensitive",
            m
        );
    }
}

// ---- The wallet's passphrase ----

#[test]
fn wallet_levels() {
    let enc = WalletStatus { encrypted: true, unlocked_until: 0 };
    let plain = WalletStatus { encrypted: false, unlocked_until: 0 };
    assert_eq!(wallet_check(&Ok(Some(enc))).level, Level::Ok);
    let red = wallet_check(&Ok(Some(plain)));
    assert_eq!((red.id, red.level), ("wallet", Level::Red));
    assert!(red.fix.contains("passphrase"));
    assert_eq!(wallet_check(&Ok(None)).level, Level::Info);
    let down = wallet_check(&Err("Request failed: connection refused".into()));
    assert_eq!(down.level, Level::Info);
    assert!(down.detail.contains("connection refused"));
}

#[test]
fn node_errors_read_plainly() {
    assert_eq!(plain_error("RPC error -28: Loading block index..."), "Loading block index...");
    assert_eq!(plain_error("RPC error: Loading"), "Loading");
    assert_eq!(plain_error("Request failed: refused"), "Request failed: refused");
}

// ---- freebank.conf ----

#[test]
fn conf_lines() {
    let c = parse_conf(
        "# FreeBank node settings\n\
         coinbasetag=my house # a comment\n\
         rpcbind = 127.0.0.1\n\
         rpcallowip=10.0.0.0/8\n\
         rpcallowip=192.168.1.0/24\n\
         zmqpubrawtx=tcp://0.0.0.0:28332\n\
         zmqpubhashblock = tcp://127.0.0.1:28333\n\
         #rpcpassword=hunter2\n\
         listen=0\n\
         listen=1\n\
         [main]\n\
         unknown\n",
    );
    assert_eq!(c.rpcbind, vec!["127.0.0.1"]);
    assert_eq!(c.rpcallowip, vec!["10.0.0.0/8", "192.168.1.0/24"]);
    assert_eq!(
        c.zmq,
        vec![
            ("zmqpubrawtx".to_string(), "tcp://0.0.0.0:28332".to_string()),
            ("zmqpubhashblock".to_string(), "tcp://127.0.0.1:28333".to_string())
        ]
    );
    assert_eq!(c.listen, Some(false), "the first value is the one freebankd uses");
    assert!(!c.rpc_login, "a commented-out password doesn't count");
    assert!(parse_conf("rpcpassword=x\n").rpc_login);
    assert!(parse_conf("rpcauth=u:salt$hash\n").rpc_login);
    assert_eq!(parse_conf("nolisten=1\n").listen, Some(false));
    assert_eq!(parse_conf("nolisten=0\n").listen, Some(true));
    assert_eq!(parse_conf("listen=\n").listen, Some(true));
    assert_eq!(parse_conf("").listen, None);
    assert_eq!(parse_conf(""), Conf::default());
}

#[test]
fn hosts_and_loopback() {
    assert_eq!(host_part("127.0.0.1:8454"), "127.0.0.1");
    assert_eq!(host_part("[::1]:8454"), "::1");
    assert_eq!(host_part("::1"), "::1");
    assert_eq!(host_part("fe80::1"), "fe80::1");
    assert_eq!(host_part("localhost"), "localhost");
    assert_eq!(host_part(" 100.64.0.20:50051 "), "100.64.0.20");
    for h in ["127.0.0.1", "127.1.2.3", "::1", "[::1]", "localhost", "LOCALHOST"] {
        assert!(is_loopback_host(h), "{}", h);
    }
    for h in ["0.0.0.0", "::", "192.168.1.5", "100.64.0.20", "example.com", "", "*"] {
        assert!(!is_loopback_host(h), "{}", h);
    }
    for z in ["tcp://127.0.0.1:28332", "tcp://localhost:1", "tcp://[::1]:28332", "ipc:///tmp/zmq", "tcp://lo:28332", "tcp://eth0;127.0.0.1:5"] {
        assert!(zmq_is_local(z), "{}", z);
    }
    for z in ["tcp://*:28332", "tcp://0.0.0.0:28332", "tcp://eth0:28332", "tcp://192.168.1.5:1", "tcp://[::]:1", "28332", ""] {
        assert!(!zmq_is_local(z), "{}", z);
    }
}

#[test]
fn rpc_levels() {
    let conf = Path::new("/d/freebank.conf");
    let ok = rpc_check(&Conf::default(), conf, 8454, &[]);
    assert_eq!((ok.id, ok.level), ("rpc", Level::Ok));
    assert!(ok.detail.contains("8454"));

    // rpcbind alone is ignored by freebankd: still loopback only.
    let bind_only = Conf { rpcbind: vec!["0.0.0.0".into()], ..Default::default() };
    let c = rpc_check(&bind_only, conf, 8454, &[]);
    assert_eq!(c.level, Level::Ok);
    assert!(c.detail.contains("ignores the rpcbind= line"));

    // rpcallowip alone binds every address.
    let allow = Conf { rpcallowip: vec!["192.168.1.0/24".into()], ..Default::default() };
    let c = rpc_check(&allow, conf, 8454, &[]);
    assert_eq!(c.level, Level::Red);
    assert!(c.detail.contains("every network address"));
    assert!(c.fix.contains("/d/freebank.conf"));

    // rpcallowip with loopback binds only: fine. With a network bind: red.
    let local = Conf { rpcallowip: vec!["127.0.0.1".into()], rpcbind: vec!["127.0.0.1".into(), "[::1]:8454".into()], ..Default::default() };
    assert_eq!(rpc_check(&local, conf, 8454, &[]).level, Level::Ok);
    let lan = Conf { rpcallowip: vec!["0.0.0.0/0".into()], rpcbind: vec!["127.0.0.1".into(), "192.168.1.5".into()], ..Default::default() };
    let c = rpc_check(&lan, conf, 8454, &[]);
    assert_eq!(c.level, Level::Red);
    assert!(c.detail.contains("192.168.1.5"));

    // Found answering on a network address: red, whatever the file says.
    let c = rpc_check(&Conf::default(), conf, 8454, &[addr("192.168.1.5:8454"), addr("[2001:db8::5]:8454")]);
    assert_eq!(c.level, Level::Red);
    assert!(c.detail.contains("192.168.1.5:8454, [2001:db8::5]:8454"), "{}", c.detail);
    assert!(c.fix.contains("without -rpcallowip"), "no lines in the file: the options came from elsewhere");
    assert!(rpc_check(&allow, conf, 8454, &[addr("10.0.0.2:8454")]).fix.contains("Remove the rpcallowip="));
}

#[test]
fn zmq_levels() {
    let conf = Path::new("/d/freebank.conf");
    assert_eq!(zmq_check(&Conf::default(), conf).title, "ZMQ is off");
    let local = Conf { zmq: vec![("zmqpubhashblock".into(), "tcp://127.0.0.1:28332".into())], ..Default::default() };
    assert_eq!(zmq_check(&local, conf).level, Level::Ok);
    let open = Conf {
        zmq: vec![
            ("zmqpubhashblock".into(), "tcp://127.0.0.1:28332".into()),
            ("zmqpubrawtx".into(), "tcp://*:28333".into()),
        ],
        ..Default::default()
    };
    let c = zmq_check(&open, conf);
    assert_eq!((c.id, c.level), ("zmq", Level::Warn));
    assert!(c.detail.starts_with("zmqpubrawtx=tcp://*:28333 publishes"), "{}", c.detail);
    assert!(!c.detail.contains("28332"));
}

#[test]
fn p2p_is_info() {
    let c = p2p_check(&Conf::default(), 8455);
    assert_eq!((c.id, c.level), ("p2p", Level::Info));
    assert!(c.detail.contains("8455"));
    let quiet = p2p_check(&Conf { listen: Some(false), ..Default::default() }, 8455);
    assert_eq!(quiet.level, Level::Info);
    assert!(quiet.detail.contains("listen=0"));
}

// ---- Wallet backups ----

#[test]
fn wallet_files_by_mkey() {
    let d = base("mkey");
    let enc = d.join("enc.dat");
    std::fs::write(&enc, bdb(b"\x04mkey\x01\x00\x00\x00")).unwrap();
    let plain = d.join("plain.dat");
    std::fs::write(&plain, bdb(b"\x03key\x21\x02")).unwrap();
    let text = d.join("notes.dat");
    std::fs::write(&text, b"just some text that mentions \x04mkey").unwrap();
    let tiny = d.join("tiny.dat");
    std::fs::write(&tiny, b"\x04mkey").unwrap();
    let mut sqlite = b"SQLite format 3\0".to_vec();
    sqlite.extend_from_slice(&[0u8; 4000]);
    std::fs::write(d.join("sqlite-plain.dat"), &sqlite).unwrap();
    sqlite.extend_from_slice(b"\x04mkey");
    std::fs::write(d.join("sqlite-enc.dat"), &sqlite).unwrap();

    assert_eq!(wallet_file(&enc).unwrap(), WalletFile::Encrypted);
    assert_eq!(wallet_file(&plain).unwrap(), WalletFile::Unencrypted);
    assert_eq!(wallet_file(&text).unwrap(), WalletFile::NotAWallet);
    assert_eq!(wallet_file(&tiny).unwrap(), WalletFile::NotAWallet);
    assert_eq!(wallet_file(&d.join("sqlite-plain.dat")).unwrap(), WalletFile::Unencrypted);
    assert_eq!(wallet_file(&d.join("sqlite-enc.dat")).unwrap(), WalletFile::Encrypted);
    assert!(wallet_file(&d.join("missing.dat")).is_err());

    // Remembered while the file stays the same; read again once it changes.
    let c = d.join("cached.dat");
    std::fs::write(&c, bdb(b"\x03key")).unwrap();
    assert_eq!(wallet_file_cached(&c).unwrap(), WalletFile::Unencrypted);
    assert_eq!(wallet_file_cached(&c).unwrap(), WalletFile::Unencrypted);
    let mut sealed = bdb(b"\x04mkey");
    sealed.extend_from_slice(b"grown");
    std::fs::write(&c, &sealed).unwrap();
    assert_eq!(wallet_file_cached(&c).unwrap(), WalletFile::Encrypted, "a new size is read again");
    assert!(wallet_file_cached(&d.join("missing.dat")).is_err());

    // The record found across the 1 MiB reads, at every split of its five bytes.
    for split in 0..=5usize {
        let mut big = bdb(b"");
        big.resize(16 + (1 << 20) - split, 0);
        big.extend_from_slice(b"\x04mkey");
        big.extend_from_slice(&[0u8; 100]);
        let p = d.join(format!("big-{}.dat", split));
        std::fs::write(&p, &big).unwrap();
        assert_eq!(wallet_file(&p).unwrap(), WalletFile::Encrypted, "split {}", split);
    }
    let mut big = bdb(b"");
    big.resize(3 << 20, 0);
    std::fs::write(d.join("big-plain.dat"), &big).unwrap();
    assert_eq!(wallet_file(&d.join("big-plain.dat")).unwrap(), WalletFile::Unencrypted);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn backups_found_by_name_record_and_session() {
    let d = base("backups");
    let docs = d.join("Documents");
    let other = d.join("elsewhere");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let a = docs.join("FreeBank-wallet-20260927-101010.dat");
    let b = docs.join("FreeBank-wallet-20260928-111111-wallets-savings.dat");
    let chosen = other.join("my-backup.dat");
    for p in [&a, &b, &chosen] {
        std::fs::write(p, bdb(b"")).unwrap();
    }
    std::fs::write(docs.join("FreeBank-wallet-notes.txt"), b"x").unwrap();
    std::fs::write(docs.join("wallet.dat"), bdb(b"")).unwrap();
    std::fs::create_dir_all(docs.join("FreeBank-wallet-folder.dat")).unwrap();
    assert_eq!(backups_in(&docs), vec![a.clone(), b.clone()]);
    assert!(backups_in(&d.join("missing")).is_empty());

    // The record: written 0600, kept across sessions, each path once, nothing after Obliterate.
    let app = d.join("app");
    let mgr = NodeManager::new(app.clone());
    record_backups(&mgr, &[a.to_string_lossy().into_owned(), chosen.to_string_lossy().into_owned()]);
    record_backups(&mgr, &[chosen.to_string_lossy().into_owned()]);
    record_backups(&mgr, &[]);
    assert_eq!(recorded_backups(&app), vec![a.clone(), chosen.clone()]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(app.join(BACKUPS_FILE)).unwrap().permissions().mode();
        assert_eq!(m & 0o777, 0o600);
    }
    assert!(recorded_backups(&d.join("nowhere")).is_empty());
    mgr.obliterated.store(true, std::sync::atomic::Ordering::SeqCst);
    record_backups(&mgr, &[b.to_string_lossy().into_owned()]);
    assert_eq!(recorded_backups(&app).len(), 2, "nothing is written after Obliterate");

    // All of them, once each (the same file by another path too); a listed backup that is gone is
    // left out.
    let gone = other.join("deleted.dat");
    let known = known_backups(
        &[a.clone(), chosen.clone(), gone, docs.join("../Documents/FreeBank-wallet-20260927-101010.dat")],
        &[docs.clone()],
    );
    assert_eq!(known, vec![a.clone(), chosen.clone(), b.clone()]);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn backups_levels() {
    let ok = backups_check(&[], &[]);
    assert_eq!((ok.id, ok.level), ("backups", Level::Ok));
    let sealed = backups_check(&[(PathBuf::from("/h/a.dat"), WalletFile::Encrypted)], &[]);
    assert_eq!(sealed.level, Level::Ok);
    assert!(sealed.detail.contains("needs your passphrase"));
    let one = backups_check(
        &[(PathBuf::from("/h/a.dat"), WalletFile::Encrypted), (PathBuf::from("/h/b.dat"), WalletFile::Unencrypted)],
        &[],
    );
    assert_eq!(one.level, Level::Red);
    assert_eq!(one.title, "An unencrypted wallet backup is on this computer");
    assert_eq!(one.files, vec!["/h/b.dat"]);
    assert!(one.fix.contains("never deletes"));
    let two = backups_check(
        &[(PathBuf::from("/h/b.dat"), WalletFile::Unencrypted), (PathBuf::from("/h/c.dat"), WalletFile::Unencrypted)],
        &[],
    );
    assert_eq!(two.title, "2 unencrypted wallet backups are on this computer");
    assert_eq!(two.files.len(), 2);
    // The screens get the files only when there are some.
    let json = serde_json::to_value(&ok).unwrap();
    assert!(json.get("files").is_none());
    assert_eq!(json["level"], "ok");
    assert_eq!(serde_json::to_value(&two).unwrap()["level"], "red");
}

/// Wallets in folders a restore or setup moved aside: the ones Settings records and the
/// <datadir>.old-<digits> folders, each wallet tested for its "mkey" record like a backup.
#[test]
fn moved_aside_wallets_are_checked() {
    let d = base("asides");
    let datadir = d.join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    // A restore's folder: the replaced wallet, without a passphrase.
    let restored = d.join("node.old-1790700000");
    std::fs::create_dir_all(&restored).unwrap();
    let plain = restored.join("wallet.dat.old-20260929-181500");
    std::fs::write(&plain, bdb(b"\x03key")).unwrap();
    // Setup's move of an older data folder, recorded in Settings, holding an encrypted wallet.
    let older = d.join("elsewhere/freebank-old");
    std::fs::create_dir_all(older.join("blocks")).unwrap();
    let sealed = older.join("wallet.dat");
    std::fs::write(&sealed, bdb(b"\x04mkey\x01\x00\x00\x00")).unwrap();
    // A recorded folder in a freebankd-made layout, without a passphrase.
    let layout = d.join("node.old-1790700005");
    std::fs::create_dir_all(layout.join("wallets")).unwrap();
    let plain2 = layout.join("wallets/wallet.dat");
    std::fs::write(&plain2, bdb(b"\x03key")).unwrap();
    // Not moved aside: a name that doesn't fit, and the data folder itself.
    std::fs::create_dir_all(d.join("node.old-abc")).unwrap();
    std::fs::write(d.join("node.old-abc/wallet.dat"), bdb(b"\x03key")).unwrap();
    std::fs::write(datadir.join("wallet.dat"), bdb(b"\x03key")).unwrap();
    let s = Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        moved_aside: vec![older.to_string_lossy().into_owned(), layout.to_string_lossy().into_owned()],
        ..Default::default()
    };
    // Recorded first, then the named ones; the recorded one also named isn't listed twice.
    assert_eq!(aside_folders(&s), vec![older.clone(), layout.clone(), restored.clone()]);
    let found = aside_wallets(&s);
    assert_eq!(found, vec![sealed.clone(), plain2.clone(), plain.clone()]);
    let scanned: Vec<(PathBuf, WalletFile)> = found.iter().map(|p| (p.clone(), wallet_file(p).unwrap())).collect();

    // Only the wallets without a passphrase are red, with "Show in folder" for each.
    let c = backups_check(&[], &scanned);
    assert_eq!((c.id, c.level), ("backups", Level::Red));
    assert_eq!(c.title, "2 unencrypted wallets FreeBank moved aside are on this computer");
    assert_eq!(c.files, vec![plain2.to_string_lossy().into_owned(), plain.to_string_lossy().into_owned()]);
    assert!(c.detail.contains("moved these wallets aside when it set up or restored your wallet"));
    assert!(c.fix.contains("may hold coins you still need") && c.fix.contains("never deletes them"));
    let one = backups_check(&[], &scanned[..2]);
    assert_eq!(one.title, "An unencrypted wallet FreeBank moved aside is on this computer");
    // With an unencrypted backup too: one line for all of them.
    let both = backups_check(&[(PathBuf::from("/h/b.dat"), WalletFile::Unencrypted)], &scanned);
    assert_eq!(both.title, "3 unencrypted wallet copies are on this computer");
    assert_eq!(both.files.len(), 3);
    assert!(both.detail.starts_with("FreeBank saved this backup before your wallet had a passphrase."));
    // Moved aside with a passphrase: fine, and said.
    let fine = backups_check(&[], &scanned[..1]);
    assert_eq!(fine.level, Level::Ok);
    assert!(fine.detail.ends_with("The wallet FreeBank moved aside needs a passphrase too."));
    // Nothing moved aside: as before.
    assert!(aside_wallets(&Settings { datadir: d.join("none/node").to_string_lossy().into_owned(), ..Default::default() }).is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}

/// "Show in folder" opens only a file on its list, compared as real paths.
#[test]
fn reveal_only_what_is_listed() {
    let d = base("reveal");
    let a = d.join("a.dat");
    std::fs::write(&a, b"a").unwrap();
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let known = vec![a.clone(), d.join("missing.dat")];
    assert_eq!(revealable(known.clone(), &d.join("sub/../a.dat")), Some(a.clone()));
    assert_eq!(revealable(known.clone(), &d.join("b.dat")), None);
    assert_eq!(revealable(known, &d.join("missing.dat")), None, "only files that are there");
    std::fs::remove_dir_all(&d).unwrap();
}

// ---- The node program's signature ----

#[test]
fn signature_levels() {
    let d = base("sig");
    let mgr = NodeManager::new(d.clone());
    let none = signature_check(&mgr, None);
    assert_eq!((none.id, none.level), ("signature", Level::Info));
    assert_eq!(signature_check(&mgr, Some("v0.2.16")).level, Level::Info, "installed, but the program is gone");

    let bin = mgr.freebankd("v0.2.16");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    let red = signature_check(&mgr, Some("v0.2.16"));
    assert_eq!(red.level, Level::Red);
    assert!(red.detail.contains("won't start it"));
    assert!(red.fix.starts_with("Download it again on the Node tab"));
    // The marker the installer writes (node/install.rs, VERIFIED), with the archive's hash.
    std::fs::write(mgr.release_dir("v0.2.16").join(VERIFIED_MARK), "b19da93f\n").unwrap();
    let ok = signature_check(&mgr, Some("v0.2.16"));
    assert_eq!(ok.level, Level::Ok);
    assert!(ok.detail.contains("v0.2.16"));
    std::fs::remove_dir_all(&d).unwrap();
}

// ---- Who else can read the keys on disk ----

/// Can every account get from / down to `dir`?
#[cfg(unix)]
fn open_to_all(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    dir.canonicalize()
        .unwrap()
        .ancestors()
        .all(|a| std::fs::metadata(a).map(|m| m.permissions().mode() & 0o001 != 0).unwrap_or(false))
}

#[cfg(unix)]
#[test]
fn permissions_follow_the_folders_above() {
    let d = base("perm");
    chmod(&d, 0o755);
    let f = d.join("secret");
    std::fs::write(&f, b"x").unwrap();
    chmod(&f, 0o600);
    assert_eq!(readable_by_others(&f), None);
    assert_eq!(readable_by_others(&d.join("missing")), None);

    // A readable file in a private folder stays private.
    let private = d.join("private");
    std::fs::create_dir(&private).unwrap();
    chmod(&private, 0o700);
    let inside = private.join("cookie");
    std::fs::write(&inside, b"x").unwrap();
    chmod(&inside, 0o644);
    assert_eq!(readable_by_others(&inside), None);

    // Where the test folder is reachable by everyone (the usual /tmp), the open cases show too.
    if open_to_all(&d) {
        chmod(&f, 0o644);
        assert_eq!(readable_by_others(&f), Some("every account"));
        chmod(&f, 0o640);
        assert_eq!(readable_by_others(&f), Some("your group"));
        chmod(&private, 0o750);
        assert_eq!(readable_by_others(&inside), Some("your group"), "everyone is stopped, the group isn't");
        chmod(&private, 0o711);
        assert_eq!(readable_by_others(&inside), Some("every account"), "a folder anyone may pass through");
        chmod(&d, 0o700);
        assert_eq!(readable_by_others(&f), None);
    }
    chmod(&d, 0o755);
    chmod(&private, 0o755);
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_secrets_on_disk() {
    let d = base("secrets");
    let app = d.join("app");
    let datadir = d.join("node");
    assert!(secrets(&app, &datadir, &Conf::default()).is_empty());

    std::fs::create_dir_all(app.join("phone")).unwrap();
    std::fs::write(app.join("phone/desktop.key"), b"k").unwrap();
    std::fs::write(app.join("phone/devices.json"), b"[]").unwrap();
    std::fs::create_dir_all(app.join("wallet")).unwrap();
    std::fs::write(app.join("wallet/seed.enc"), b"s").unwrap();
    std::fs::create_dir_all(&datadir).unwrap();
    std::fs::write(datadir.join(".cookie"), b"__cookie__:x").unwrap();
    std::fs::write(datadir.join("freebank.conf"), b"rpcpassword=x\n").unwrap();
    std::fs::write(datadir.join("wallet.dat"), b"w").unwrap();

    let paths = |c: &Conf| secrets(&app, &datadir, c).into_iter().map(|s| s.path).collect::<Vec<_>>();
    assert_eq!(
        paths(&Conf::default()),
        vec![
            app.join("phone"),
            app.join("phone/desktop.key"),
            app.join("phone/devices.json"),
            app.join("wallet/seed.enc"),
            datadir.join(".cookie"),
            datadir.join("wallet.dat"),
        ],
        "freebank.conf only when it holds a login"
    );
    let with_login = paths(&Conf { rpc_login: true, ..Default::default() });
    assert!(with_login.contains(&datadir.join("freebank.conf")));
    let s = secrets(&app, &datadir, &Conf::default());
    assert_eq!(s[1].folder.as_deref(), Some(app.join("phone").as_path()));
    assert_eq!(s[3].folder.as_deref(), Some(app.join("wallet").as_path()));
    assert_eq!(s[4].folder.as_deref(), Some(datadir.as_path()));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn files_levels() {
    let ok = files_check(&[]);
    assert_eq!((ok.id, ok.level), ("files", Level::Ok));
    let node = PathBuf::from("/home/u/.freebank");
    let exposed = vec![
        (Secret { path: node.join(".cookie"), folder: Some(node.clone()) }, "every account"),
        (Secret { path: node.join("wallet.dat"), folder: Some(node.clone()) }, "your group"),
        (Secret { path: PathBuf::from("/x/key"), folder: None }, "your group"),
    ];
    let c = files_check(&exposed);
    assert_eq!(c.level, Level::Warn);
    assert!(c.detail.starts_with("Every account on this computer can read /home/u/.freebank/.cookie"), "{}", c.detail);
    assert_eq!(c.fix, "In a terminal, run: chmod 700 '/home/u/.freebank'; chmod 600 '/x/key'");
    let group = files_check(&exposed[1..]);
    assert!(group.detail.starts_with("Accounts in your group"));
}

// ---- The eCash side ----

#[test]
fn stack_levels() {
    let ok = stack_check("127.0.0.1:18302", "127.0.0.1:50051", &[], &[]);
    assert_eq!((ok.id, ok.level), ("stack", Level::Ok));
    let remote = stack_check("127.0.0.1:18302", "100.64.0.20:50051", &[], &[]);
    assert_eq!(remote.level, Level::Info);
    assert!(remote.detail.starts_with("The enforcer is at 100.64.0.20:50051."), "{}", remote.detail);
    assert!(remote.detail.contains("Its port belongs to the eCash side, not to FreeBank"), "{}", remote.detail);
    assert!(remote.fix.starts_with("Keep that port behind a firewall"));
    assert!(remote.detail.contains("must not be reachable by strangers"));
    let both = stack_check("host.lan:18302", "[fd7a::1]:50051", &[], &[]);
    assert!(both.detail.contains("The eCash node is at host.lan:18302; The enforcer is at [fd7a::1]:50051. Their ports belong"));
    assert!(both.fix.starts_with("Keep those ports"));
    let open = stack_check("127.0.0.1:18302", "127.0.0.1:50051", &[], &[addr("192.168.1.5:50051")]);
    assert_eq!(open.level, Level::Warn);
    assert!(open.detail.contains("the enforcer's gRPC port answers at 192.168.1.5:50051"));
    assert!(open.fix.contains("--serve-grpc-addr"));
    let rest = stack_check("127.0.0.1:18302", "127.0.0.1:50051", &[addr("10.0.0.2:18302")], &[]);
    assert!(rest.detail.contains("the eCash node's RPC port answers at 10.0.0.2:18302"));
}

// ---- Network probes ----

#[test]
fn own_addresses_leave_out_loopback() {
    for ip in network_addresses() {
        assert!(!ip.is_loopback() && !ip.is_unspecified(), "{}", ip);
        if let IpAddr::V6(v6) = ip {
            assert_ne!(v6.segments()[0] & 0xffc0, 0xfe80, "link-local {}", ip);
        }
    }
}

#[tokio::test]
async fn probes_see_what_a_port_is_bound_to() {
    // Everything here listens on loopback only. Linux answers on all of 127.0.0.0/8, so a program bound
    // to 127.0.0.2 stands in for one bound to a network address: the probe must find exactly it.
    let wait = Duration::from_millis(600);
    let lo = |n: u8| IpAddr::V4(Ipv4Addr::new(127, 0, 0, n));
    assert!(answering(&[lo(1)], 0, wait).await.is_empty());

    let one = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = one.local_addr().unwrap().port();
    assert_eq!(answering(&[lo(1)], port, wait).await, vec![SocketAddr::new(lo(1), port)]);
    // This computer's network addresses don't answer for a program bound to loopback. (Nothing else can
    // take this port on every address while `one` holds it on 127.0.0.1.)
    assert!(answering(&network_addresses(), port, wait).await.is_empty());
    drop(one);

    // Other tests run beside this one and bind 127.0.0.1, where the same port number can be free again,
    // so only 127.0.0.2 and 127.0.0.3 are asked here.
    if let Ok(two) = tokio::net::TcpListener::bind("127.0.0.2:0").await {
        let port = two.local_addr().unwrap().port();
        let found = answering(&[lo(2), lo(3)], port, wait).await;
        assert_eq!(found, vec![SocketAddr::new(lo(2), port)]);
    }
}

// ---- All of them ----

#[tokio::test]
async fn run_all_checks_worst_first() {
    let d = base("all");
    let mgr = NodeManager::new(d.join("app"));
    let datadir = d.join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    std::fs::write(datadir.join("freebank.conf"), "coinbasetag=x\nzmqpubrawtx=tcp://0.0.0.0:28332\n").unwrap();
    let backup = d.join("FreeBank-wallet-20260927-101010.dat");
    std::fs::write(&backup, bdb(b"")).unwrap();
    std::fs::write(d.join("FreeBank-wallet-20260927-101011.dat"), b"not a wallet").unwrap();
    let s = Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        // Nothing listens on port 1.
        rpc_port: 1,
        installed_tag: None,
        ..Default::default()
    };
    let wallet = Ok(Some(WalletStatus { encrypted: false, unlocked_until: 0 }));
    let backups = vec![backup.clone(), d.join("FreeBank-wallet-20260927-101011.dat")];
    let checks = run_checks(&mgr, &s, wallet, backups).await;

    let ids: Vec<&str> = checks.iter().map(|c| c.id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["backups", "files", "p2p", "rpc", "signature", "stack", "wallet", "zmq"]);
    assert!(checks.windows(2).all(|w| w[0].level >= w[1].level), "worst first: {:?}", ids);
    let get = |id: &str| checks.iter().find(|c| c.id == id).unwrap();
    assert_eq!(&ids[..2], &["wallet", "backups"], "the reds first, in the listed order");
    assert_eq!(get("backups").files, vec![backup.to_string_lossy().into_owned()]);
    assert_eq!(get("zmq").level, Level::Warn);
    assert_eq!(get("rpc").level, Level::Ok);
    assert_eq!(get("signature").level, Level::Info);
    assert_eq!(get("stack").level, Level::Ok);
    std::fs::remove_dir_all(&d).unwrap();
}

// ---- The link opener ----

#[test]
fn opener_scope_matches_the_frontend() {
    // tauri.conf.json's plugins.shell.open (the shell plugin wraps it in ^…$ and enforces it) and
    // lib/node.ts's OPENABLE must name the same links.
    let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    let scope = conf["plugins"]["shell"]["open"].as_str().expect("plugins.shell.open is set");
    let node_ts = include_str!("../../../src/lib/node.ts");
    let js = node_ts
        .split("export const OPENABLE =")
        .nth(1)
        .and_then(|rest| rest.trim_start().strip_prefix("/^"))
        .and_then(|rest| rest.split("$/;").next())
        .expect("OPENABLE in lib/node.ts");
    assert_eq!(js.replace("\\/", "/"), scope);
    // Only https, and only FreeBank's own hosts.
    assert!(scope.starts_with("https://("));
    assert!(!scope.contains("http?") && !scope.contains(".*") && !scope.contains("\\w+"));
    let perms = include_str!("../../capabilities/default.json");
    assert!(perms.contains("\"shell:allow-open\"") && !perms.contains("shell:allow-execute") && !perms.contains("shell:allow-spawn"));
}

// ---- The app updater ----

#[test]
fn the_updater_test_feature_never_reaches_a_release() {
    // app_update.rs reads another address and key only with the `update-test` feature (and then only a server on this
    // computer): no release build may enable it, by any route.
    let cargo = include_str!("../../Cargo.toml");
    for line in cargo.lines().filter(|l| l.contains("update-test")) {
        let line = line.trim();
        assert!(line.starts_with('#') || line == "update-test = []", "Cargo.toml turns it on: {line}");
    }
    // The Tauri CLI passes build.features to cargo, from tauri.conf.json and the files it merges in.
    let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    assert!(conf["build"].get("features").is_none(), "tauri.conf.json sets build.features");
    let src_tauri = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for other in ["tauri.linux.conf.json", "tauri.macos.conf.json", "tauri.windows.conf.json", "tauri.conf.json5", "Tauri.toml"] {
        let text = std::fs::read_to_string(src_tauri.join(other)).unwrap_or_default();
        assert!(!text.contains("features") && !text.contains("update-test"), "{other}");
    }
    for (name, text) in [
        ("release.yml", include_str!("../../../.github/workflows/release.yml")),
        ("build.sh", include_str!("../../../build/linux/build.sh")),
        ("rebuild.sh", include_str!("../../../build/linux/rebuild.sh")),
        ("Dockerfile", include_str!("../../../build/linux/Dockerfile")),
        ("package.json", include_str!("../../../package.json")),
    ] {
        assert!(!text.contains("update-test") && !text.contains("--features") && !text.contains("--all-features"), "{name}");
    }
    // Cargo's config files could add it with rustflags.
    for dir in [src_tauri.to_path_buf(), src_tauri.join("..")] {
        for f in [".cargo/config", ".cargo/config.toml"] {
            let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
            assert!(!text.contains("update-test"), "{}", dir.join(f).display());
        }
    }
}

// ---- Sample reports for the screens' browser harness ----

/// Prints two real reports as JSON between markers: a setup with every kind of problem, and a sound one.
/// `cargo test sample_reports -- --ignored --nocapture`
#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn sample_reports() {
    let d = base("sample");
    chmod(&d, 0o755);
    let bin_for = |mgr: &NodeManager| {
        let bin = mgr.freebankd("v0.2.16");
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    };

    // Everything wrong: no passphrase, RPC opened by freebank.conf, ZMQ open, an unencrypted backup, an
    // unchecked program, readable keys, the enforcer on another computer.
    let bad = d.join("bad");
    let app = bad.join("app");
    let datadir = bad.join("node");
    std::fs::create_dir_all(app.join("phone")).unwrap();
    std::fs::create_dir_all(&datadir).unwrap();
    for p in [&bad, &app, &datadir] {
        chmod(p, 0o755);
    }
    chmod(&app.join("phone"), 0o755);
    std::fs::write(app.join("phone/desktop.key"), b"k").unwrap();
    chmod(&app.join("phone/desktop.key"), 0o644);
    std::fs::write(
        datadir.join("freebank.conf"),
        "coinbasetag=alex\nrpcallowip=192.168.1.0/24\nzmqpubrawtx=tcp://0.0.0.0:28332\n",
    )
    .unwrap();
    let docs = bad.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let backup = docs.join("FreeBank-wallet-20260927-101010.dat");
    std::fs::write(&backup, bdb(b"\x03key")).unwrap();
    let mgr = NodeManager::new(app.clone());
    bin_for(&mgr);
    let s = Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        enforcer: "192.168.1.20:50051".into(),
        rpc_port: 1,
        installed_tag: Some("v0.2.16".into()),
        ..Default::default()
    };
    let plain = Ok(Some(WalletStatus { encrypted: false, unlocked_until: 0 }));
    let report = run_checks(&mgr, &s, plain, known_backups(&[], &[docs.clone()])).await;
    println!("=== BAD {}\n{}\n=== END", bad.display(), serde_json::to_string_pretty(&report).unwrap());

    // All sound: a passphrase, loopback only, a checked program, private keys, an encrypted backup.
    let good = d.join("good");
    let app = good.join("app");
    let datadir = good.join("node");
    std::fs::create_dir_all(app.join("phone")).unwrap();
    chmod(&app.join("phone"), 0o700);
    std::fs::create_dir_all(&datadir).unwrap();
    chmod(&datadir, 0o700);
    std::fs::write(datadir.join("freebank.conf"), "coinbasetag=alex\n").unwrap();
    let docs = good.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("FreeBank-wallet-20260929-090000.dat"), bdb(b"\x04mkey\x01\x00\x00\x00")).unwrap();
    let mgr = NodeManager::new(app.clone());
    bin_for(&mgr);
    std::fs::write(mgr.release_dir("v0.2.16").join(VERIFIED_MARK), "b19da93f\n").unwrap();
    let s = Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        rpc_port: 1,
        installed_tag: Some("v0.2.16".into()),
        ..Default::default()
    };
    let sealed = Ok(Some(WalletStatus { encrypted: true, unlocked_until: 0 }));
    let report = run_checks(&mgr, &s, sealed, known_backups(&[], &[docs.clone()])).await;
    println!("=== GOOD {}\n{}\n=== END", good.display(), serde_json::to_string_pretty(&report).unwrap());
    std::fs::remove_dir_all(&d).unwrap();
}

// ---- Against real files and a real node (run by hand) ----

/// What wallet_file makes of real files: FB_WALLET_FILES=<path>:<path>…
/// `cargo test real_wallet_files -- --ignored --nocapture`
#[test]
#[ignore]
fn real_wallet_files() {
    let list = std::env::var("FB_WALLET_FILES").expect("set FB_WALLET_FILES");
    for p in list.split(':').filter(|p| !p.is_empty()) {
        println!("{:?}\t{}", wallet_file(Path::new(p)).map_err(|e| e.to_string()), p);
    }
}

/// Every check against a running node: FB_NODE_DATADIR, FB_NODE_RPCPORT, FB_APP_DIR, FB_BACKUP_DIR,
/// and optionally FB_REST and FB_ENFORCER. Prints the report as JSON.
/// `cargo test real_node_checks -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn real_node_checks() {
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {}", k));
    let datadir = env("FB_NODE_DATADIR");
    let port: u16 = env("FB_NODE_RPCPORT").parse().unwrap();
    let mut c = crate::rpc::FreeBankClient::default();
    assert!(c.configure_local(&format!("http://127.0.0.1:{}", port), PathBuf::from(&datadir)), "no cookie");
    let wallet = crate::wallet::status(&mut c).await.map(Some).map_err(|e| plain_error(&e));
    let s = Settings {
        datadir,
        rpc_port: port,
        rest: std::env::var("FB_REST").unwrap_or_else(|_| "127.0.0.1:18302".into()),
        enforcer: std::env::var("FB_ENFORCER").unwrap_or_else(|_| "127.0.0.1:50051".into()),
        installed_tag: None,
        ..Default::default()
    };
    let mgr = NodeManager::new(PathBuf::from(env("FB_APP_DIR")));
    let backups = known_backups(&recorded_backups(&mgr.app_dir), &[PathBuf::from(env("FB_BACKUP_DIR"))]);
    let report = run_checks(&mgr, &s, wallet, backups).await;
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
