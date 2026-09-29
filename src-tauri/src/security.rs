//! Settings > Security (v0.2.0): the app checks its own setup at launch and on demand. Each check
//! says what it found, why it matters and how to fix it; red ones also show on Home until fixed.
//! The checks only read: files, settings, the wallet's state, and TCP connects to this computer's
//! own network addresses. They never change anything.
//!
//! Also here: the allowlist for `rpc_call`, the screens' generic path to the node.

use crate::commands::ClientState;
use crate::node::{self, NodeManager, Settings};
use crate::wallet::WalletStatus;
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

// ---------------------------------------------------------------------------------------------
// rpc_call's allowlist
// ---------------------------------------------------------------------------------------------

/// What the screens may ask the node through `rpc_call`: every call they make today (a test finds them
/// in every file under src/: fbCall("…"), and rpc_call with a literal method), plus read-only calls.
/// Everything else is refused. The wallet-sensitive calls (walletpassphrase, encryptwallet, sethdseed,
/// dumpprivkey, dumpwallet, importprivkey, importwallet, backupwallet, signmessage,
/// signrawtransactionwithwallet, bumpfee, rescanblockchain, …) and `stop` go through their own Rust
/// commands, or nowhere. Names are matched exactly, as the node matches them.
pub const RPC_ALLOWED: &[&str] = &[
    // The screens' calls today. The credit actions sign with the wallet, so a locked wallet
    // refuses them until the unlock prompt (src/lib/wallet.ts) has asked for the passphrase.
    "getblockcount",
    "getblockheader",
    "getnetworkinfo",
    "gettransaction",
    "listmynotes",
    "mintnote",
    "transfernote",
    "redeemnote",
    "demandnote",
    "listhouses",
    "registerhouse",
    "attesthouse",
    "listpools",
    "listmylp",
    "swapnote",
    "createpool",
    "addpoolliquidity",
    "removepoolliquidity",
    "listmybills",
    "getnewbillpubkey",
    "issuebill",
    "endorsebill",
    "retirebill",
    "claimbillescrow",
    // The Deposit panel (a new deposit address; it can't move coins) and the balance card's
    // pending line.
    "getdepositaddress",
    "getwalletinfo",
    // Read-only: the chain and the node.
    "getbestblockhash",
    "getblock",
    "getblockchaininfo",
    "getblockhash",
    "getchaintips",
    "getconnectioncount",
    "getmempoolinfo",
    "getmempoolentry",
    "getrawmempool",
    "getrawtransaction",
    "gettxout",
    "decoderawtransaction",
    "decodescript",
    "estimatesmartfee",
    "validateaddress",
    "getaddressinfo",
    "uptime",
    "getgateinfo",
    "getmainchainblockcount",
    // Read-only: the wallet.
    "getbalance",
    "getunconfirmedbalance",
    "listtransactions",
    "listsinceblock",
    "listunspent",
    "listmywithdrawals",
    "getwithdrawal",
    // Read-only: FreeBank's houses, pools and bills.
    "gethouse",
    "getpool",
    "getbill",
    "listbills",
];

/// Refuse a method the screens may not call (see RPC_ALLOWED).
pub fn allow_rpc(method: &str) -> Result<(), String> {
    if RPC_ALLOWED.contains(&method) {
        Ok(())
    } else {
        Err(format!(
            "The app's screens can't use the node call \"{}\": it isn't on the app's list of allowed calls.",
            method.chars().take(64).collect::<String>()
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Info,
    Warn,
    Red,
}

/// One line of the Security page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    /// Stable, for the screens: wallet, rpc, zmq, p2p, backups, signature, files, stack.
    pub id: &'static str,
    pub level: Level,
    pub title: String,
    /// What was found and why it matters.
    pub detail: String,
    /// How to fix it; empty when there is nothing to do.
    pub fix: String,
    /// Files the screen can show in the file manager (`security_reveal`): unencrypted backups and
    /// moved-aside wallets.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

impl Check {
    fn new(id: &'static str, level: Level, title: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id,
            level,
            title: title.into(),
            detail: detail.into(),
            fix: fix.into(),
            files: Vec::new(),
        }
    }
}

/// "Stop, then Start" on the Node tab, for the node the app runs; a node another program runs is
/// restarted there.
const RESTART: &str = "then restart the node (the Node tab's Stop, then Start; a node another program started is restarted there)";

// ---- The wallet's passphrase ----

/// `status`: None when the app isn't connected to a node yet.
pub fn wallet_check(status: &Result<Option<WalletStatus>, String>) -> Check {
    const ID: &str = "wallet";
    match status {
        Ok(Some(s)) if s.encrypted => Check::new(
            ID,
            Level::Ok,
            "Your wallet has a passphrase",
            "Nobody can spend your coins without it, not even someone who copies the wallet file.",
            "",
        ),
        Ok(Some(_)) => Check::new(
            ID,
            Level::Red,
            "Your wallet has no passphrase",
            "Anyone who can use this computer, or who gets a copy of the wallet file, can spend your coins.",
            "Set a passphrase for your wallet in Settings. FreeBank then asks for it before each payment.",
        ),
        Ok(None) => Check::new(ID, Level::Info, "Wallet not checked yet", "FreeBank isn't connected to your node yet.", ""),
        Err(e) => Check::new(
            ID,
            Level::Info,
            "Wallet not checked",
            format!("Your node didn't answer ({}). FreeBank checks again when you press Check again.", e),
            "",
        ),
    }
}

// ---- freebank.conf ----

/// The lines of freebank.conf the checks read. freebankd (Core 0.16) reads `key=value` lines; `#`
/// starts a comment anywhere on a line, and the first of several values is the one it uses.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Conf {
    pub rpcbind: Vec<String>,
    pub rpcallowip: Vec<String>,
    /// zmqpub*= lines: (option, address)
    pub zmq: Vec<(String, String)>,
    pub listen: Option<bool>,
    /// An rpcpassword= or rpcauth= line: then the file holds a login.
    pub rpc_login: bool,
}

/// Core's InterpretBool: empty is true, else the leading number is.
fn conf_bool(v: &str) -> bool {
    if v.is_empty() {
        return true;
    }
    let digits: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    digits.parse::<i64>().map(|n| n != 0).unwrap_or(false)
}

pub fn parse_conf(text: &str) -> Conf {
    let mut c = Conf::default();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (line, ""),
        };
        match k {
            "rpcbind" => c.rpcbind.push(v.to_string()),
            "rpcallowip" => c.rpcallowip.push(v.to_string()),
            "rpcpassword" | "rpcauth" => c.rpc_login = true,
            "listen" if c.listen.is_none() => c.listen = Some(conf_bool(v)),
            "nolisten" if c.listen.is_none() => c.listen = Some(!conf_bool(v)),
            k if k.starts_with("zmqpub") => c.zmq.push((k.to_string(), v.to_string())),
            _ => {}
        }
    }
    c
}

/// The host of "host", "host:port", "[v6]:port" or a bare IPv6 address (Core's SplitHostPort).
pub fn host_part(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match s.matches(':').count() {
        1 => s.split(':').next().unwrap_or(s),
        _ => s,
    }
}

/// Is `host` this computer's loopback (127.0.0.0/8, ::1, localhost)?
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// A ZMQ endpoint that stays on this computer: ipc:// and inproc://, or tcp:// on loopback (or the
/// loopback interface by name).
pub fn zmq_is_local(endpoint: &str) -> bool {
    let e = endpoint.trim();
    if e.starts_with("ipc://") || e.starts_with("inproc://") {
        return true;
    }
    let Some(rest) = e.strip_prefix("tcp://") else {
        return false;
    };
    // tcp://host:port, tcp://[v6]:port, tcp://iface;host:port (the part after ';' is what binds)
    let bind = rest.rsplit(';').next().unwrap_or(rest);
    let host = match bind.rsplit_once(':') {
        Some((h, _)) => h,
        None => bind,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "lo" || host == "lo0" || is_loopback_host(host)
}

/// RPC: answering on a network address is red, and so is a freebank.conf that opens it.
/// freebankd (httpserver.cpp) binds 127.0.0.1 and ::1 unless rpcallowip= is set. With rpcallowip=
/// alone it binds every address; with rpcbind= too, the addresses rpcbind names.
pub fn rpc_check(conf: &Conf, conf_path: &Path, port: u16, answering: &[SocketAddr]) -> Check {
    const ID: &str = "rpc";
    let conf_opens = !conf.rpcallowip.is_empty()
        && (conf.rpcbind.is_empty() || conf.rpcbind.iter().any(|b| !is_loopback_host(host_part(b))));
    let lines = if conf.rpcallowip.is_empty() && conf.rpcbind.is_empty() {
        None
    } else {
        Some(format!("Remove the rpcallowip= and rpcbind= lines from {}, {}.", conf_path.display(), RESTART))
    };
    if !answering.is_empty() {
        return Check::new(
            ID,
            Level::Red,
            "RPC is open to your network",
            format!(
                "Port {} controls your node and its wallet, and it answers at {}. Unless a firewall blocks it, \
                 other computers can reach it and try to log in.",
                port,
                list_addrs(answering)
            ),
            lines.unwrap_or_else(|| {
                "Restart the node without -rpcallowip and -rpcbind options: it then answers on this computer only.".into()
            }),
        );
    }
    if conf_opens {
        return Check::new(
            ID,
            Level::Red,
            "freebank.conf opens RPC to your network",
            format!(
                "Its rpcallowip= line makes the node answer on {} when it starts. RPC controls your node and its \
                 wallet, so other computers could reach it and try to log in.",
                if conf.rpcbind.is_empty() { "every network address".to_string() } else { conf.rpcbind.join(", ") }
            ),
            lines.unwrap_or_default(),
        );
    }
    let ignored = if !conf.rpcbind.is_empty() && conf.rpcallowip.is_empty() {
        " (freebankd ignores the rpcbind= line in freebank.conf without an rpcallowip= line)"
    } else {
        ""
    };
    Check::new(
        ID,
        Level::Ok,
        "RPC answers on this computer only",
        format!(
            "Port {}, which controls your node and its wallet, is closed to other computers{}.",
            port, ignored
        ),
        "",
    )
}

pub fn zmq_check(conf: &Conf, conf_path: &Path) -> Check {
    const ID: &str = "zmq";
    if conf.zmq.is_empty() {
        return Check::new(ID, Level::Ok, "ZMQ is off", "Your node publishes no ZMQ feeds.", "");
    }
    let open: Vec<String> = conf
        .zmq
        .iter()
        .filter(|(_, a)| !zmq_is_local(a))
        .map(|(k, a)| format!("{}={}", k, a))
        .collect();
    if open.is_empty() {
        return Check::new(
            ID,
            Level::Ok,
            "ZMQ stays on this computer",
            "Your node's ZMQ feeds are bound to this computer only.",
            "",
        );
    }
    Check::new(
        ID,
        Level::Warn,
        "ZMQ is open to your network",
        format!(
            "{} publishes each block and transaction your node sees to anyone who connects. It can't touch \
             your coins, but it tells others when you pay.",
            open.join(", ")
        ),
        format!(
            "In {}, bind each ZMQ line to tcp://127.0.0.1:<port> (or remove it), {}.",
            conf_path.display(),
            RESTART
        ),
    )
}

pub fn p2p_check(conf: &Conf, port: u16) -> Check {
    if conf.listen == Some(false) {
        return Check::new(
            "p2p",
            Level::Info,
            "Your node takes no incoming peers",
            "freebank.conf says listen=0: your node connects out to other FreeBank nodes, and nothing connects in.",
            "",
        );
    }
    Check::new(
        "p2p",
        Level::Info,
        "The peer port is public, by design",
        format!(
            "Your node swaps blocks and transactions with other FreeBank nodes on port {}, on every network this \
             computer is on. That is how the network works; the port gives no access to your wallet.",
            port
        ),
        "",
    )
}

// ---- Wallet backups the app made ----

/// "Back up wallet" saves into Documents (or home) as FreeBank-wallet-<time>[-…].dat
/// (node/obliterate.rs); each backup is also listed in <app data>/backups.json.
pub const BACKUPS_FILE: &str = "backups.json";
const BACKUP_PREFIX: &str = "FreeBank-wallet-";

/// What a wallet file is, as far as its encryption goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalletFile {
    /// It has a master key record ("mkey"): its keys are encrypted with a passphrase.
    Encrypted,
    Unencrypted,
    /// Not a wallet (not a Berkeley DB or SQLite file).
    NotAWallet,
}

/// Is `path` a wallet, and is it encrypted? An encrypted Core wallet holds an "mkey" record (the
/// master key, encrypted with the passphrase); one without it keeps its keys in the clear.
pub fn wallet_file(path: &Path) -> std::io::Result<WalletFile> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 16];
    if f.read_exact(&mut head).is_err() {
        return Ok(WalletFile::NotAWallet);
    }
    let magic = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    let bdb = magic == 0x0005_3162 || magic == 0x6231_0500;
    if !bdb && &head != b"SQLite format 3\0" {
        return Ok(WalletFile::NotAWallet);
    }
    // The record's key: the string "mkey" as Core serialises it (length 4, then the letters).
    const MKEY: &[u8] = b"\x04mkey";
    let mut buf = vec![0u8; 1 << 20];
    let mut carry: Vec<u8> = head.to_vec();
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        carry.extend_from_slice(&buf[..n]);
        if carry.windows(MKEY.len()).any(|w| w == MKEY) {
            return Ok(WalletFile::Encrypted);
        }
        let keep = carry.len().saturating_sub(MKEY.len() - 1);
        carry.drain(..keep);
    }
    Ok(if carry.windows(MKEY.len()).any(|w| w == MKEY) { WalletFile::Encrypted } else { WalletFile::Unencrypted })
}

/// `wallet_file`, remembered per file while its size and modification time stay the same: the checks run
/// on each visit to Home, and a wallet backup can be many megabytes.
pub fn wallet_file_cached(path: &Path) -> std::io::Result<WalletFile> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Seen = HashMap<PathBuf, (u64, std::time::SystemTime, WalletFile)>;
    static SEEN: OnceLock<Mutex<Seen>> = OnceLock::new();
    let meta = std::fs::metadata(path)?;
    let (len, modified) = (meta.len(), meta.modified()?);
    let seen = SEEN.get_or_init(Default::default);
    if let Some((l, m, w)) = seen.lock().unwrap().get(path) {
        if *l == len && *m == modified {
            return Ok(*w);
        }
    }
    let w = wallet_file(path)?;
    seen.lock().unwrap().insert(path.to_path_buf(), (len, modified, w));
    Ok(w)
}

/// The backups listed in <app data>/backups.json.
pub fn recorded_backups(app_dir: &Path) -> Vec<PathBuf> {
    std::fs::read(app_dir.join(BACKUPS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Add freshly saved backups to <app data>/backups.json (0600), so later sessions check them too.
/// Nothing is written once "Obliterate" has run.
pub fn record_backups(mgr: &NodeManager, saved: &[String]) {
    if mgr.still_here().is_err() || saved.is_empty() {
        return;
    }
    let mut all: Vec<String> = recorded_backups(&mgr.app_dir)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for s in saved {
        if !all.contains(s) {
            all.push(s.clone());
        }
    }
    let _ = write_private(&mgr.app_dir.join(BACKUPS_FILE), &serde_json::to_vec_pretty(&all).unwrap_or_default());
}

/// Write a file only its owner can read (0600 on Unix), through a temporary file and a rename.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// FreeBank-wallet-*.dat files in `folder`: the backups "Back up wallet" saved there, this session or
/// an earlier one (v0.1.1 didn't list them anywhere).
pub fn backups_in(folder: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with(BACKUP_PREFIX) && n.ends_with(".dat")
        })
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found
}

/// Every backup the app knows it made that still exists, once each: listed in backups.json, or found
/// by name in the folders backups go to. (Obliterate's "Back up wallet" and Settings' backup both save
/// there, through node/obliterate.rs's backup_wallet.)
pub fn known_backups(recorded: &[PathBuf], folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let scanned: Vec<PathBuf> = folders.iter().flat_map(|f| backups_in(f)).collect();
    for p in recorded.iter().chain(scanned.iter()) {
        if !p.is_file() {
            continue;
        }
        let key = p.canonicalize().unwrap_or_else(|_| p.clone());
        if seen.insert(key) {
            all.push(p.clone());
        }
    }
    all
}

/// `backups`: each backup the app made and what it is, and each wallet a restore or setup moved aside
/// (`asides`, see `aside_wallets`). Red while any of them has no passphrase: its keys are in the clear.
pub fn backups_check(backups: &[(PathBuf, WalletFile)], asides: &[(PathBuf, WalletFile)]) -> Check {
    const ID: &str = "backups";
    let plain_in = |list: &[(PathBuf, WalletFile)]| -> Vec<String> {
        list.iter()
            .filter(|(_, w)| *w == WalletFile::Unencrypted)
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect()
    };
    let plain = plain_in(backups);
    let plain_aside = plain_in(asides);
    let sealed = backups.iter().filter(|(_, w)| *w == WalletFile::Encrypted).count();
    if plain.is_empty() && plain_aside.is_empty() {
        let mut detail = match sealed {
            0 => "FreeBank hasn't found any wallet backup it made on this computer that lacks a passphrase.".to_string(),
            1 => "The wallet backup FreeBank made on this computer needs your passphrase to use.".to_string(),
            n => format!("The {} wallet backups FreeBank made on this computer need your passphrase to use.", n),
        };
        match asides.iter().filter(|(_, w)| *w == WalletFile::Encrypted).count() {
            0 => {}
            1 => detail.push_str(" The wallet FreeBank moved aside needs a passphrase too."),
            n => detail.push_str(&format!(" The {} wallets FreeBank moved aside need a passphrase too.", n)),
        }
        return Check::new(ID, Level::Ok, "No unencrypted wallet backups", detail, "");
    }
    let n = plain.len() + plain_aside.len();
    let them = |k: usize| if k == 1 { "it" } else { "them" };
    let title = match (plain.len(), plain_aside.len()) {
        (1, 0) => "An unencrypted wallet backup is on this computer".to_string(),
        (b, 0) => format!("{} unencrypted wallet backups are on this computer", b),
        (0, 1) => "An unencrypted wallet FreeBank moved aside is on this computer".to_string(),
        (0, a) => format!("{} unencrypted wallets FreeBank moved aside are on this computer", a),
        _ => format!("{} unencrypted wallet copies are on this computer", n),
    };
    let mut detail = Vec::new();
    if !plain.is_empty() {
        detail.push(format!(
            "FreeBank saved {} before your wallet had a passphrase.",
            if plain.len() == 1 { "this backup" } else { "these backups" }
        ));
    }
    if !plain_aside.is_empty() {
        detail.push(format!(
            "FreeBank moved {} aside when it set up or restored your wallet, and kept {} as it promised.",
            if plain_aside.len() == 1 { "this wallet" } else { "these wallets" },
            them(plain_aside.len())
        ));
    }
    detail.push(format!(
        "Anyone who can read {} can spend the coins its keys hold, even after you set a passphrase.",
        them(n)
    ));
    let fix = if plain_aside.is_empty() {
        format!(
            "Once you have a new backup made with your passphrase, delete {}, or move {} somewhere only you can \
             reach, such as an encrypted USB drive. FreeBank never deletes a backup by itself.",
            them(n),
            them(n)
        )
    } else {
        let mut fix = Vec::new();
        if !plain.is_empty() {
            fix.push(format!(
                "Once you have a new backup made with your passphrase, delete the unencrypted {}, or move {} \
                 somewhere only you can reach, such as an encrypted USB drive.",
                if plain.len() == 1 { "backup" } else { "backups" },
                them(plain.len())
            ));
        }
        let (w, k) = (if plain_aside.len() == 1 { "wallet" } else { "wallets" }, them(plain_aside.len()));
        fix.push(format!(
            "If the {} FreeBank moved aside may hold coins you still need, keep {} only somewhere like an \
             encrypted USB drive; otherwise delete {}. FreeBank never deletes {} by itself.",
            w, k, k, k
        ));
        fix.join(" ")
    };
    let mut c = Check::new(ID, Level::Red, title, detail.join(" "), fix);
    c.files = plain;
    c.files.extend(plain_aside);
    c
}

// ---- Wallets a restore or setup moved aside ----

/// The folders a restore or setup moved aside, once each: those Settings records (setup's moves of
/// an older data folder, and a restore's of the wallet it replaced, recovery/job.rs) and any named
/// <datadir>.old-<digits> beside the data folder, recorded or not.
pub fn aside_folders(s: &Settings) -> Vec<PathBuf> {
    let datadir = PathBuf::from(&s.datadir);
    let mut found: Vec<PathBuf> = s.moved_aside.iter().map(PathBuf::from).collect();
    if let (Some(parent), Some(name)) = (datadir.parent(), datadir.file_name()) {
        let prefix = format!("{}.old-", name.to_string_lossy());
        let mut named: Vec<PathBuf> = std::fs::read_dir(parent)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.strip_prefix(&prefix)
                    .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            })
            .map(|e| e.path())
            .collect();
        named.sort();
        found.extend(named);
    }
    let mut seen = std::collections::HashSet::new();
    found
        .into_iter()
        .filter(|p| p.is_dir())
        .filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())))
        .collect()
}

/// The wallet files in the folders a restore or setup moved aside (node::wallet_files finds them:
/// wallet.dat, wallets/, and named ones such as a restore's wallet.dat.old-<time>).
pub fn aside_wallets(s: &Settings) -> Vec<PathBuf> {
    aside_folders(s).iter().flat_map(|d| node::wallet_files(d)).collect()
}

/// The one of `known` that `want` names, compared as real paths: what "Show in folder" may open.
pub fn revealable(known: impl IntoIterator<Item = PathBuf>, want: &Path) -> Option<PathBuf> {
    let want = want.canonicalize().unwrap_or_else(|_| want.to_path_buf());
    known.into_iter().find(|p| p.canonicalize().map(|c| c == want).unwrap_or(false))
}

// ---- The node program's signature ----

/// Written into releases/<tag>/ once the archive has passed the signature and hash checks. The
/// panel asks the node manager, which also refuses to start a release without it.
#[cfg(test)]
pub const VERIFIED_MARK: &str = crate::node::install::VERIFIED;

pub fn signature_check(mgr: &NodeManager, installed_tag: Option<&str>) -> Check {
    const ID: &str = "signature";
    let Some(tag) = installed_tag else {
        return Check::new(
            ID,
            Level::Info,
            "FreeBank didn't install your node's program",
            "Your node runs a freebankd installed some other way, so the app can't vouch for it. FreeBank's \
             releases are at github.com/mbdrivechains/freebank, with checksums signed by the FreeBank release key.",
            "Check the signature of the release you run, as the FreeBank guide's \"Verify your download\" shows.",
        );
    };
    let bin = mgr.freebankd(tag);
    if !bin.is_file() {
        return Check::new(
            ID,
            Level::Info,
            "The node's program is missing",
            format!("freebankd {} isn't in {} any more.", tag, mgr.release_dir(tag).display()),
            "Set FreeBank up again: it downloads the program and checks its signature.",
        );
    }
    if mgr.verified(tag) {
        return Check::new(
            ID,
            Level::Ok,
            "Your node's program passed the signature check",
            format!(
                "FreeBank checked freebankd {} against the FreeBank release key and its signed checksums before \
                 installing it.",
                tag
            ),
            "",
        );
    }
    Check::new(
        ID,
        Level::Red,
        "Your node's program wasn't signature-checked",
        format!(
            "freebankd {} in {} was installed before the app checked release signatures, so FreeBank can't vouch \
             for it and won't start it.",
            tag,
            mgr.release_dir(tag).display()
        ),
        "Download it again on the Node tab, or set FreeBank up again: either way FreeBank checks the signature \
         first. Your wallet and data folder stay.",
    )
}

// ---- Who else can read the keys on disk ----

/// Can another account on this computer read `path` (or list it, for a folder)? Its mode must let
/// the group or everyone read it, and every folder above must let them through: a private folder
/// anywhere above keeps it private. None for a private or missing path.
#[cfg(unix)]
pub fn readable_by_others(path: &Path) -> Option<&'static str> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    let (mut other, mut group) = (mode & 0o004 != 0, mode & 0o040 != 0);
    if !other && !group {
        return None;
    }
    let full = path.canonicalize().ok()?;
    for dir in full.ancestors().skip(1) {
        let m = std::fs::metadata(dir).ok()?.permissions().mode();
        other &= m & 0o001 != 0;
        // A group member passes with the folder's group bit, or with everyone's.
        group &= m & 0o011 != 0;
        if !other && !group {
            return None;
        }
    }
    Some(if other { "every account" } else { "your group" })
}

#[cfg(not(unix))]
pub fn readable_by_others(_path: &Path) -> Option<&'static str> {
    None
}

/// A secret on disk and the folder to make private to protect it.
#[derive(Debug, Clone)]
pub struct Secret {
    pub path: PathBuf,
    /// "chmod 700" this folder (or "chmod 600" the file when None).
    pub folder: Option<PathBuf>,
}

/// The files that hold keys or logins: the app's phone keys (<app data>/phone/ and its files), the
/// app seed (<app data>/wallet/seed.enc, when there is one), and in the node's data folder its RPC
/// cookie, its wallets, and freebank.conf when it holds a login (rpcpassword= or rpcauth=).
pub fn secrets(app_dir: &Path, datadir: &Path, conf: &Conf) -> Vec<Secret> {
    let mut out = Vec::new();
    let phone = app_dir.join("phone");
    if phone.is_dir() {
        out.push(Secret { path: phone.clone(), folder: Some(phone.clone()) });
        let mut files: Vec<PathBuf> = std::fs::read_dir(&phone)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        out.extend(files.into_iter().map(|path| Secret { path, folder: Some(phone.clone()) }));
    }
    let seed = app_dir.join("wallet").join("seed.enc");
    if seed.is_file() {
        out.push(Secret { path: seed, folder: Some(app_dir.join("wallet")) });
    }
    if datadir.is_dir() {
        let d = Some(datadir.to_path_buf());
        let cookie = datadir.join(".cookie");
        if cookie.is_file() {
            out.push(Secret { path: cookie, folder: d.clone() });
        }
        if conf.rpc_login {
            out.push(Secret { path: datadir.join("freebank.conf"), folder: d.clone() });
        }
        out.extend(node::wallet_files(datadir).into_iter().map(|path| Secret { path, folder: d.clone() }));
    }
    out
}

/// `exposed`: each secret others can read, with who ("every account" / "your group").
pub fn files_check(exposed: &[(Secret, &'static str)]) -> Check {
    const ID: &str = "files";
    if exposed.is_empty() {
        return Check::new(
            ID,
            Level::Ok,
            "Your keys and logins on disk are private",
            "Only your account can read the node's login cookie and wallet, and the app's phone keys and wallet seed.",
            "",
        );
    }
    let names: Vec<String> = exposed.iter().map(|(s, _)| s.path.to_string_lossy().into_owned()).collect();
    let everyone = exposed.iter().any(|(_, who)| *who == "every account");
    let mut fixes: Vec<String> = Vec::new();
    for (s, _) in exposed {
        let cmd = match &s.folder {
            Some(f) => format!("chmod 700 '{}'", f.display()),
            None => format!("chmod 600 '{}'", s.path.display()),
        };
        if !fixes.contains(&cmd) {
            fixes.push(cmd);
        }
    }
    Check::new(
        ID,
        Level::Warn,
        "Other accounts on this computer can read some of your keys",
        format!(
            "{} can read {}. These hold keys or logins for your node and wallet.",
            if everyone { "Every account on this computer" } else { "Accounts in your group" },
            names.join(", ")
        ),
        format!("In a terminal, run: {}", fixes.join("; ")),
    )
}

// ---- The eCash side: the enforcer and the eCash node ----

/// The eCash node (REST; Core serves REST on its RPC port) and the enforcer (gRPC), as the app
/// reaches them. On another computer: an info line. On this one but answering on a network
/// address as well: a warning, since the enforcer's wallet and the eCash node's RPC must not be
/// reachable by strangers. They are the stack's, not the app's, so the fix is theirs.
pub fn stack_check(rest: &str, enforcer: &str, rest_open: &[SocketAddr], enforcer_open: &[SocketAddr]) -> Check {
    const ID: &str = "stack";
    let remote: Vec<String> = [("The eCash node", rest), ("The enforcer", enforcer)]
        .iter()
        .filter(|(_, a)| !is_loopback_host(host_part(a)))
        .map(|(n, a)| format!("{} is at {}", n, a))
        .collect();
    if !enforcer_open.is_empty() || !rest_open.is_empty() {
        let mut found = Vec::new();
        if !enforcer_open.is_empty() {
            found.push(format!("the enforcer's gRPC port answers at {}", list_addrs(enforcer_open)));
        }
        if !rest_open.is_empty() {
            found.push(format!("the eCash node's RPC port answers at {}", list_addrs(rest_open)));
        }
        return Check::new(
            ID,
            Level::Warn,
            "The eCash side answers on your network",
            format!(
                "On this computer, {}. They belong to the eCash side (BitWindow runs them), not to FreeBank. The \
                 enforcer's wallet and the eCash node's controls must not be reachable by strangers.",
                found.join(", and ")
            ),
            "Bind them to 127.0.0.1 (the enforcer's --serve-grpc-addr, the eCash node's rpcbind), or block those \
             ports in your firewall.",
        );
    }
    if !remote.is_empty() {
        let (its, those) = if remote.len() == 1 { ("Its port belongs", "that port") } else { ("Their ports belong", "those ports") };
        return Check::new(
            ID,
            Level::Info,
            "The eCash side runs on another computer",
            format!(
                "{}. {} to the eCash side, not to FreeBank, and must not be reachable by strangers: the enforcer's \
                 gRPC controls its wallet.",
                remote.join("; "),
                its
            ),
            format!("Keep {} behind a firewall, or on a private network such as Tailscale.", those),
        );
    }
    Check::new(
        ID,
        Level::Ok,
        "The eCash side stays on this computer",
        format!(
            "FreeBank reaches the eCash node at {} and the enforcer at {}, and neither answers on your network.",
            rest, enforcer
        ),
        "",
    )
}

// ---- Network probes ----

fn list_addrs(a: &[SocketAddr]) -> String {
    a.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
}

/// This computer's own addresses other than loopback: the ones other computers could use to reach
/// it. IPv6 link-local addresses are left out (dialling them needs an interface).
#[cfg(unix)]
pub fn network_addresses() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    // SAFETY: getifaddrs hands back a list we only read, then free with freeifaddrs.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut p = ifap;
        while !p.is_null() {
            let ifa = &*p;
            if !ifa.ifa_addr.is_null() {
                match (*ifa.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                        out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr))));
                    }
                    libc::AF_INET6 => {
                        let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                        out.push(IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    let mut keep: Vec<IpAddr> = Vec::new();
    for ip in out {
        let link_local = matches!(ip, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
        if !ip.is_loopback() && !ip.is_unspecified() && !link_local && !keep.contains(&ip) {
            keep.push(ip);
        }
    }
    keep
}

#[cfg(not(unix))]
pub fn network_addresses() -> Vec<IpAddr> {
    Vec::new()
}

/// Which of `addrs` accept a TCP connection on `port`, tried together, each for at most `wait`.
/// A connect to this computer's own address goes nowhere near the network, so it shows what a
/// program is bound to, not what a firewall lets through.
pub async fn answering(addrs: &[IpAddr], port: u16, wait: Duration) -> Vec<SocketAddr> {
    if port == 0 {
        return Vec::new();
    }
    let tries = addrs.iter().map(|ip| {
        let sa = SocketAddr::new(*ip, port);
        async move {
            match tokio::time::timeout(wait, tokio::net::TcpStream::connect(sa)).await {
                Ok(Ok(_)) => Some(sa),
                _ => None,
            }
        }
    });
    futures_util::future::join_all(tries).await.into_iter().flatten().collect()
}

/// A node error without its "RPC error <code>: " prefix, for a sentence.
fn plain_error(e: &str) -> String {
    let e = e.trim();
    match e.strip_prefix("RPC error") {
        Some(rest) => rest.split_once(": ").map(|(_, m)| m).unwrap_or(rest).trim().to_string(),
        None => e.to_string(),
    }
}

/// The port of a "host:port" endpoint, if it has one.
fn port_of(endpoint: &str) -> Option<u16> {
    endpoint.rsplit_once(':').and_then(|(_, p)| p.parse().ok())
}

// ---------------------------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------------------------

type Mgr = Arc<NodeManager>;

/// Where "Back up wallet" saves: Documents, else the home folder (node/commands.rs, wallet_backup).
fn backup_folders(app: &AppHandle) -> Vec<PathBuf> {
    let path = app.path();
    path.document_dir()
        .ok()
        .filter(|d| d.is_dir())
        .or_else(|| path.home_dir().ok())
        .into_iter()
        .collect()
}

fn all_backups(app: &AppHandle, mgr: &NodeManager) -> Vec<PathBuf> {
    known_backups(&recorded_backups(&mgr.app_dir), &backup_folders(app))
}

/// Every check, worst first. `wallet`: the wallet's state from the node the app is connected to.
pub async fn run_checks(
    mgr: &NodeManager,
    s: &Settings,
    wallet: Result<Option<WalletStatus>, String>,
    backups: Vec<PathBuf>,
) -> Vec<Check> {
    let datadir = PathBuf::from(&s.datadir);
    let conf_path = datadir.join("freebank.conf");
    let conf = parse_conf(&std::fs::read_to_string(&conf_path).unwrap_or_default());

    // Connects to this computer's network addresses: the node's RPC, the enforcer and the eCash
    // node's port when they are on this computer (elsewhere, they aren't ours to probe).
    let addrs = network_addresses();
    let wait = Duration::from_millis(600);
    let local_port = |e: &str| if is_loopback_host(host_part(e)) { port_of(e).unwrap_or(0) } else { 0 };
    let (rpc_open, enforcer_open, rest_open) = tokio::join!(
        answering(&addrs, s.rpc_port, wait),
        answering(&addrs, local_port(&s.enforcer), wait),
        answering(&addrs, local_port(&s.rest), wait),
    );

    let app_dir = mgr.app_dir.clone();
    let conf2 = conf.clone();
    let s2 = s.clone();
    let (scanned, scanned_asides, exposed) = tokio::task::spawn_blocking(move || {
        let scan = |list: Vec<PathBuf>| -> Vec<(PathBuf, WalletFile)> {
            list.into_iter()
                .filter_map(|p| wallet_file_cached(&p).ok().map(|w| (p, w)))
                .filter(|(_, w)| *w != WalletFile::NotAWallet)
                .collect()
        };
        let scanned = scan(backups);
        let scanned_asides = scan(aside_wallets(&s2));
        let exposed: Vec<(Secret, &'static str)> = secrets(&app_dir, &datadir, &conf2)
            .into_iter()
            .filter_map(|s| readable_by_others(&s.path).map(|who| (s, who)))
            .collect();
        (scanned, scanned_asides, exposed)
    })
    .await
    .unwrap_or_default();

    let mut checks = vec![
        wallet_check(&wallet),
        rpc_check(&conf, &conf_path, s.rpc_port, &rpc_open),
        zmq_check(&conf, &conf_path),
        p2p_check(&conf, s.p2p_port),
        backups_check(&scanned, &scanned_asides),
        signature_check(mgr, s.installed_tag.as_deref()),
        files_check(&exposed),
        stack_check(&s.rest, &s.enforcer, &rest_open, &enforcer_open),
    ];
    // Worst first; the order above within a level.
    checks.sort_by(|a, b| b.level.cmp(&a.level));
    checks
}

/// Settings > Security, and Home's red items: every check, worst first.
#[tauri::command]
pub async fn security_check(
    app: AppHandle,
    mgr: State<'_, Mgr>,
    client: State<'_, ClientState>,
) -> Result<Vec<Check>, String> {
    let s = mgr.settings.lock().await.clone();
    let wallet = {
        let mut c = client.lock().await;
        if !c.is_configured() {
            Ok(None)
        } else {
            match tokio::time::timeout(Duration::from_secs(10), crate::wallet::status(&mut c)).await {
                Ok(r) => r.map(Some).map_err(|e| plain_error(&e)),
                Err(_) => Err("it took too long".into()),
            }
        }
    };
    let backups = all_backups(&app, &mgr);
    Ok(run_checks(&mgr, &s, wallet, backups).await)
}

/// "Show in folder" for an unencrypted backup or moved-aside wallet: opens the folder it is in (and
/// selects it on a Mac). Only a backup the app knows it made, or a wallet in a folder a restore or
/// setup moved aside, is shown; any other path is refused.
#[tauri::command]
pub async fn security_reveal(app: AppHandle, mgr: State<'_, Mgr>, path: String) -> Result<(), String> {
    let s = mgr.settings.lock().await.clone();
    let known = revealable(all_backups(&app, &mgr).into_iter().chain(aside_wallets(&s)), Path::new(&path))
        .ok_or("FreeBank shows only the wallet backups it made and the wallets it moved aside.")?;
    reveal(&known)
}

fn reveal(file: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(file);
        c
    } else {
        let folder = file.parent().ok_or("That backup has no folder.")?;
        let mut c = std::process::Command::new("xdg-open");
        c.arg(folder);
        c
    };
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't open the file manager: {}", e))?;
    // Reaped in the background so it doesn't linger.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests;
