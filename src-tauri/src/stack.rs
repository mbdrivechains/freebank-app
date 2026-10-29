//! The eCash node and enforcer the app installs and runs itself for its network (v0.5.0; easynet first). Before, the
//! app needed a stack already running, usually BitWindow's; it still uses one when the person picks it.
//!
//! - The programs come from the network's release (network.rs `Stack`), each archive checked against the sha256
//!   pinned in the app before anything is unpacked, into `stack/<tag>/` in the network's app folder.
//! - Their data goes in `stack-data/` there (`node/`, `enforcer/`); the node's own config file is written by the app,
//!   so an eCash node this computer already runs is never touched.
//! - A port plan: what listens is looked at first. The app's own defaults avoid every other network's well-known
//!   ports; a port held by a program the app didn't start makes it take the next free one, and the screen names
//!   that program (`holder`).
//! - Both run in sessions of their own, so "Keep running" can leave them up; otherwise they stop as the app closes.
//!   A stack left running is used again at the next launch (its pid files, checked against the programs' paths).
//! - Beta's (v0.5.1) is the whole chain, about 890 GB. It starts from a snapshot when one answers (network.rs
//!   `Snapshot`: Ava Chow's at 965,000, else hers at 935,000), else syncs from the start. Its blocks may go on another
//!   disk (`blocksdir=`). The free space is shown before and while it runs; the node stops itself when under `LOW_DISK` is
//!   left; and "Stop and delete" (`remove`) ends an install or a sync at any point and deletes what it wrote.

use crate::network::{self, Snapshot, Stack};
use crate::node::{install, NodeManager, Settings};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ports {
    pub p2p: u16,
    pub rpc: u16,
    pub zmq: u16,
    pub grpc: u16,
    pub erpc: u16,
}

/// The app's own stack, in Settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StackSettings {
    /// The release installed, e.g. "easynet-3.1".
    pub tag: String,
    /// Its data folder (node/ and enforcer/ inside).
    pub dir: String,
    pub ports: Ports,
    /// A folder of their own for the node's blocks (`blocksdir=`: a bigger disk), chosen at install (v0.5.1).
    pub blocks: Option<String>,
    /// The snapshot chosen and not loaded yet, by id (v0.5.1). The install carries on from it.
    pub snapshot: Option<String>,
    /// How the node started: a snapshot's id, or "full" for a sync from the start (v0.5.1; Settings says it).
    pub start: Option<String>,
}

impl StackSettings {
    /// The FreeBank node is pointed at this stack (Advanced can point it at another one instead).
    pub fn in_use(&self, s: &Settings) -> bool {
        crate::node::detect::normalize_endpoint(&s.rest) == format!("127.0.0.1:{}", self.ports.rpc)
            && crate::node::detect::normalize_endpoint(&s.enforcer) == format!("127.0.0.1:{}", self.ports.grpc)
    }

    pub fn node_dir(&self) -> PathBuf {
        Path::new(&self.dir).join("node")
    }
    /// The folder the node keeps this network's files in (its cookie, debug.log, wallets): on main, its data folder.
    pub fn node_files(&self, s: &Stack) -> PathBuf {
        if s.folder.is_empty() {
            self.node_dir()
        } else {
            self.node_dir().join(s.folder)
        }
    }
    /// The disk its blocks fill: their own folder, or the node's.
    pub fn blocks_at(&self, s: &Stack) -> PathBuf {
        self.blocks.as_ref().map(PathBuf::from).unwrap_or_else(|| self.node_files(s))
    }
    pub fn enforcer_dir(&self) -> PathBuf {
        Path::new(&self.dir).join("enforcer")
    }
}

/// Every port another stack on this computer may use by default: BitWindow's beta node and enforcer, FreeBank on beta
/// and mainnet, easynet's installer (its node, ZMQ and enforcer). The app's own stacks keep off them.
/// From v0.5.1 also the eCash node's own defaults (8532, 8533 and its Tor port) and the app's beta and easynet stacks,
/// which may run at once.
pub const WELL_KNOWN: &[u16] = &[
    8122, 8454, 8455, 8532, 8533, 8534, 18301, 18302, 18303, 28532, 28533, 28534, 28535, 28454, 28455, 28551, 28552,
    38532, 38533, 38534, 38535, 38551, 38552, 50051,
];

/// The ports the app's stack for `s` will use: its defaults, each moved to the next free port when `busy` says another
/// program holds it (and never onto a well-known one, or one already taken by this plan). The node's P2P+1 is left
/// free too: it is the node's own Tor port when it binds P2P without `bind=`.
pub fn plan(s: &Stack, busy: impl Fn(u16) -> bool, ours: Option<&Ports>) -> Ports {
    let mut taken: Vec<u16> = Vec::new();
    let mut pick = |want: u16, pair: bool| -> u16 {
        let mut p = want;
        loop {
            let held = |q: u16| taken.contains(&q) || (busy(q) && !ours.is_some_and(|o| is_ours(o, q)));
            let clear = !held(p) && !(pair && held(p + 1)) && (p == want || !WELL_KNOWN.contains(&p))
                && !(pair && p != want && WELL_KNOWN.contains(&(p + 1)));
            if clear {
                taken.push(p);
                if pair {
                    taken.push(p + 1);
                }
                return p;
            }
            p = p.checked_add(if pair { 2 } else { 1 }).unwrap_or(40000);
        }
    };
    let p2p = pick(s.p2p, true);
    let rpc = pick(s.rpc, false);
    let zmq = pick(s.zmq, false);
    let grpc = pick(s.grpc, false);
    let erpc = pick(s.erpc, false);
    Ports { p2p, rpc, zmq, grpc, erpc }
}

fn is_ours(o: &Ports, p: u16) -> bool {
    [o.p2p, o.p2p + 1, o.rpc, o.zmq, o.grpc, o.erpc].contains(&p)
}

/// "x86_64-linux-gnu" or "universal-apple-darwin": the platform in the release's archive names, and its pinned hash.
fn platform(d: &network::Download) -> Result<(&'static str, &'static str), String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("x86_64-linux-gnu", d.linux))
    } else if cfg!(target_os = "macos") {
        Ok(("universal-apple-darwin", d.mac))
    } else {
        Err("The app installs the eCash node on Linux x86_64 and Macs only.".into())
    }
}

/// The app's copy of the stack's programs.
pub fn programs(mgr: &NodeManager, tag: &str) -> PathBuf {
    mgr.app_dir.join("stack").join(tag)
}
fn bitcoind(mgr: &NodeManager, tag: &str) -> PathBuf {
    programs(mgr, tag).join("bitcoind")
}
fn enforcer_bin(mgr: &NodeManager, tag: &str) -> PathBuf {
    programs(mgr, tag).join("bip300301_enforcer")
}
fn installed(mgr: &NodeManager, tag: &str) -> bool {
    let p = programs(mgr, tag);
    p.join(install::VERIFIED).is_file() && p.join("bitcoind").is_file() && p.join("bip300301_enforcer").is_file()
}

/// The node's config, written by the app: this network's section only, RPC and ZMQ on this computer only, REST on (the
/// FreeBank node reads deposits from blocks over it), the transaction index where the network has it, no Tor listener,
/// and the blocks' own folder when one was chosen.
pub fn node_conf(s: &Stack, st: &StackSettings) -> String {
    let p = &st.ports;
    let seeds: String = s.seeds.iter().map(|a| format!("addnode={}\n", a)).collect();
    let blocks = st.blocks.as_ref().map(|b| format!("blocksdir={}\n", b)).unwrap_or_default();
    format!(
        "# Written by the FreeBank app for its own {sec} node. Other eCash nodes on this computer keep their own files.\n\
         {blocks}[{sec}]\nserver=1\n{txindex}rest=1\nlistenonion=0\nrpcbind=127.0.0.1\nrpcallowip=127.0.0.1\nrpcport={rpc}\n\
         port={p2p}\nzmqpubsequence=tcp://127.0.0.1:{zmq}\nfallbackfee=0.00001\n{seeds}",
        sec = s.section,
        txindex = if s.txindex { "txindex=1\n" } else { "" },
        rpc = p.rpc,
        p2p = p.p2p,
        zmq = p.zmq
    )
}

/// The enforcer's arguments.
pub fn enforcer_args(s: &Stack, st: &StackSettings) -> Vec<String> {
    let p = &st.ports;
    let mut a: Vec<String> = vec![
        format!("--data-dir={}", st.enforcer_dir().display()),
        format!("--node-rpc-addr=127.0.0.1:{}", p.rpc),
        format!("--node-rpc-cookie-path={}", st.node_files(s).join(".cookie").display()),
        format!("--node-zmq-addr-sequence=tcp://127.0.0.1:{}", p.zmq),
        format!("--serve-grpc-addr=127.0.0.1:{}", p.grpc),
        format!("--serve-rpc-addr=127.0.0.1:{}", p.erpc),
        "--bitcoin-core-skip-version-check".into(),
        format!("--log-directory={}", st.enforcer_dir().join("logs").display()),
        "--max-log-files=3".into(),
        "--max-log-file-size-mb=20".into(),
    ];
    a.extend(s.enforcer_args.iter().map(|x| x.to_string()));
    a
}

// ---- Who holds a port

/// A program listening on a port of this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub port: u16,
    pub pid: Option<u32>,
    /// Its program name, and a word on whose it is when that can be told ("BitWindow's", "easynet's installer").
    pub name: String,
}

/// The inodes of sockets listening on `port`, from /proc/net/tcp and tcp6 (state 0A).
#[cfg(target_os = "linux")]
fn listening_inodes(table: &str, port: u16) -> Vec<String> {
    table
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let local = f.get(1)?;
            let p = u16::from_str_radix(local.rsplit(':').next()?, 16).ok()?;
            (p == port && f.get(3) == Some(&"0A")).then(|| f.get(9).map(|s| s.to_string())).flatten()
        })
        .collect()
}

fn label(cmdline: &str) -> String {
    let prog = cmdline.split_whitespace().next().unwrap_or("");
    let name = Path::new(prog).file_name().and_then(|n| n.to_str()).unwrap_or(prog).to_string();
    let whose = if cmdline.to_ascii_lowercase().contains("bitwindow") {
        " (BitWindow's)"
    } else if cmdline.contains("/easynet/") {
        " (easynet's installer)"
    } else {
        ""
    };
    format!("{}{}", if name.is_empty() { "another program".into() } else { name }, whose)
}

/// Which program listens on `port`, if any: /proc on Linux, lsof on a Mac. A port that answers but whose program can't
/// be seen (another user's) is "another program".
pub fn holder(port: u16) -> Option<Holder> {
    if !crate::node::detect::port_busy(port) {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let mut inodes = Vec::new();
        for t in ["/proc/net/tcp", "/proc/net/tcp6"] {
            inodes.extend(listening_inodes(&std::fs::read_to_string(t).unwrap_or_default(), port));
        }
        let want: Vec<String> = inodes.iter().map(|i| format!("socket:[{}]", i)).collect();
        for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            let fds = std::fs::read_dir(e.path().join("fd"));
            let hit = fds.into_iter().flatten().flatten().any(|fd| {
                std::fs::read_link(fd.path()).ok().and_then(|l| l.to_str().map(String::from)).is_some_and(|l| want.contains(&l))
            });
            if hit {
                let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
                let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ");
                return Some(Holder { port, pid: Some(pid), name: label(&cmd) });
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(o) = std::process::Command::new("lsof").args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"]).output() {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(pid) = text.lines().find_map(|l| l.strip_prefix('p')).and_then(|p| p.parse::<u32>().ok()) {
                let cmd = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]).output();
                let cmd = cmd.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
                return Some(Holder { port, pid: Some(pid), name: label(&cmd) });
            }
        }
    }
    Some(Holder { port, pid: None, name: "another program".into() })
}

// ---- Running

/// Is `pid` running the program `bin`? (A pid file can outlive its process, and the pid be reused.) The kernel names a
/// process's program with every link resolved, and " (deleted)" after it once the file is replaced: so is `bin` here.
fn runs_bin(pid: u32, bin: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let want = bin.canonicalize().unwrap_or_else(|_| bin.to_path_buf());
        std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|e| {
            let e = e.to_string_lossy();
            let e = e.strip_suffix(" (deleted)").unwrap_or(&e);
            Path::new(e) == want || Path::new(e) == bin
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim_start().starts_with(&*bin.to_string_lossy()))
    }
}

fn read_pid(p: &Path) -> Option<u32> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn node_pid(st: &StackSettings, s: &Stack) -> Option<u32> {
    read_pid(&st.node_files(s).join("bitcoind.pid"))
}
fn enforcer_pid(st: &StackSettings) -> Option<u32> {
    read_pid(&st.enforcer_dir().join("enforcer.pid"))
}

/// A JSON-RPC call to the stack's node, with its cookie.
async fn node_call(st: &StackSettings, s: &Stack, method: &str) -> Result<serde_json::Value, String> {
    node_rpc(st, s, method, serde_json::json!([]), Some(Duration::from_secs(20))).await
}

/// `node_call` with parameters, and no time limit when `timeout` is None (loadtxoutset takes many minutes).
async fn node_rpc(
    st: &StackSettings,
    s: &Stack,
    method: &str,
    params: serde_json::Value,
    timeout: Option<Duration>,
) -> Result<serde_json::Value, String> {
    let cookie = std::fs::read_to_string(st.node_files(s).join(".cookie")).map_err(|_| "The eCash node isn't up yet.".to_string())?;
    let (user, pass) = cookie.trim().split_once(':').ok_or("The eCash node's cookie is damaged.")?;
    let mut b = reqwest::Client::builder().no_proxy();
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    let http = b.build().map_err(|e| e.to_string())?;
    let r: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/", st.ports.rpc))
        .basic_auth(user, Some(pass))
        .json(&serde_json::json!({"jsonrpc": "1.0", "id": "fb", "method": method, "params": params}))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    match r.get("error") {
        Some(e) if !e.is_null() => Err(e["message"].as_str().unwrap_or("error").to_string()),
        _ => Ok(r["result"].clone()),
    }
}

/// The enforcer's tip height (its Connect API answers JSON over plain HTTP: ValidatorService/GetChainTip).
async fn enforcer_tip(port: u16) -> Option<u64> {
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().ok()?;
    let v: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/cusf.mainchain.v1.ValidatorService/GetChainTip", port))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v["blockHeaderInfo"]["height"].as_u64()
}

/// Wait until the node has caught up with its peers and the enforcer has every block it has. A FreeBank node started
/// earlier drops the first headers its peer sends when it can't yet check them against the eCash chain, and asks again
/// only when the peer announces its next block (minutes on easynet). At most `secs`; with `while_moving` (an install
/// from a snapshot) for as long as the node or the enforcer still moves on (23 minutes on beta in our test), and `secs`
/// once neither does. A full sync takes days, so that install waits `secs` only. True once caught up.
async fn wait_synced(st: &StackSettings, s: &Stack, secs: u64, while_moving: bool, patient: bool) -> bool {
    let started = Instant::now();
    let mut until = started + Duration::from_secs(secs);
    let mut last: (u64, Option<u64>) = (0, None);
    loop {
        let info = node_call(st, s, "getblockchaininfo").await.ok();
        let peer_info = node_call(st, s, "getpeerinfo").await.ok();
        let peers = peer_info.as_ref().and_then(|v| v.as_array()).map_or(0, |a| a.len() as u64);
        // The height its peers had when they connected: until it has that, "blocks == headers" may only mean it hasn't
        // heard of the newer blocks yet.
        let peer_best = peer_info
            .as_ref()
            .and_then(|v| v.as_array())
            .and_then(|a| a.iter().filter_map(|p| p["startingheight"].as_u64()).max())
            .unwrap_or(0);
        let (blocks, headers) = info
            .as_ref()
            .map(|i| (i["blocks"].as_u64().unwrap_or(0), i["headers"].as_u64().unwrap_or(0)))
            .unwrap_or((0, 0));
        set(|p| {
            if p.running {
                p.blocks = Some(blocks);
                p.headers = Some(headers);
            }
        });
        let tip = enforcer_tip(st.ports.grpc).await;
        if peers > 0 && headers > 0 && blocks == headers && blocks >= peer_best && tip == Some(blocks) {
            return true;
        }
        if while_moving && (blocks, tip) != last {
            last = (blocks, tip);
            until = Instant::now() + Duration::from_secs(secs);
        }
        if cancelled().is_some() {
            return false;
        }
        // No peer in 30 seconds: this computer is offline, or the network's seed is down. Waiting longer only holds the
        // FreeBank node back (review finding 7), unless it stopped uncleanly (`patient`, `wait_caught_up`): then its
        // start waits, as a computer just switched on may take longer to reach its network.
        if Instant::now() > until || (!patient && peers == 0 && started.elapsed() > Duration::from_secs(30)) {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The app's own eCash node and enforcer, when the FreeBank node uses them: (its settings, its release).
async fn own_in_use(mgr: &NodeManager) -> Option<(StackSettings, &'static Stack)> {
    let s = mgr.settings.lock().await.clone();
    let st = s.stack.clone().filter(|st| st.in_use(&s))?;
    Some((st, network::current().stack?))
}

/// The FreeBank node replays its last blocks against the eCash view as it starts after an unclean stop, and a view still
/// behind marks good blocks invalid (2026-10-25: eleven hours of redialling the seed). So that start waits for the app's
/// own eCash node and enforcer to catch up with their peers, up to `secs`, however long the peers take to answer
/// (v0.6.8). True when they have, or when the app runs none of its own.
pub async fn wait_caught_up(mgr: &NodeManager, secs: u64) -> bool {
    match own_in_use(mgr).await {
        Some((st, s)) => wait_synced(&st, s, secs, false, true).await,
        None => true,
    }
}

/// Whether the app's own eCash node and enforcer have caught up with their peers, looked at once; None when the FreeBank
/// node doesn't use them.
pub async fn caught_up(mgr: &NodeManager) -> Option<bool> {
    let (st, s) = own_in_use(mgr).await?;
    Some(wait_synced(&st, s, 0, false, false).await)
}

fn spawn_detached(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()
}

/// What a plain start says while a chosen snapshot isn't loaded yet; the screens offer Continue on it.
pub const SNAPSHOT_PENDING: &str = "FreeBank's eCash node hasn't loaded its snapshot yet. Press Continue to carry on (the \
     download goes on from where it stopped), or Stop and delete.";

/// One stack operation at a time: two starts at once could each spawn an enforcer (review finding 5).
static OPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start the node and the enforcer if they aren't running; each one already up is left as it is.
pub async fn start(mgr: &NodeManager) -> Result<(), String> {
    // The install starts them itself, and holds OPS until it is done (on beta, maybe an hour).
    if installing() {
        return Err("The eCash node is being installed: it starts when the install gets there.".into());
    }
    let _one = OPS.lock().await;
    start_locked(mgr, Why::Hand).await
}

/// Who starts the stack.
#[derive(Clone, Copy, PartialEq)]
enum Why {
    /// A start by hand, or the FreeBank node's.
    Hand,
    /// The install's own start (Update, Continue), which may wait the whole first sync.
    Install,
    /// The watch's restart (`check_gone`): it doesn't wait for a sync, and gives up when a stop is asked meanwhile.
    Watch,
}

/// `start`, under OPS. Once both run, the watch expects them to (`check_gone`). A start that isn't the watch's clears
/// what the watch found before: it no longer applies (v0.6.1 review 4).
async fn start_locked(mgr: &NodeManager, why: Why) -> Result<(), String> {
    let r = start_inner(mgr, why).await;
    if r.is_ok() {
        let mut k = kept().lock().unwrap();
        k.want = true;
        if why != Why::Watch {
            k.stopped = None;
            k.restarted = [None, None];
        }
    }
    r
}

/// The watch's restart gives up once a stop was asked: `stop` clears `want` before it waits for OPS, which the restart
/// holds (v0.6.1 review 2: Stop, Stop and delete and quitting waited up to five minutes behind it).
fn check_watch(why: Why) -> Result<(), String> {
    if why == Why::Watch && !kept().lock().unwrap().want {
        return Err(STOP_ASKED.into());
    }
    Ok(())
}
const STOP_ASKED: &str = "A stop was asked meanwhile.";

/// Stops asked (by hand, at quit, by Stop and delete): the watch's restart compares it before and after, so one asked
/// meanwhile isn't followed by a "started it again" (v0.6.1 re-review 3).
static STOPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A stop is asked, before it waits for OPS: the watch, if it is starting them again, gives up (`check_watch`), and what
/// it found no longer applies (the user knows they are down).
fn stop_asked() {
    STOPS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let mut k = kept().lock().unwrap();
    k.want = false;
    k.stopped = None;
    k.restarted = [None, None];
}

async fn start_inner(mgr: &NodeManager, why: Why) -> Result<(), String> {
    let installing = why == Why::Install;
    let s = network::current().stack.ok_or("This network has no eCash node the app installs.")?;
    let mut st = mgr.settings.lock().await.stack.clone().ok_or("The app hasn't installed its eCash node.")?;
    if st.tag != s.tag {
        return Err(format!(
            "FreeBank has a newer {} node ({}): update it in Settings, Node & connection.",
            network::current().l1,
            s.tag
        ));
    }
    if !installed(mgr, &st.tag) {
        return Err("The app's eCash node isn't installed (or wasn't checked): install it again.".into());
    }
    // A snapshot chosen and not loaded yet: the install carries on from it (setup's Continue), not a plain start, which
    // would sync from the start meanwhile.
    if st.snapshot.is_some() && !installing {
        return Err(SNAPSHOT_PENDING.into());
    }
    let node_bin = bitcoind(mgr, &st.tag);
    let enf_bin = enforcer_bin(mgr, &st.tag);
    let node_up = node_pid(&st, s).is_some_and(|p| runs_bin(p, &node_bin));
    let enf_up = enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enf_bin));
    if !node_up {
        if let Some(why) = low_disk(&st, s) {
            return Err(why);
        }
    }
    *stopped_low().lock().unwrap() = None;
    // Another program took one of its ports since (BitWindow, easynet's installer…): with both of the app's own down,
    // its stack moves to free ports, as its install would have chosen, and the FreeBank node follows. Its config is
    // the app's own, so nothing else is changed.
    if !node_up && !enf_up {
        let held: Vec<Holder> = [st.ports.p2p, st.ports.rpc, st.ports.zmq, st.ports.grpc, st.ports.erpc]
            .into_iter()
            .filter_map(holder)
            .collect();
        if !held.is_empty() {
            let moved = plan(s, crate::node::detect::port_busy, None);
            let names: Vec<String> = held.iter().map(|h| format!("{} ({})", h.port, h.name)).collect();
            crate::activity::note(&format!("stack: ports in use by other programs: {}; moved to {:?}", names.join(", "), moved));
            st.ports = moved;
            let mut settings = mgr.settings.lock().await.clone();
            record_stack(&mut settings, s, &st, false);
            mgr.save_settings(settings).await?;
        }
    }
    let mut node_child: Option<std::process::Child> = None;
    if !node_up {
        for port in [st.ports.p2p, st.ports.rpc, st.ports.zmq] {
            if let Some(h) = holder(port) {
                return Err(format!("Port {} is in use by {}: the eCash node can't start.", port, h.name));
            }
        }
        std::fs::create_dir_all(st.node_dir()).map_err(|e| e.to_string())?;
        // Owner-only, as the node's own files: it holds no secret (the node logs in by its cookie), but nobody else
        // needs to read or change it.
        {
            use std::io::Write;
            let conf = st.node_dir().join("ecash.conf");
            let _ = std::fs::remove_file(&conf);
            let mut f = install::private_file(&conf).map_err(|e| e.to_string())?;
            f.write_all(node_conf(s, &st).as_bytes()).map_err(|e| e.to_string())?;
        }
        let mut c = std::process::Command::new(&node_bin);
        c.arg(format!("-datadir={}", st.node_dir().display())).arg(format!("-conf={}", st.node_dir().join("ecash.conf").display()));
        // Stop and delete may have begun meanwhile: no node is started under a folder about to go (review L1).
        check_cancel()?;
        check_watch(why)?;
        node_child = Some(spawn_detached(&mut c).map_err(|e| format!("Couldn't start the eCash node: {}", e))?);
        crate::activity::note("stack: eCash node started");
    }
    // The enforcer needs the node's RPC. A node with a big chain takes minutes to load its block index.
    let until = Instant::now() + Duration::from_secs(300);
    while node_call(&st, s, "getblockcount").await.is_err() {
        check_cancel()?;
        if let Err(e) = check_watch(why) {
            // The node it just started may not have written its pid file yet, so the stop could miss it: it goes here
            // (v0.6.1 re-review 2).
            if let Some(mut c) = node_child.take() {
                #[cfg(unix)]
                unsafe {
                    libc::kill(c.id() as libc::pid_t, libc::SIGTERM);
                }
                wait_gone(c.id(), &node_bin, 180).await;
                let _ = c.try_wait();
            }
            return Err(e);
        }
        // One that ends at its start (bad data, a bad config) says so now, not after five minutes (v0.6.1 review 2).
        if let Some(Ok(Some(status))) = node_child.as_mut().map(|c| c.try_wait()) {
            let words = last_words(&st.node_files(s).join("debug.log")).map(|w| format!(" {}", w)).unwrap_or_default();
            return Err(format!("The eCash node stopped as it started ({}).{}", status, words));
        }
        if Instant::now() > until {
            return Err(format!("The eCash node didn't answer within 5 minutes: see {}", st.node_files(s).join("debug.log").display()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // A snapshot first (v0.5.1), then the enforcer, which starts from the snapshot's blocks on.
    if let Some(x) = st.snapshot.clone().and_then(|id| pending_snapshot(s, &id)) {
        load_snapshot(mgr, &mut st, s, x).await?;
    }
    check_cancel()?;
    if !enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enf_bin)) {
        for port in [st.ports.grpc, st.ports.erpc] {
            if let Some(h) = holder(port) {
                return Err(format!("Port {} is in use by {}: the enforcer can't start. Stop that program, or stop and start FreeBank's eCash node to move it.", port, h.name));
            }
        }
        std::fs::create_dir_all(st.enforcer_dir().join("logs")).map_err(|e| e.to_string())?;
        let mut c = std::process::Command::new(&enf_bin);
        c.args(enforcer_args(s, &st));
        check_cancel()?;
        check_watch(why)?;
        let child = spawn_detached(&mut c).map_err(|e| format!("Couldn't start the enforcer: {}", e))?;
        std::fs::write(st.enforcer_dir().join("enforcer.pid"), child.id().to_string()).map_err(|e| e.to_string())?;
        crate::activity::note("stack: enforcer started");
    }
    // Both were up already: nothing to wait for (review finding 7). Nor for the watch's restart: the FreeBank node runs
    // and catches up as they do.
    if (node_up && enf_up) || why == Why::Watch {
        return Ok(());
    }
    set(|p| {
        if p.running {
            p.stage = "sync".into();
        }
    });
    let from_snapshot = st.start.as_deref().is_some_and(|x| x != "full");
    if !wait_synced(&st, s, if installing { 900 } else { 120 }, installing && from_snapshot, false).await {
        crate::activity::note("stack: not caught up yet; FreeBank's node starts anyway (unless it stopped uncleanly)");
    }
    Ok(())
}

/// Stop the enforcer, then the node (RPC `stop`, else SIGTERM), and wait for both. Only processes running the app's own
/// programs are signalled. During an install, that ends the install (`halt_install`): Continue carries on.
pub async fn stop(mgr: &NodeManager) -> Result<(), String> {
    if installing() {
        return halt_install(mgr, "Stopped. Continue carries on from where it was.").await;
    }
    stop_asked();
    let _one = OPS.lock().await;
    let Some(st) = mgr.settings.lock().await.stack.clone() else { return Ok(()) };
    stop_locked(mgr, &st).await
}

/// End an install under way, without waiting for OPS, which it holds: cancelled with `why`, its programs stopped (that
/// ends a header wait or a snapshot load at once), then a little time for it to give up.
async fn halt_install(mgr: &NodeManager, why: &str) -> Result<(), String> {
    cancel_install(why);
    let st = mgr.settings.lock().await.stack.clone();
    let r = match &st {
        Some(st) => stop_locked(mgr, st).await,
        None => Ok(()),
    };
    let until = Instant::now() + Duration::from_secs(30);
    while installing() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    r
}

/// `stop` for the stack `st` describes (its own release's programs), under OPS. An error if either is still running.
async fn stop_locked(mgr: &NodeManager, st: &StackSettings) -> Result<(), String> {
    // Every stop is meant: the watch no longer expects them up.
    kept().lock().unwrap().want = false;
    let Some(s) = network::current().stack else { return Ok(()) };
    let st = st.clone();
    let enf_bin = enforcer_bin(mgr, &st.tag);
    // Logged only when something was running: Stop and delete stops twice (the second catches a node started in
    // between), and said "stopped" twice.
    let mut stopped_any = false;
    if let Some(p) = enforcer_pid(&st).filter(|&p| runs_bin(p, &enf_bin)) {
        stopped_any = true;
        #[cfg(unix)]
        unsafe {
            libc::kill(p as libc::pid_t, libc::SIGTERM);
        }
        wait_gone(p, &enf_bin, 30).await;
    }
    let node_bin = bitcoind(mgr, &st.tag);
    if let Some(p) = node_pid(&st, s).filter(|&p| runs_bin(p, &node_bin)) {
        stopped_any = true;
        if node_call(&st, s, "stop").await.is_err() {
            #[cfg(unix)]
            unsafe {
                libc::kill(p as libc::pid_t, libc::SIGTERM);
            }
        }
        wait_gone(p, &node_bin, 180).await;
    }
    let still: Vec<&str> = [
        ("the eCash node", node_pid(&st, s).is_some_and(|p| runs_bin(p, &node_bin))),
        ("the enforcer", enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enf_bin))),
    ]
    .into_iter()
    .filter_map(|(n, up)| up.then_some(n))
    .collect();
    if !still.is_empty() {
        return Err(format!("{} didn't stop. Nothing else was changed.", still.join(" and ")));
    }
    if stopped_any {
        crate::activity::note("stack: stopped");
    }
    Ok(())
}

async fn wait_gone(pid: u32, bin: &Path, secs: u64) {
    let until = Instant::now() + Duration::from_secs(secs);
    while runs_bin(pid, bin) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// At the app's exit: the stack stops with it, unless "Keep running" keeps the FreeBank node (which needs it) up. An install
/// under way (on beta it can take an hour) holds OPS: it is cancelled and its programs stopped without waiting for it,
/// whatever "Keep running" says, and the next launch offers Continue (a download goes on from where it stopped).
pub async fn at_exit(mgr: &NodeManager) {
    let s = mgr.settings.lock().await.clone();
    if installing() {
        let _ = halt_install(mgr, "Stopped: the app closed. Continue carries on.").await;
        crate::activity::note("stack: the install stopped with the app");
        return;
    }
    // "Keep running" keeps the FreeBank node up, which needs the stack; a stack it doesn't read from (the user went on to
    // another computer's) has no reason to outlive the app (review M3), nor does one whose FreeBank node is down: "Stop
    // everything and close" stops that node first, and left the eCash node and enforcer running (found in v0.6.0's walk).
    if let Some(st) = &s.stack {
        if !st.in_use(&s) || !s.keep_running || !crate::node::process::child_alive(mgr).await {
            let _ = stop(mgr).await;
        }
    }
}

// ---- Disk, snapshots, Stop and delete (v0.5.1)

/// The node stops itself when less than this is free on the disk of its blocks or of its data: the user's disk is never
/// filled to the brim. Beta's (an 890 GB chain); a small chain like easynet's (2 GB) keeps `LOW_DISK_SMALL` (review L4).
pub const LOW_DISK: u64 = 20_000_000_000;
pub const LOW_DISK_SMALL: u64 = 2_000_000_000;

fn low_disk_floor(s: &Stack) -> u64 {
    if s.full_gb >= 100 {
        LOW_DISK
    } else {
        LOW_DISK_SMALL
    }
}

/// `d` is a chain's folder directly inside the app's own stack-data/: both real folders, not links, and `d` holds a
/// node's or an enforcer's folder. A v0.5.0 chain there carries no mark but is the app's. A stack-data/ that links to
/// another disk is not trusted this way, as what is there may not be FreeBank's (review H1); only the mark counts there.
pub fn in_own_data(app_dir: &Path, d: &Path) -> bool {
    let own = app_dir.join("stack-data");
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    d.parent() == Some(own.as_path())
        && real_dir(&own)
        && real_dir(d)
        && (real_dir(&d.join("node")) || real_dir(&d.join("enforcer")))
}

/// The mark in every folder this app created for its stack. Stop and delete, and Obliterate, delete only a folder that
/// carries it.
pub const MARK: &str = ".freebank-stack";

/// Free bytes for this user on the disk holding `path` (its nearest folder that exists).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
        let mut v: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut v) } != 0 {
            return None;
        }
        #[allow(clippy::unnecessary_cast)]
        Some(v.f_bavail as u64 * v.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The bytes of the files under `p`, links not followed.
pub fn tree_bytes(p: &Path) -> u64 {
    let Ok(m) = std::fs::symlink_metadata(p) else { return 0 };
    if m.is_dir() {
        std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| tree_bytes(&e.path())).sum()
    } else if m.is_file() {
        m.len()
    } else {
        0
    }
}

/// What the stack's folders take, measured at most once a minute (beta's are thousands of files).
fn used_bytes(st: &StackSettings) -> u64 {
    static USED: Mutex<Option<(Instant, String, u64)>> = Mutex::new(None);
    let key = format!("{}|{}", st.dir, st.blocks.as_deref().unwrap_or(""));
    if let Some((at, k, n)) = USED.lock().unwrap().clone() {
        if k == key && at.elapsed() < Duration::from_secs(60) {
            return n;
        }
    }
    let n = tree_bytes(Path::new(&st.dir)) + st.blocks.as_ref().map_or(0, |b| tree_bytes(Path::new(b)));
    *USED.lock().unwrap() = Some((Instant::now(), key, n));
    n
}

fn gb(n: u64) -> String {
    if n < 1_000_000_000 {
        return format!("{} MB", (n + 500_000) / 1_000_000);
    }
    format!("{} GB", (n + 500_000_000) / 1_000_000_000)
}

/// Why the node must not run now: under LOW_DISK free where its blocks or its data are. None while there is room.
fn low_disk(st: &StackSettings, s: &Stack) -> Option<String> {
    let mut at = vec![st.blocks_at(s)];
    if st.blocks.is_some() {
        at.push(PathBuf::from(&st.dir));
    }
    at.into_iter().find_map(|p| {
        let free = free_bytes(&p)?;
        let floor = low_disk_floor(s);
        (free < floor).then(|| {
            format!(
                "Only {} is free on the disk with {}. FreeBank keeps {} free, so its eCash node is stopped: free some \
                 space, or Stop and delete it.",
                gb(free),
                p.display(),
                gb(floor)
            )
        })
    })
}

/// Why the node was stopped for want of disk space (the watch), until it starts again.
fn stopped_low() -> &'static Mutex<Option<String>> {
    static L: Mutex<Option<String>> = Mutex::new(None);
    &L
}

/// What stopped on its own (v0.6.1): the watch found the app's own eCash node or enforcer gone while the app meant them
/// to run. On a Mac's first easynet run the eCash node stopped with the app open, and nothing said so or why.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StoppedOwn {
    /// "node", "enforcer" or "both".
    pub what: String,
    /// When the watch found it (Unix seconds).
    pub at: u64,
    /// The last error in its log before it stopped, else how its log ends.
    pub line: Option<String>,
    /// The watch started it again: it does that once an hour at most.
    pub restarted: bool,
    /// Why starting it again failed.
    pub restart_error: Option<String>,
}

/// The stack as the app left it, for the watch.
struct Kept {
    /// Started by the app (or found running when it started them): both should be up. Every stop clears it.
    want: bool,
    /// The watch's last restart of the eCash node and of the enforcer: a second stop of either within the hour waits for
    /// Start; the other's first stop is still started again (v0.6.1 review; one time for both let them take turns).
    restarted: [Option<Instant>; 2],
    /// Shown until the next start by hand.
    stopped: Option<StoppedOwn>,
}

fn kept() -> &'static Mutex<Kept> {
    static K: Mutex<Kept> = Mutex::new(Kept { want: false, restarted: [None, None], stopped: None });
    &K
}

/// Every minute, under the watch: a stack the app meant to run that has stopped on its own is noted with the last line
/// of its log, and started again, once an hour at most; a second stop waits for Start.
async fn check_gone(mgr: &NodeManager, st: &StackSettings, s: &Stack) {
    if !kept().lock().unwrap().want || installing() {
        return;
    }
    // A start or stop under way decides; look again next minute.
    let Ok(_one) = OPS.try_lock() else { return };
    let stops = STOPS.load(std::sync::atomic::Ordering::SeqCst);
    let node_up = node_pid(st, s).is_some_and(|p| runs_bin(p, &bitcoind(mgr, &st.tag)));
    let enf_up = enforcer_pid(st).is_some_and(|p| runs_bin(p, &enforcer_bin(mgr, &st.tag)));
    if (node_up && enf_up) || !kept().lock().unwrap().want {
        return;
    }
    let (what, name) = match (node_up, enf_up) {
        (false, false) => ("both", "the eCash node and the enforcer"),
        (false, true) => ("node", "the eCash node"),
        _ => ("enforcer", "the enforcer"),
    };
    let line = if !node_up {
        last_words(&st.node_files(s).join("debug.log"))
    } else {
        newest_file(&st.enforcer_dir().join("logs")).and_then(|p| last_words(&p))
    };
    // Which of the two this stop is about: [node, enforcer].
    let which = [!node_up, !enf_up];
    let again = {
        let k = kept().lock().unwrap();
        (0..2).any(|i| which[i] && k.restarted[i].is_some_and(|t| t.elapsed() < Duration::from_secs(3600)))
    };
    crate::activity::note(&format!(
        "stack: {} stopped on its own{}: {}",
        name,
        if again { " again" } else { "" },
        line.as_deref().unwrap_or("(nothing in its log)")
    ));
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut gone = StoppedOwn { what: what.into(), at, line, restarted: false, restart_error: None };
    if !again {
        {
            let mut k = kept().lock().unwrap();
            for i in (0..2).filter(|&i| which[i]) {
                k.restarted[i] = Some(Instant::now());
            }
        }
        match start_locked(mgr, Why::Watch).await {
            Ok(()) => {
                gone.restarted = true;
                crate::activity::note("stack: started again");
            }
            Err(e) => {
                crate::activity::note(&format!("stack: starting it again failed: {}", e));
                gone.restart_error = Some(e);
            }
        }
    }
    let mut k = kept().lock().unwrap();
    // A stop asked meanwhile (`stop_asked` cleared want and the notice): it stands, and nothing is shown.
    if STOPS.load(std::sync::atomic::Ordering::SeqCst) != stops || gone.restart_error.as_deref() == Some(STOP_ASKED) {
        return;
    }
    k.want = gone.restarted;
    k.stopped = Some(gone);
}

/// The watch's first look: a stack already running when the app opened, as "Keep running" leaves it (the FreeBank node
/// is then found running, so nothing starts the stack), is watched too, if the app uses it (v0.6.1 review 3).
async fn adopt(mgr: &NodeManager, st: &StackSettings, s: &Stack) {
    let wanted = kept().lock().unwrap().want;
    if wanted || installing() || !st.in_use(&*mgr.settings.lock().await) {
        return;
    }
    let Ok(_one) = OPS.try_lock() else { return };
    let node_up = node_pid(st, s).is_some_and(|p| runs_bin(p, &bitcoind(mgr, &st.tag)));
    let enf_up = enforcer_pid(st).is_some_and(|p| runs_bin(p, &enforcer_bin(mgr, &st.tag)));
    if node_up && enf_up {
        kept().lock().unwrap().want = true;
        crate::activity::note("stack: found running; the watch looks after it");
    }
}

/// The app's own eCash node's debug.log and the enforcer's newest log, for a report's recent activity (v0.6.1).
pub async fn report_logs(mgr: &NodeManager) -> Option<(PathBuf, Option<PathBuf>)> {
    let s = network::current().stack?;
    let st = mgr.settings.lock().await.stack.clone()?;
    Some((st.node_files(s).join("debug.log"), newest_file(&st.enforcer_dir().join("logs"))))
}

/// The newest file in `dir` (the enforcer's rotating logs).
fn newest_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.is_file())
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// What a program's log says about how it stopped: the last error before a "Shutdown in progress" (the eCash node's
/// orderly stop; "Shutdown: In progress" in older builds), else that it stopped normally; with no such line, the last
/// error in its end, else that it ended without a word (a program killed or crashing writes nothing; its last lines go
/// with a report). Without the time stamp; at most 300 characters.
fn last_words(log: &Path) -> Option<String> {
    let text = crate::node::process::read_since(log, 0, 16 * 1024)?;
    let all: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let lines = &all[all.len().saturating_sub(40)..];
    let stop = lines.iter().rposition(|l| {
        let x = l.to_ascii_lowercase();
        x.contains("shutdown in progress") || x.contains("shutdown: in progress")
    });
    let err = lines[..stop.unwrap_or(lines.len())].iter().rev().find(|l| {
        let x = l.to_ascii_lowercase();
        x.contains("error") || x.contains("fatal") || x.contains("panicked")
    });
    let pick = match (err, stop) {
        (Some(e), _) => e,
        (None, Some(_)) => return Some("It stopped normally, as if it had been asked to.".into()),
        (None, None) if !lines.is_empty() => {
            return Some("Its log ends with no error and no stop, as when a program is killed or crashes.".into())
        }
        (None, None) => return None,
    };
    Some(unstamped(pick).chars().take(300).collect())
}

/// A log line without its time stamp: "2026-10-21 05:45:20 " (FreeBank's node, older eCash builds), or
/// "2026-10-21T05:45:20Z " (eCash 31) and "2026-10-21T05:45:20.123456Z " (the enforcer).
fn unstamped(l: &str) -> &str {
    let b = l.as_bytes();
    if b.len() > 20 && b[4] == b'-' && b[7] == b'-' {
        if b[10] == b' ' && b[13] == b':' && b[19] == b' ' {
            return l[20..].trim_start();
        }
        if b[10] == b'T' {
            return l.split_once(char::is_whitespace).map_or(l, |(_, r)| r.trim_start());
        }
    }
    l
}

/// Cancel what the install is doing (Stop and delete, or the disk watch), with the reason it gives. Only an install
/// run's start and end clear it (review L2: when each canceller cleared it after itself, one clearing another's let
/// the install go on), and it counts only while an install runs.
fn cancel_slot() -> &'static Mutex<Option<String>> {
    static C: Mutex<Option<String>> = Mutex::new(None);
    &C
}
/// The first reason stands.
fn cancel_install(why: &str) {
    cancel_slot().lock().unwrap().get_or_insert_with(|| why.to_string());
}
fn cancelled() -> Option<String> {
    if !installing() {
        return None;
    }
    cancel_slot().lock().unwrap().clone()
}
fn check_cancel() -> Result<(), String> {
    cancelled().map_or(Ok(()), Err)
}

/// A snapshot as the setup screen offers it.
#[derive(Debug, Clone, Serialize)]
pub struct Offer {
    pub id: &'static str,
    pub source: &'static str,
    pub height: u64,
    pub size: u64,
    /// It answered just now, with its size: the screen offers only those.
    pub available: bool,
}

/// The network's snapshots, each asked (HEAD) whether it is there with its size, at most every ten minutes. A snapshot
/// is looked for first: the first available is the one setup picks.
async fn offers(http: &reqwest::Client, s: &Stack) -> Vec<Offer> {
    static SEEN: Mutex<Option<(Instant, Vec<Offer>)>> = Mutex::new(None);
    if s.snapshots.is_empty() {
        return Vec::new();
    }
    if let Some((at, v)) = SEEN.lock().unwrap().clone() {
        if at.elapsed() < Duration::from_secs(600) && v.len() == s.snapshots.len() && v.iter().zip(s.snapshots).all(|(a, b)| a.id == b.id) {
            return v;
        }
    }
    let mut out = Vec::new();
    for x in s.snapshots {
        let r = http.head(x.url).timeout(Duration::from_secs(10)).send().await;
        // The header itself: a HEAD answer has no body, so reqwest's content_length() says 0.
        let size = |r: &reqwest::Response| {
            r.headers().get(reqwest::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok())
        };
        let available = r.is_ok_and(|r| r.status().is_success() && size(&r) == Some(x.size));
        out.push(Offer { id: x.id, source: x.source, height: x.height, size: x.size, available });
    }
    *SEEN.lock().unwrap() = Some((Instant::now(), out.clone()));
    out
}

fn snapshot_by_id(s: &Stack, id: &str) -> Option<&'static Snapshot> {
    s.snapshots.iter().find(|x| x.id == id)
}

/// The snapshot to carry on with, chosen and not loaded yet. One no longer offered (v0.6.3: ours at 967,000 went, for
/// Ava Chow's at 965,000) becomes the first there is, rather than leave the node to sync from the start.
fn pending_snapshot(s: &Stack, id: &str) -> Option<&'static Snapshot> {
    snapshot_by_id(s, id).or_else(|| s.snapshots.first())
}

/// Part-files of snapshots no longer offered (v0.6.3: ours at 967,000), so 9 GB doesn't stay behind.
fn remove_old_snapshots(st: &StackSettings, s: &Stack) {
    let keep: Vec<PathBuf> = s.snapshots.iter().map(|x| snapshot_file(st, s, x)).collect();
    let Ok(dir) = std::fs::read_dir(st.blocks_at(s)) else { return };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Only the names snapshot_file gives: utxo-<height>-<id>.dat.
        let ours = name.strip_prefix("utxo-").and_then(|n| n.strip_suffix(".dat")).and_then(|n| n.split_once('-')).is_some_and(|(h, id)| {
            !h.is_empty() && h.bytes().all(|b| b.is_ascii_digit()) && !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())
        });
        if ours && !keep.contains(&e.path()) {
            crate::activity::note(&format!("stack: deleting {}, a snapshot no longer offered", name));
            remove_snapshot(&e.path());
        }
    }
}

/// Give a downloaded snapshot the node's network bytes (v0.6.3). Its header is `utxo\xff`, a 2-byte version, then the
/// 4 bytes of the network it was made on, which the node wants to be its own; beta's chain is Bitcoin's up to the fork,
/// so Bitcoin Core's file loads once they say beta's. Nothing else in the file changes. False: not a snapshot's header.
fn own_network(file: &Path, network: [u8; 4]) -> Result<bool, String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(file).map_err(|e| e.to_string())?;
    let mut head = [0u8; 11];
    if f.read_exact(&mut head).is_err() || &head[..5] != b"utxo\xff" {
        return Ok(false);
    }
    if head[7..] != network {
        f.seek(SeekFrom::Start(7)).map_err(|e| e.to_string())?;
        f.write_all(&network).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// Where a snapshot is downloaded to: beside the blocks (the big disk), deleted once loaded.
fn snapshot_file(st: &StackSettings, s: &Stack, x: &Snapshot) -> PathBuf {
    st.blocks_at(s).join(format!("utxo-{}-{}.dat", x.height, x.id))
}

/// Beside a snapshot's part-file: the server's validator for it (a strong ETag, else Last-Modified), so a resumed
/// download asks for the rest of the same file only (`If-Range`, review L7).
fn validator_file(dest: &Path) -> PathBuf {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    dest.with_file_name(format!("{name}.validator"))
}

/// The snapshot and its validator go together.
fn remove_snapshot(dest: &Path) {
    let _ = std::fs::remove_file(dest);
    let _ = std::fs::remove_file(validator_file(dest));
}

/// Download snapshot `x` to `dest`, carrying on from what is there (Range), retrying an interrupted read a few times.
/// A resumed request carries `If-Range`: if the file at the address changed, the server sends it whole and the
/// download starts over, rather than gluing two files together.
async fn fetch_snapshot(x: &Snapshot, dest: &Path) -> Result<(), String> {
    use std::io::Write;
    let vfile = validator_file(dest);
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
    let mut tries = 0;
    loop {
        check_cancel()?;
        let have = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        set(|p| {
            p.bytes = have.min(x.size);
            p.total = Some(x.size);
        });
        if have == x.size {
            return Ok(());
        }
        let mut req = http.get(x.url);
        if have > 0 && have < x.size {
            req = req.header(reqwest::header::RANGE, format!("bytes={}-", have));
            if let Some(v) = std::fs::read_to_string(&vfile).ok().filter(|v| !v.trim().is_empty()) {
                req = req.header(reqwest::header::IF_RANGE, v.trim());
            }
        }
        let result: Result<(), String> = async {
            let mut resp = req.send().await.and_then(|r| r.error_for_status()).map_err(|e| format!("The snapshot's download failed: {}", e))?;
            let mut f = if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                std::fs::OpenOptions::new().append(true).open(dest).map_err(|e| e.to_string())?
            } else {
                // The whole file: keep what names this version of it, for a later resume.
                let h = resp.headers();
                let etag = h.get(reqwest::header::ETAG).and_then(|v| v.to_str().ok()).filter(|e| !e.starts_with("W/"));
                let v = etag.or_else(|| h.get(reqwest::header::LAST_MODIFIED).and_then(|v| v.to_str().ok()));
                match v {
                    Some(v) => std::fs::write(&vfile, v).map_err(|e| e.to_string())?,
                    None => {
                        let _ = std::fs::remove_file(&vfile);
                    }
                }
                std::fs::File::create(dest).map_err(|e| e.to_string())?
            };
            let mut got = if resp.status() == reqwest::StatusCode::PARTIAL_CONTENT { have } else { 0 };
            loop {
                check_cancel()?;
                let chunk = tokio::time::timeout(Duration::from_secs(120), resp.chunk())
                    .await
                    .map_err(|_| "The snapshot's download stalled.".to_string())?
                    .map_err(|e| format!("The snapshot's download was interrupted: {}", e))?;
                let Some(chunk) = chunk else { break };
                f.write_all(&chunk).map_err(|e| e.to_string())?;
                got += chunk.len() as u64;
                if got > x.size {
                    return Err("The snapshot is bigger than it should be.".into());
                }
                set(|p| p.bytes = got);
            }
            f.sync_all().map_err(|e| e.to_string())
        }
        .await;
        match result {
            Ok(()) => {}
            Err(e) if cancelled().is_some() => return Err(cancelled().unwrap_or(e)),
            Err(e) if tries < 5 && !e.contains("bigger") => {
                tries += 1;
                crate::activity::note(&format!("stack: {} (try {} of 5 more)", e, tries));
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Err(e) => {
                if e.contains("bigger") {
                    remove_snapshot(dest);
                }
                return Err(e);
            }
        }
    }
}

/// Until the node knows the snapshot's block header (loadtxoutset needs it), with the headers count as progress.
async fn wait_header(st: &StackSettings, s: &Stack, x: &Snapshot, downloaded: &std::sync::atomic::AtomicBool) -> Result<(), String> {
    let mut alone_since: Option<Instant> = None;
    loop {
        check_cancel()?;
        if node_rpc(st, s, "getblockheader", serde_json::json!([x.blockhash]), Some(Duration::from_secs(20))).await.is_ok() {
            // The count stops here, so it goes rather than stay frozen beside a download still running.
            set(|p| p.headers = None);
            return Ok(());
        }
        let headers = node_call(st, s, "getblockchaininfo").await.ok().and_then(|i| i["headers"].as_u64());
        set(|p| p.headers = headers);
        let peers = node_call(st, s, "getconnectioncount").await.ok().and_then(|v| v.as_u64()).unwrap_or(0);
        // No peers counts only once the download is done: a network that lets HTTP through but not the node's own
        // connections shouldn't cut the download short (re-review 2).
        if peers == 0 && downloaded.load(std::sync::atomic::Ordering::Relaxed) {
            let t = *alone_since.get_or_insert_with(Instant::now);
            if t.elapsed() > Duration::from_secs(600) {
                return Err("The eCash node found no peers in ten minutes: is this computer online?".into());
            }
        } else {
            alone_since = None;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// The node's chainstates (getchainstates): a snapshot it already loaded, by its block.
async fn loaded_snapshot(st: &StackSettings, s: &Stack) -> Option<String> {
    let v = node_call(st, s, "getchainstates").await.ok()?;
    v["chainstates"].as_array()?.iter().find_map(|c| c["snapshot_blockhash"].as_str().map(String::from))
}

/// The block of a snapshot the node started from and hasn't finished checking the history behind, when the stack `s`
/// has no snapshot at that block (v0.6.3: 967,000). The node keeps it in `chainstate_snapshot` until that check is done
/// and it has restarted; a node without an entry for its block stops as it starts, so an update to it would strand the
/// chain.
fn stranded_snapshot(st: &StackSettings, s: &Stack) -> Option<String> {
    let b = std::fs::read(st.node_files(s).join("chainstate_snapshot").join("base_blockhash")).ok()?;
    let hash: String = b.iter().rev().map(|x| format!("{:02x}", x)).collect();
    (b.len() == 32 && !s.snapshots.iter().any(|x| x.blockhash == hash)).then_some(hash)
}

/// Start the node from snapshot `x`: download it (while the node gets the headers), check it, load it with the network
/// paused (the node would otherwise go on fetching blocks from the start), and delete the file. The node itself checks
/// the file's UTXO hash against the one compiled into it, and refuses a wrong one.
async fn load_snapshot(mgr: &NodeManager, st: &mut StackSettings, s: &Stack, x: &'static Snapshot) -> Result<(), String> {
    let done = |st: &mut StackSettings| {
        st.snapshot = None;
        st.start = Some(x.id.to_string());
    };
    if loaded_snapshot(st, s).await.as_deref() == Some(x.blockhash) {
        done(st);
        return Ok(());
    }
    // Already past it (a sync that ran on): a snapshot below the tip can't be loaded, nor is it needed.
    if node_call(st, s, "getblockcount").await.ok().and_then(|v| v.as_u64()).is_some_and(|b| b >= x.height) {
        st.snapshot = None;
        st.start = Some("full".into());
        return Ok(());
    }
    let file = snapshot_file(st, s, x);
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    remove_old_snapshots(st, s);
    set(|p| {
        p.stage = "download-snapshot".into();
        p.snapshot = Some(x.id.to_string());
    });
    // The first error ends both: a failed download doesn't wait for the headers (review L3).
    let downloaded = std::sync::atomic::AtomicBool::new(false);
    tokio::try_join!(
        async {
            fetch_snapshot(x, &file).await?;
            downloaded.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok::<(), String>(())
        },
        wait_header(st, s, x, &downloaded),
    )?;
    set(|p| p.stage = "check-snapshot".into());
    let f2 = file.clone();
    // The network's bytes first: the hash FreeBank expects is of the file with them (and writing them again is a no-op).
    let sum = tokio::task::spawn_blocking(move || -> Result<Option<String>, String> {
        if !own_network(&f2, x.network)? {
            return Ok(None);
        }
        install::sha256_file(&f2).map(Some)
    })
    .await
    .map_err(|e| e.to_string())??;
    if sum.as_deref() != Some(x.sha256) {
        remove_snapshot(&file);
        return Err(format!(
            "The snapshot from {} didn't match the hash FreeBank expects, so it was deleted and not loaded. Try again, or \
             choose another way to start.",
            x.source
        ));
    }
    check_cancel()?;
    set(|p| p.stage = "load-snapshot".into());
    crate::activity::note(&format!("stack: loading the {} snapshot at {}", x.id, x.height));
    let _ = node_rpc(st, s, "setnetworkactive", serde_json::json!([false]), Some(Duration::from_secs(20))).await;
    let loaded = node_rpc(st, s, "loadtxoutset", serde_json::json!([file.to_string_lossy()]), None).await;
    let _ = node_rpc(st, s, "setnetworkactive", serde_json::json!([true]), Some(Duration::from_secs(20))).await;
    if let Err(e) = loaded {
        // The call can end before the node does (a dropped connection): what counts is whether it loaded.
        if loaded_snapshot(st, s).await.as_deref() != Some(x.blockhash) {
            check_cancel()?;
            return Err(format!("The eCash node didn't load the snapshot: {}", e));
        }
    }
    remove_snapshot(&file);
    done(st);
    let mut settings = mgr.settings.lock().await.clone();
    settings.stack = Some(st.clone());
    mgr.save_settings(settings).await?;
    crate::activity::note(&format!("stack: the {} snapshot is loaded", x.id));
    Ok(())
}

/// The folder for the blocks on a disk of their own: FreeBank's own, `freebank-<chain>-blocks`, inside the one chosen, so
/// Stop and delete takes nothing else from that disk (review M1). The one chosen must exist and be a full path without
/// `..`; it is resolved through any links first, so the mark can't land in another folder (review H2).
fn blocks_folder(mgr: &NodeManager, settings: &Settings, s: &Stack, chosen: &Path) -> Result<PathBuf, String> {
    if !chosen.is_absolute() || chosen.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("Give the blocks folder as a full path, without \"..\".".into());
    }
    let real = chosen.canonicalize().map_err(|_| {
        format!("{} isn't there. Choose a folder that exists (on the bigger disk): FreeBank makes its own inside it.", chosen.display())
    })?;
    if !real.is_dir() {
        return Err(format!("{} isn't a folder.", chosen.display()));
    }
    let b = real.join(format!("freebank-{}-blocks", s.chain));
    check_blocks_folder(mgr, settings, &b)?;
    Ok(b)
}

/// FreeBank's blocks folder: a full path, new, empty or one this app made, and not holding (or inside) a FreeBank node's
/// data.
fn check_blocks_folder(mgr: &NodeManager, settings: &Settings, b: &Path) -> Result<(), String> {
    if !b.is_absolute() {
        return Err("Give the blocks folder as a full path.".into());
    }
    if b.is_dir() && !b.join(MARK).exists() && std::fs::read_dir(b).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} is there already and isn't FreeBank's. Move it, or choose another folder.", b.display()));
    }
    if b.is_file() {
        return Err(format!("{} is a file.", b.display()));
    }
    let top = network::top_of(&mgr.app_dir);
    let mut others: Vec<PathBuf> = network::other_datadirs(&top, network::current()).into_iter().map(|(_, d)| d).collect();
    others.push(PathBuf::from(&settings.datadir));
    if let Some(d) = others.iter().find(|d| network::nested(b, d)) {
        return Err(format!("That folder holds, or is inside, a FreeBank node's data ({}). Choose another.", d.display()));
    }
    Ok(())
}

/// Put the mark in a folder this app made for its stack.
fn mark(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MARK), "FreeBank made this folder for its own eCash node. Stop and delete removes it.\n")
        .map_err(|e| e.to_string())
}

/// What Stop and delete did.
#[derive(Debug, Clone, Serialize)]
pub struct Removed {
    /// The folders deleted, and the bytes freed.
    pub deleted: Vec<String>,
    pub freed: u64,
    /// Folders left because they don't carry FreeBank's mark.
    pub kept: Vec<String>,
    /// It was the eCash node the app used: the app goes back to setup for another.
    pub in_use: bool,
}

/// Stop and delete: at any point (an install, a snapshot loading, a sync, or a node at the tip), stop the FreeBank node
/// if it reads from this stack, then the enforcer and the eCash node, and delete their folders: the chain, the
/// enforcer's data and the eCash wallets FreeBank made in that node (watch-only: their keys come from the words). The
/// programs stay, so setting it up again downloads only the chain. Only folders with FreeBank's mark are deleted.
pub async fn remove(mgr: &Arc<NodeManager>) -> Result<Removed, String> {
    network::current().stack.ok_or("This network has no eCash node the app installs.")?;
    // A first install still fetching its programs has no folder yet: it stops, and what it fetched goes (review M4). If
    // it reached its config stage meanwhile, its folder goes too, below.
    if mgr.settings.lock().await.stack.is_none() {
        if !installing() {
            return Err("FreeBank hasn't set up an eCash node of its own here.".into());
        }
        halt_install(mgr, "Stopped: Stop and delete.").await?;
        if mgr.settings.lock().await.stack.is_none() {
            let tmp = mgr.app_dir.join("tmp-stack");
            let freed = tree_bytes(&tmp);
            let _ = std::fs::remove_dir_all(&tmp);
            if !installing() {
                *progress().lock().unwrap() = Progress::default();
            }
            crate::activity::note(&format!("stack: the install stopped and its downloads deleted ({} freed)", gb(freed)));
            return Ok(Removed { deleted: Vec::new(), freed, kept: Vec::new(), in_use: false });
        }
    }
    let Some(st) = mgr.settings.lock().await.stack.clone() else {
        return Err("FreeBank hasn't set up an eCash node of its own here.".into());
    };
    let settings = mgr.settings.lock().await.clone();
    if st.in_use(&settings) {
        crate::node::process::stop(mgr).await.map_err(|e| format!("The FreeBank node didn't stop, so nothing was deleted: {}", e))?;
    }
    // An install under way (a snapshot's download, say) gives up at its next check rather than hold OPS to the end
    // (security review of v0.6.0, M6). With none running this does nothing.
    cancel_install("Stopped: Stop and delete.");
    remove_locked(mgr, &st).await
}

async fn remove_locked(mgr: &Arc<NodeManager>, st: &StackSettings) -> Result<Removed, String> {
    // Down first, without waiting for the install: that ends a download, a header wait or a snapshot load at once.
    // Nothing is left to start again, and no notice stays for a stack that is gone (v0.6.1 review 4).
    stop_asked();
    stop_locked(mgr, st).await?;
    let _one = OPS.lock().await;
    stop_locked(mgr, st).await?;
    // The install's last word lands before the slate is cleaned.
    let until = Instant::now() + Duration::from_secs(30);
    while installing() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let home = crate::node::home();
    let mut out = Removed { deleted: Vec::new(), freed: 0, kept: Vec::new(), in_use: false };
    let mut dirs = vec![PathBuf::from(&st.dir)];
    dirs.extend(st.blocks.as_ref().map(PathBuf::from));
    for d in dirs {
        if !d.exists() {
            continue;
        }
        let ours = d.join(MARK).exists() || in_own_data(&mgr.app_dir, &d);
        let ok = ours && crate::node::obliterate::check_deletable(&d, &home).is_ok();
        if !ok {
            out.kept.push(d.display().to_string());
            continue;
        }
        let n = tree_bytes(&d);
        let d2 = d.clone();
        tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&d2)).await.map_err(|e| e.to_string())?.map_err(|e| {
            format!("Couldn't delete {}: {}", d.display(), e)
        })?;
        out.freed += n;
        out.deleted.push(d.display().to_string());
    }
    let mut settings = mgr.settings.lock().await.clone();
    if st.in_use(&settings) {
        out.in_use = true;
        // The app goes back to setup: no typed login waits there for whatever computer comes next (re-review 1).
        crate::ecash::conn::save_password(&mgr.app_dir, None)?;
        let n = network::current();
        settings.rest = n.rest.into();
        settings.enforcer = n.enforcer.into();
        settings.l1_rpc = None;
        settings.l1_datadir = None;
        settings.l1_user = None;
    }
    settings.stack = None;
    mgr.save_settings(settings).await?;
    *progress().lock().unwrap() = Progress::default();
    *stopped_low().lock().unwrap() = None;
    crate::activity::note(&format!("stack: stopped and deleted ({} freed)", gb(out.freed)));
    Ok(out)
}

/// While the app runs: every minute, an own node with less than LOW_DISK free where it writes is stopped (and an install
/// in progress ends), before the disk fills. It says why in its status until it starts again. And one that stopped on
/// its own is noted and started again (`check_gone`, v0.6.1).
pub fn spawn_watch(mgr: Arc<NodeManager>) {
    let Some(s) = network::current().stack else { return };
    tauri::async_runtime::spawn(async move {
        let mut first = true;
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Some(st) = mgr.settings.lock().await.stack.clone() else { continue };
            if std::mem::take(&mut first) {
                adopt(&mgr, &st, s).await;
            }
            check_gone(&mgr, &st, s).await;
            let up = node_pid(&st, s).is_some_and(|p| runs_bin(p, &bitcoind(&mgr, &st.tag)));
            if !up {
                continue;
            }
            if let Some(why) = low_disk(&st, s) {
                crate::activity::note(&format!("stack: {}", why));
                *stopped_low().lock().unwrap() = Some(why.clone());
                if installing() {
                    cancel_install(&why);
                }
                let _ = stop_locked(&mgr, &st).await;
                let until = Instant::now() + Duration::from_secs(60);
                while installing() && Instant::now() < until {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
}

// ---- Another computer (v0.5.1)

/// What setup found at another computer's address: its eCash node and enforcer, on the ports they usually answer on.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Remote {
    /// The node's REST and RPC address (one port), and the enforcer's, that answered.
    pub rest: Option<String>,
    pub enforcer: Option<String>,
    /// The node is on this network (FreeBank's pin block is in its chain).
    pub on_network: bool,
    pub l1_blocks: Option<u64>,
    /// The address it reached, and whether that is a Tailscale one (the link is then encrypted).
    pub address: Option<String>,
    pub tailscale: bool,
    /// What FreeBank tried, and what is missing in plain words (empty when both answered).
    pub tried: Vec<String>,
    pub detail: String,
}

/// An address in Tailscale's ranges: 100.64.0.0/10, or fd7a:115c:a1e0::/48. Mobile carriers use 100.64.0.0/10 too
/// (shared address space), so this alone doesn't make a link Tailscale's: `via_tailscale` does.
pub fn is_tailscale(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64,
        std::net::IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// The link to `ip` is Tailscale's: the address is in its ranges and this computer reaches it from a Tailscale
/// address of its own (review L6). The system says which local address it would use: a UDP socket "connected" to
/// `ip` sends nothing.
pub fn via_tailscale(ip: std::net::IpAddr) -> bool {
    let local = (|| {
        let any: std::net::SocketAddr = if ip.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { (std::net::Ipv6Addr::UNSPECIFIED, 0).into() };
        let sock = std::net::UdpSocket::bind(any).ok()?;
        sock.connect((ip, 9)).ok()?;
        sock.local_addr().ok().map(|a| a.ip())
    })();
    tailscale_route(ip, local)
}

fn tailscale_route(peer: std::net::IpAddr, local: Option<std::net::IpAddr>) -> bool {
    is_tailscale(peer) && local.is_some_and(is_tailscale)
}

/// "nearby-box", "100.64.0.2", "box.tail1234.ts.net:18303", "[fd7a::1]": the host, and a port if one was given.
fn split_host(input: &str) -> (String, Option<u16>) {
    let t = crate::node::detect::normalize_endpoint(input);
    if let Some(rest) = t.strip_prefix('[') {
        let (h, after) = rest.split_once(']').unwrap_or((rest, ""));
        return (h.to_string(), after.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match t.matches(':').count() {
        1 => {
            let (h, p) = t.split_once(':').unwrap_or((&t, ""));
            (h.to_string(), p.parse().ok())
        }
        _ => (t, None),
    }
}

fn join_host(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    }
}

/// Look for this network's eCash node and enforcer at `input`: an IP or a name (a Tailscale name works with MagicDNS),
/// with the node's port or without (then the ports they usually use: BitWindow's, the eCash node's own, this app's).
pub async fn find_remote(http: &reqwest::Client, input: &str) -> Remote {
    let n = network::current();
    let (host, port) = split_host(input);
    let mut out = Remote::default();
    if host.is_empty() {
        out.detail = "Type the other computer's IP address or Tailscale name.".into();
        return out;
    }
    let port_of = |a: &str| a.rsplit(':').next().and_then(|p| p.parse::<u16>().ok());
    let own = n.stack;
    let mut node_ports: Vec<u16> = match port {
        Some(p) => vec![p],
        None => [port_of(n.rest), Some(18303), Some(8532), own.map(|s| s.rpc)].into_iter().flatten().collect(),
    };
    node_ports.dedup();
    let mut enf_ports: Vec<u16> = [port_of(n.enforcer), own.map(|s| s.grpc)].into_iter().flatten().collect();
    enf_ports.dedup();
    // The name first: a name that doesn't resolve is the likeliest mistake.
    let addr = tokio::time::timeout(Duration::from_secs(8), tokio::net::lookup_host((host.as_str(), node_ports[0]))).await;
    let ip = match addr {
        Ok(Ok(mut a)) => a.next().map(|a| a.ip()),
        _ => None,
    };
    let Some(ip) = ip else {
        out.detail = format!(
            "FreeBank couldn't find \u{201c}{}\u{201d}. Check the name (a Tailscale name works when Tailscale's MagicDNS is \
             on), or type its IP address.",
            host
        );
        return out;
    };
    out.address = Some(ip.to_string());
    out.tailscale = via_tailscale(ip);
    for p in &node_ports {
        let a = join_host(&host, *p);
        out.tried.push(a.clone());
        if let Some(b) = crate::node::detect::l1_blocks(http, &a).await {
            out.rest = Some(a);
            out.l1_blocks = Some(b);
            break;
        }
    }
    for p in &enf_ports {
        let a = join_host(&host, *p);
        out.tried.push(a.clone());
        let up = matches!(tokio::time::timeout(Duration::from_secs(4), tokio::net::TcpStream::connect(&a)).await, Ok(Ok(_)));
        if up && crate::node::detect::enforcer_tip(&a).await.is_ok() {
            out.enforcer = Some(a);
            break;
        }
    }
    if let (Some(r), Some(e)) = (&out.rest, &out.enforcer) {
        let c = crate::node::detect::check_stack(http, r, e).await;
        out.on_network = c.on_beta;
        out.detail = c.detail;
    } else {
        out.detail = match (&out.rest, &out.enforcer) {
            (None, None) => format!("Neither an eCash node nor an enforcer answered at {}.", host),
            (None, Some(_)) => format!("The enforcer answered at {}, but no eCash node did.", host),
            _ => format!("The eCash node answered at {}, but no enforcer did.", host),
        };
    }
    out
}

/// Use the eCash node and enforcer on another computer: the FreeBank node and the app's eCash wallet read from them. The
/// wallet logs in with the login typed under Settings, eCash (that node's cookie stays on that computer). The app's own
/// stack, if it has one, stays as it is.
pub async fn use_remote(mgr: &NodeManager, rest: &str, enforcer: &str) -> Result<Settings, String> {
    let mut s = mgr.settings.lock().await.clone();
    // A login typed for one computer is never sent to another (review M2): another host forgets it, the password on disk
    // too, even with no user name beside it (re-review 1).
    let was = crate::security::host_part(s.l1_rpc.as_deref().unwrap_or(&s.rest)).to_ascii_lowercase();
    let now = crate::security::host_part(&crate::node::detect::normalize_endpoint(rest)).to_ascii_lowercase();
    if was != now {
        let had = s.l1_user.is_some() || crate::ecash::conn::password_path(&mgr.app_dir).exists();
        s.l1_user = None;
        crate::ecash::conn::save_password(&mgr.app_dir, None)?;
        if had {
            crate::activity::note("stack: the eCash login was for another computer, so it is forgotten");
        }
    }
    s.rest = crate::node::detect::normalize_endpoint(rest);
    s.enforcer = crate::node::detect::normalize_endpoint(enforcer);
    s.l1_rpc = None;
    s.l1_datadir = None;
    mgr.save_settings(s.clone()).await?;
    crate::activity::note(&format!("stack: using the eCash node at {} and the enforcer at {}", s.rest, s.enforcer));
    Ok(s)
}

#[tauri::command]
pub async fn stack_find_remote(mgr: tauri::State<'_, Arc<NodeManager>>, host: String) -> Result<Remote, String> {
    Ok(find_remote(&mgr.http, &host).await)
}

#[tauri::command]
pub async fn stack_use_remote(mgr: tauri::State<'_, Arc<NodeManager>>, rest: String, enforcer: String) -> Result<Settings, String> {
    mgr.still_here()?;
    use_remote(&mgr, &rest, &enforcer).await
}

// ---- Install

/// What the install screen shows (stack_install_progress).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub done: bool,
    /// download-node, download-enforcer, check, unpack, config, start; with a snapshot download-snapshot,
    /// check-snapshot, load-snapshot; then sync
    pub stage: String,
    pub bytes: u64,
    pub total: Option<u64>,
    /// While it catches up (stage "sync"): the node's blocks and the headers it knows. While a snapshot downloads, the
    /// headers it has so far.
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
    pub error: Option<String>,
    /// The snapshot this install starts from, by id (v0.5.1): the screen shows its stages.
    pub snapshot: Option<String>,
}

fn progress() -> &'static Arc<Mutex<Progress>> {
    static P: OnceLock<Arc<Mutex<Progress>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn set(f: impl FnOnce(&mut Progress)) {
    f(&mut progress().lock().unwrap());
}

/// Download `d` for this platform into `tmp`, and check it against its pinned hash. Nothing is unpacked from an archive
/// that fails.
async fn fetch(mgr: &NodeManager, d: &network::Download, stage: &str, tmp: &Path) -> Result<PathBuf, String> {
    let (plat, want) = platform(d)?;
    let url = d.url.replace("{platform}", plat);
    let name = url.rsplit('/').next().unwrap_or("archive.tar.gz").to_string();
    let dest = tmp.join(&name);
    set(|p| {
        p.stage = stage.into();
        p.bytes = 0;
        p.total = None;
    });
    install::download(&mgr.http, &url, &dest, |got, total| set(|p| {
        p.bytes = got;
        p.total = total;
    }))
    .await?;
    set(|p| p.stage = "check".into());
    let got = install::sha256_file(&dest)?;
    if got != want {
        return Err(format!("{} didn't match the hash FreeBank expects, so nothing was installed. (expected {}, got {})", name, want, got));
    }
    Ok(dest)
}

/// Find the one file called `name` under `dir` (an archive's own folder layout).
fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// How the person chose to set it up (v0.5.1; the setup screen's beta card).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Choice {
    /// A snapshot's id, "full" for a sync from the start, or none: the first snapshot that answers.
    pub snapshot: Option<String>,
    /// A folder of their own for the blocks: a bigger disk. None or empty: with the node's data.
    pub blocks: Option<String>,
    /// Setup's install: the app then uses this node. Settings' Continue and Update leave a user who went on to another
    /// computer's node there (review M3).
    #[serde(default)]
    pub take: bool,
}

async fn install_stack(mgr: &Arc<NodeManager>, choice: Choice) -> Result<(), String> {
    let _one = OPS.lock().await;
    mgr.still_here()?;
    let s = network::current().stack.ok_or("This network has no eCash node the app installs.")?;
    // An earlier release still running stops first, by its own programs' paths (review finding 2).
    if let Some(old) = mgr.settings.lock().await.stack.clone().filter(|o| o.tag != s.tag) {
        let same_chain = old.dir == mgr.app_dir.join("stack-data").join(s.chain).to_string_lossy();
        if same_chain && stranded_snapshot(&old, s).is_some() {
            return Err(format!(
                "This {} node is still checking the history behind the snapshot it started from, and the new one ({}) \
                 can't take that over. To update it, use Stop and delete, then install again: it starts afresh from a \
                 snapshot.",
                network::current().l1,
                s.tag
            ));
        }
        stop_locked(mgr, &old).await?;
    }
    let dir = programs(mgr, s.tag);
    if !installed(mgr, s.tag) {
        let tmp = mgr.app_dir.join("tmp-stack");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let node = cancellable(fetch(mgr, &s.node, "download-node", &tmp)).await?;
        let enf = cancellable(fetch(mgr, &s.enforcer, "download-enforcer", &tmp)).await?;
        set(|p| p.stage = "unpack".into());
        let x = tmp.join("x");
        install::untar_gz(&node, &x)?;
        install::untar_gz(&enf, &x)?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for name in ["bitcoind", "bitcoin-cli", "bip300301_enforcer"] {
            let f = find(&x, name).ok_or_else(|| format!("The release has no {}.", name))?;
            std::fs::copy(&f, dir.join(name)).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            }
        }
        install::unquarantine(&dir);
        let ok = std::process::Command::new(dir.join("bitcoind")).arg("-version").output().is_ok_and(|o| o.status.success());
        if !ok {
            return Err("The eCash node doesn't run on this computer.".into());
        }
        std::fs::write(dir.join(install::VERIFIED), format!("{}\n", s.tag)).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&tmp);
    }

    check_cancel()?;
    set(|p| p.stage = "config".into());
    let mut settings = mgr.settings.lock().await.clone();
    let old = settings.stack.clone();
    let take = choice.take || old.is_none();
    let ports = plan(s, |p| crate::node::detect::port_busy(p), old.as_ref().map(|o| &o.ports));
    // A folder per chain: a new release on the same chain keeps its data; a reset starts afresh, as its node would refuse
    // the old chain's (review finding 2).
    let dir = mgr.app_dir.join("stack-data").join(s.chain).to_string_lossy().into_owned();
    let (st, fresh_blocks) = match old.filter(|o| o.dir == dir) {
        // The same chain (an update, or Continue after a restart): its blocks folder and its start stay. A snapshot not
        // loaded yet may be swapped for another.
        Some(o) => (
            StackSettings {
                tag: s.tag.into(),
                dir,
                ports,
                blocks: o.blocks,
                snapshot: o
                    .snapshot
                    .clone()
                    .map(|was| choice.snapshot.clone().filter(|c| snapshot_by_id(s, c).is_some()).unwrap_or(was))
                    .and_then(|id| pending_snapshot(s, &id).map(|x| x.id.to_string())),
                start: o.start,
            },
            false,
        ),
        None => {
            let blocks = match choice.blocks.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                Some(b) => Some(blocks_folder(mgr, &settings, s, Path::new(b))?),
                None => None,
            };
            // A snapshot first: the one chosen, else the first that answers; else a sync from the start.
            let snapshot = match choice.snapshot.as_deref() {
                Some("full") => None,
                Some(id) => Some(snapshot_by_id(s, id).ok_or_else(|| format!("FreeBank knows no snapshot \"{}\".", id))?.id.to_string()),
                None => offers(&mgr.http, s).await.into_iter().find(|o| o.available).map(|o| o.id.to_string()),
            };
            let start = (snapshot.is_none() && !s.snapshots.is_empty()).then(|| "full".to_string());
            (StackSettings { tag: s.tag.into(), dir, ports, blocks: blocks.map(|b| b.to_string_lossy().into_owned()), snapshot, start }, true)
        }
    };
    mark(Path::new(&st.dir))?;
    if let Some(b) = &st.blocks {
        // A blocks folder that has gone (its disk not plugged in) isn't made again on this one (review L8).
        if !fresh_blocks && !Path::new(b).is_dir() {
            return Err(format!("The eCash node's blocks folder {} isn't there: is its disk plugged in?", b));
        }
        mark(Path::new(b))?;
    }
    set(|p| p.snapshot = st.snapshot.clone());
    record_stack(&mut settings, s, &st, take);
    if st.in_use(&settings) {
        // Its own node logs in by its cookie: a login typed for another computer goes (re-review 1).
        crate::ecash::conn::save_password(&mgr.app_dir, None)?;
    }
    mgr.save_settings(settings).await?;

    set(|p| p.stage = "start".into());
    start_locked(mgr, Why::Install).await
}

/// A stack install is under way (the network switch and Obliterate wait for it).
pub fn installing() -> bool {
    progress().lock().unwrap().running
}

/// Keep `st` as the app's stack, and point the app at it when `take` (setup's install) or when the stack it replaces was
/// the one in use; a user who went on to another computer's node stays there (review M3).
fn record_stack(settings: &mut Settings, s: &Stack, st: &StackSettings, take: bool) {
    if take || settings.stack.as_ref().is_some_and(|o| o.in_use(settings)) {
        use_own(settings, s, st);
    } else {
        settings.stack = Some(st.clone());
    }
}

/// Run `f`, ending it at once when the install is cancelled (Stop and delete, the app closing): a program download
/// otherwise runs to its end first (review M4).
async fn cancellable<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::pin!(f);
    loop {
        tokio::select! {
            r = &mut f => return r,
            _ = tokio::time::sleep(Duration::from_millis(300)) => check_cancel()?,
        }
    }
}

/// Point the FreeBank node and the app's eCash wallet at the app's own stack.
pub fn use_own(settings: &mut Settings, s: &Stack, st: &StackSettings) {
    settings.rest = format!("127.0.0.1:{}", st.ports.rpc);
    settings.enforcer = format!("127.0.0.1:{}", st.ports.grpc);
    settings.l1_rpc = Some(format!("127.0.0.1:{}", st.ports.rpc));
    settings.l1_datadir = Some(st.node_files(s).to_string_lossy().into_owned());
    settings.l1_user = None;
    settings.stack = Some(st.clone());
}

// ---- Status

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// The app can install an eCash node for this network.
    pub offered: bool,
    pub installed: Option<String>,
    pub node_up: bool,
    pub enforcer_up: bool,
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
    pub peers: Option<u64>,
    /// The enforcer's tip height.
    pub enforcer_tip: Option<u64>,
    /// A newer release this app has for the network (installing it updates the stack).
    pub update: Option<String>,
    /// That release is a new chain (a reset): it starts from block 0 in a folder of its own (v0.6.9).
    pub update_new_chain: bool,
    pub ports: Option<Ports>,
    /// Before an install: the default ports other programs hold, and by whom.
    pub clashes: Vec<Holder>,
    // ---- v0.5.1
    /// The whole chain on disk, in GB, once the node has every block.
    pub full_gb: u64,
    /// The network's snapshots and whether each answers (before an install, or while one waits to load).
    pub offers: Vec<Offer>,
    /// Where its blocks go (or would go, by default), and the free bytes there; the free bytes where its other data is,
    /// when the blocks have a disk of their own.
    pub blocks_at: Option<String>,
    pub blocks_free: Option<u64>,
    pub data_free: Option<u64>,
    /// What its folders take now.
    pub used: Option<u64>,
    /// A snapshot chosen and not loaded yet: setup offers Continue.
    pub snapshot_pending: Option<String>,
    /// How it started: a snapshot's id, or "full".
    pub start: Option<String>,
    /// While the node checks the history behind its snapshot: the block it has checked up to, and the snapshot's.
    pub checked_to: Option<u64>,
    pub checked_of: Option<u64>,
    /// Stopped by the disk watch: why.
    pub stopped_low: Option<String>,
    /// Stopped on its own while the app meant it to run (v0.6.1), until the next start by hand.
    pub stopped_own: Option<StoppedOwn>,
}

/// The stack's status. `with_offers`: also ask the snapshots' servers which answer (setup only: Settings never contacts
/// them).
pub async fn status(mgr: &NodeManager, with_offers: bool) -> Status {
    let Some(s) = network::current().stack else { return Status::default() };
    let st = mgr.settings.lock().await.stack.clone();
    let mut out = Status { offered: true, full_gb: s.full_gb, ..Default::default() };
    out.stopped_low = stopped_low().lock().unwrap().clone();
    out.stopped_own = kept().lock().unwrap().stopped.clone();
    match st {
        None => {
            out.clashes = [s.p2p, s.rpc, s.zmq, s.grpc, s.erpc].into_iter().filter_map(holder).collect();
            if with_offers {
                out.offers = offers(&mgr.http, s).await;
            }
            let at = mgr.app_dir.join("stack-data").join(s.chain);
            out.blocks_free = free_bytes(&at);
            out.blocks_at = Some(at.display().to_string());
        }
        Some(st) => {
            out.snapshot_pending = st.snapshot.clone();
            out.start = st.start.clone();
            if st.snapshot.is_some() && with_offers {
                out.offers = offers(&mgr.http, s).await;
            }
            let at = st.blocks_at(s);
            out.blocks_free = free_bytes(&at);
            out.blocks_at = Some(at.display().to_string());
            if st.blocks.is_some() {
                out.data_free = free_bytes(Path::new(&st.dir));
            }
            let st2 = st.clone();
            out.used = tokio::task::spawn_blocking(move || used_bytes(&st2)).await.ok();
            out.installed = installed(mgr, &st.tag).then(|| st.tag.clone());
            out.update = (st.tag != s.tag).then(|| s.tag.to_string());
            out.update_new_chain =
                out.update.is_some() && st.dir != mgr.app_dir.join("stack-data").join(s.chain).to_string_lossy();
            if let Ok(i) = node_call(&st, s, "getblockchaininfo").await {
                out.node_up = true;
                out.blocks = i["blocks"].as_u64();
                out.headers = i["headers"].as_u64();
            }
            if out.node_up {
                out.peers = node_call(&st, s, "getconnectioncount").await.ok().and_then(|v| v.as_u64());
                // Two chainstates: the snapshot's, and the one checking the history behind it.
                if let Ok(c) = node_call(&st, s, "getchainstates").await {
                    let all = c["chainstates"].as_array().cloned().unwrap_or_default();
                    if all.len() > 1 {
                        let snap = all.iter().find_map(|x| x["snapshot_blockhash"].as_str().map(String::from));
                        out.checked_of = snap.and_then(|h| s.snapshots.iter().find(|x| x.blockhash == h)).map(|x| x.height);
                        out.checked_to = all.iter().find(|x| x["snapshot_blockhash"].is_null()).and_then(|x| x["blocks"].as_u64());
                    }
                }
            }
            out.enforcer_up = enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enforcer_bin(mgr, &st.tag)))
                && crate::node::detect::port_busy(st.ports.grpc);
            if out.enforcer_up {
                out.enforcer_tip = enforcer_tip(st.ports.grpc).await;
            }
            out.ports = Some(st.ports);
        }
    }
    out
}

#[tauri::command]
pub async fn stack_status(mgr: tauri::State<'_, Arc<NodeManager>>, offers: Option<bool>) -> Result<Status, String> {
    Ok(status(&mgr, offers.unwrap_or(false)).await)
}

#[tauri::command]
pub async fn stack_install(mgr: tauri::State<'_, Arc<NodeManager>>, choice: Option<Choice>) -> Result<(), String> {
    {
        let mut p = progress().lock().unwrap();
        if p.running {
            return Err("The eCash node is already being installed.".into());
        }
        // A new run starts with no cancel, cleared under the same lock, so a Stop right after counts (review L8).
        *cancel_slot().lock().unwrap() = None;
        *p = Progress { running: true, ..Default::default() };
    }
    // A blocks folder that won't do is said now, not after the programs' download (re-review 6).
    if let (Some(s), Some(b)) = (network::current().stack, choice.as_ref().and_then(|c| c.blocks.as_deref()).map(str::trim).filter(|b| !b.is_empty())) {
        let settings = mgr.settings.lock().await.clone();
        if let Err(e) = blocks_folder(&mgr, &settings, s, Path::new(b)) {
            progress().lock().unwrap().running = false;
            return Err(e);
        }
    }
    let m = mgr.inner().clone();
    tauri::async_runtime::spawn(async move {
        let r = install_stack(&m, choice.unwrap_or_default()).await;
        set(|p| {
            // The run is over: its cancel goes with it, before another run can start (review L8).
            *cancel_slot().lock().unwrap() = None;
            p.running = false;
            match r {
                Ok(()) => p.done = true,
                Err(e) => p.error = Some(e),
            }
        });
    });
    Ok(())
}

#[tauri::command]
pub fn stack_install_progress() -> Progress {
    progress().lock().unwrap().clone()
}

#[tauri::command]
pub async fn stack_start(mgr: tauri::State<'_, Arc<NodeManager>>) -> Result<(), String> {
    start(&mgr).await
}

#[tauri::command]
pub async fn stack_stop(mgr: tauri::State<'_, Arc<NodeManager>>) -> Result<(), String> {
    stop(&mgr).await
}

/// The free bytes where a folder typed on the setup screen is (or would be made), for its disk line (v0.5.1).
#[tauri::command]
pub fn stack_free(path: String) -> Option<u64> {
    let p = PathBuf::from(path.trim());
    // A folder that isn't there has no free space to show: setup says so instead (re-review 6).
    (p.is_absolute() && p.is_dir()).then(|| free_bytes(&p)).flatten()
}

/// Stop and delete (v0.5.1).
#[tauri::command]
pub async fn stack_remove(mgr: tauri::State<'_, Arc<NodeManager>>) -> Result<Removed, String> {
    remove(mgr.inner()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn easy() -> &'static Stack {
        network::EASYNET.stack.unwrap()
    }

    #[test]
    fn last_words_say_how_a_program_stopped() {
        let d = std::env::temp_dir().join(format!("fblastwords-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let log = d.join("debug.log");
        let say = |text: &str| {
            std::fs::write(&log, text).unwrap();
            last_words(&log)
        };
        // An error, then the orderly stop it caused: the error. The fee estimator's complaint at start is older.
        assert_eq!(
            say("2026-10-21 05:32:18 ERROR: CBlockPolicyEstimator::Read(): up-version (149900) fee estimate file\n\
                 2026-10-21 05:36:14 UpdateTip: new best=00 height=9\n\
                 2026-10-21 05:36:14 Error: A fatal internal error occurred, see debug.log for details\n\
                 2026-10-21 05:36:14 Shutdown: In progress...\n2026-10-21 05:36:14 Shutdown: done\n")
            .as_deref(),
            Some("Error: A fatal internal error occurred, see debug.log for details")
        );
        // An orderly stop with no error in the lines before it (40 at most): asked to stop.
        let quiet: String = (0..45).map(|i| format!("2026-10-21 05:40:{:02} UpdateTip: height={i}\n", i % 60)).collect();
        assert_eq!(
            say(&format!("2026-10-21 05:00:00 ERROR: old\n{quiet}2026-10-21 05:45:49 Shutdown: In progress...\n")).as_deref(),
            Some("It stopped normally, as if it had been asked to.")
        );
        // eCash 31's words and stamps.
        assert_eq!(
            say("2026-10-21T09:04:41Z [warning] natpmp: Getting external address failed\n\
                 2026-10-21T09:05:00Z UpdateTip: new best=00 height=2925\n\
                 2026-10-21T09:05:02Z Shutdown in progress...\n2026-10-21T09:05:03Z Shutdown done\n")
            .as_deref(),
            Some("It stopped normally, as if it had been asked to.")
        );
        assert_eq!(
            say("2026-10-21T09:05:01Z [error] Disk full\n2026-10-21T09:05:02Z Shutdown in progress...\n").as_deref(),
            Some("[error] Disk full")
        );
        // Killed from outside: no stop line, no error.
        assert_eq!(
            say("2026-10-21 05:45:49 UpdateTip: new best=00 height=2905\n").as_deref(),
            Some("Its log ends with no error and no stop, as when a program is killed or crashes.")
        );
        // The enforcer's stamps.
        assert_eq!(
            say("2026-10-21T05:45:20.123456Z  INFO enforcer: synced\n2026-10-21T05:45:21.5Z ERROR enforcer: lost the node's ZMQ\n").as_deref(),
            Some("ERROR enforcer: lost the node's ZMQ")
        );
        assert_eq!(say(""), None);
        std::fs::remove_dir_all(&d).ok();
        assert_eq!(last_words(&log), None, "no log at all");
    }

    #[test]
    fn the_plan_keeps_its_defaults_when_free() {
        let p = plan(easy(), |_| false, None);
        assert_eq!(p, Ports { p2p: 28533, rpc: 28532, zmq: 28535, grpc: 28551, erpc: 28552 });
    }

    #[test]
    fn a_held_port_moves_to_a_free_one() {
        // easynet's installer runs on this computer: its node holds 28532-28535.
        let held = [28532u16, 28533, 28534, 28535];
        let p = plan(easy(), |q| held.contains(&q), None);
        let s = easy();
        for (q, own) in [(p.p2p, s.p2p), (p.p2p + 1, s.p2p + 1), (p.rpc, s.rpc), (p.zmq, s.zmq), (p.grpc, s.grpc), (p.erpc, s.erpc)] {
            // A port that moved never lands on a well-known one; one that kept its default may be its own well-known one.
            assert!(!held.contains(&q) && (q == own || !WELL_KNOWN.contains(&q)), "{q} in {p:?}");
        }
        assert_eq!((p.grpc, p.erpc), (s.grpc, s.erpc), "the enforcer's ports were free");
        let all = [p.p2p, p.p2p + 1, p.rpc, p.zmq, p.grpc, p.erpc];
        let mut d = all.to_vec();
        d.sort();
        d.dedup();
        assert_eq!(d.len(), all.len(), "no port twice: {p:?}");
    }

    #[test]
    fn its_own_running_stack_isnt_a_clash() {
        let ours = Ports { p2p: 28533, rpc: 28532, zmq: 28535, grpc: 28551, erpc: 28552 };
        let p = plan(easy(), |q| is_ours(&ours, q), Some(&ours));
        assert_eq!(p, ours);
    }

    #[test]
    fn the_node_config() {
        let ports = Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 };
        let st = StackSettings { tag: "t".into(), dir: "/d".into(), ports: ports.clone(), ..Default::default() };
        let c = node_conf(easy(), &st);
        assert!(c.contains("\n[easynet]\n"));
        for l in ["rpcbind=127.0.0.1", "rpcallowip=127.0.0.1", "rpcport=2", "port=1", "zmqpubsequence=tcp://127.0.0.1:3", "rest=1", "txindex=1", "listenonion=0", "addnode=163.47.9.132:28533"] {
            assert!(c.lines().any(|x| x == l), "{l}");
        }
        assert!(!c.contains("rpcpassword") && !c.contains("0.0.0.0") && !c.contains("blocksdir"));
        // Beta's (v0.5.1): main's section, no transaction index, the blocks on a disk of their own, ahead of the section.
        let st = StackSettings { blocks: Some("/big/blocks".into()), ..st };
        let c = node_conf(beta(), &st);
        assert!(c.contains("\nblocksdir=/big/blocks\n[main]\n"), "{c}");
        assert!(!c.lines().any(|x| x.starts_with("txindex") || x.starts_with("addnode")), "{c}");
        for l in ["rpcbind=127.0.0.1", "rpcallowip=127.0.0.1", "rest=1", "listenonion=0"] {
            assert!(c.lines().any(|x| x == l), "{l}");
        }
    }

    fn beta() -> &'static Stack {
        network::BETA.stack.unwrap()
    }

    #[test]
    fn betas_folders_and_ports() {
        let st = StackSettings { tag: "t".into(), dir: "/d/stack-data/beta".into(), ..Default::default() };
        assert_eq!(st.node_files(beta()), Path::new("/d/stack-data/beta/node"), "main's files are at the top of the node's folder");
        assert_eq!(st.node_files(easy()), Path::new("/d/stack-data/beta/node/easynet"));
        assert_eq!(st.blocks_at(beta()), st.node_files(beta()));
        let elsewhere = StackSettings { blocks: Some("/big/b".into()), ..st.clone() };
        assert_eq!(elsewhere.blocks_at(beta()), Path::new("/big/b"));
        let a = enforcer_args(beta(), &st);
        assert!(a.contains(&"--network-preset=betanet".to_string()));
        assert!(a.contains(&"--node-rpc-cookie-path=/d/stack-data/beta/node/.cookie".to_string()), "{a:?}");
        let p = plan(beta(), |_| false, None);
        assert_eq!(p, Ports { p2p: 38533, rpc: 38532, zmq: 38535, grpc: 38551, erpc: 38552 });
        // BitWindow's beta node on this computer, and easynet's own stack running: beta's keeps its defaults.
        let held = [18301u16, 18302, 18303, 50051, 8122, 8532, 8533, 28532, 28533, 28535, 28551, 28552];
        assert_eq!(plan(beta(), |q| held.contains(&q), None), p);
    }

    #[test]
    fn the_disk_is_measured() {
        let d = std::env::temp_dir().join(format!("fbdisk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(free_bytes(&d.join("not/yet")).is_some_and(|n| n > 0), "a folder not made yet: its disk");
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("a/one"), [0u8; 1000]).unwrap();
        std::fs::write(d.join("a/b/two"), [0u8; 234]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/usr", d.join("a/link")).unwrap();
        assert_eq!(tree_bytes(&d), 1234, "links aren't followed");
        assert_eq!(gb(LOW_DISK), "20 GB");
        assert_eq!(gb(829_600_000_000), "830 GB");
        assert_eq!(gb(160_700_000), "161 MB", "under a GB, in MB (as the screens' gigabytes())");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_blocks_folder_must_be_empty_or_ours() {
        let d = std::env::temp_dir().join(format!("fbblocks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mgr = NodeManager::new(d.join("app"));
        let settings = Settings { datadir: d.join("fb").to_string_lossy().into_owned(), ..Settings::default() };
        assert!(check_blocks_folder(&mgr, &settings, Path::new("relative/b")).is_err());
        assert!(check_blocks_folder(&mgr, &settings, &d.join("new")).is_ok(), "a folder FreeBank creates");
        std::fs::create_dir_all(d.join("full")).unwrap();
        std::fs::write(d.join("full/blk00000.dat"), b"x").unwrap();
        assert!(check_blocks_folder(&mgr, &settings, &d.join("full")).unwrap_err().contains("isn't FreeBank's"));
        mark(&d.join("ours")).unwrap();
        std::fs::write(d.join("ours/blk00000.dat"), b"x").unwrap();
        assert!(check_blocks_folder(&mgr, &settings, &d.join("ours")).is_ok(), "one FreeBank made");
        assert!(check_blocks_folder(&mgr, &settings, &d.join("fb/blocks")).unwrap_err().contains("FreeBank node"));
        assert!(check_blocks_folder(&mgr, &settings, &d).unwrap_err().contains("isn't FreeBank's"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The blocks go in FreeBank's own folder inside the one chosen, which must exist; `..` and links can't steer the mark
    /// into another folder (review H2, M1).
    #[test]
    fn the_blocks_folder_is_freebanks_own() {
        let d = std::env::temp_dir().join(format!("fbblocks2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("disk/Documents")).unwrap();
        std::fs::write(d.join("disk/Documents/letter.txt"), b"x").unwrap();
        let mgr = NodeManager::new(d.join("app"));
        let settings = Settings { datadir: d.join("fb").to_string_lossy().into_owned(), ..Settings::default() };
        let s = beta();
        let b = blocks_folder(&mgr, &settings, s, &d.join("disk")).unwrap();
        assert_eq!(b, d.join("disk").canonicalize().unwrap().join(format!("freebank-{}-blocks", s.chain)));
        assert!(blocks_folder(&mgr, &settings, s, &d.join("disk/new/../Documents")).unwrap_err().contains(".."));
        assert!(blocks_folder(&mgr, &settings, s, &d.join("missing")).unwrap_err().contains("isn't there"));
        assert!(blocks_folder(&mgr, &settings, s, Path::new("disk")).is_err(), "a full path only");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("disk"), d.join("link")).unwrap();
            assert_eq!(blocks_folder(&mgr, &settings, s, &d.join("link")).unwrap(), b, "a link is resolved first");
        }
        // A folder of that name that isn't FreeBank's is refused; none of this created or marked anything.
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("photo.jpg"), b"x").unwrap();
        assert!(blocks_folder(&mgr, &settings, s, &d.join("disk")).unwrap_err().contains("isn't FreeBank's"));
        assert!(!d.join("disk/new").exists() && !d.join("disk/Documents").join(MARK).exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A chain counts as the app's without the mark only as a chain's folder directly inside a real stack-data/
    /// (review H1).
    #[test]
    fn a_linked_stack_data_is_not_trusted() {
        let d = std::env::temp_dir().join(format!("fbowndata-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let app = d.join("app");
        std::fs::create_dir_all(app.join("stack-data/beta/node")).unwrap();
        std::fs::create_dir_all(app.join("stack-data/notes")).unwrap();
        assert!(in_own_data(&app, &app.join("stack-data/beta")));
        assert!(!in_own_data(&app, &app.join("stack-data/notes")), "no node or enforcer in it");
        assert!(!in_own_data(&app, &app.join("stack-data")));
        assert!(!in_own_data(&app, &app.join("stack-data/beta/node")), "only a chain's own folder");
        #[cfg(unix)]
        {
            std::fs::create_dir_all(d.join("big/Photos/node")).unwrap();
            let app2 = d.join("app2");
            std::fs::create_dir_all(&app2).unwrap();
            std::os::unix::fs::symlink(d.join("big"), app2.join("stack-data")).unwrap();
            assert!(!in_own_data(&app2, &app2.join("stack-data/Photos")), "stack-data links to another disk");
            std::os::unix::fs::symlink(d.join("big/Photos"), app.join("stack-data/linked")).unwrap();
            assert!(!in_own_data(&app, &app.join("stack-data/linked")), "the chain's folder is a link");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Setup's install takes the app to its own node; Settings' Continue, Update or a port move leave a user who went on
    /// to another computer's node there (review M3).
    #[test]
    fn a_user_on_another_computer_stays_there() {
        let st = StackSettings {
            tag: "t".into(),
            dir: "/x/stack-data/beta".into(),
            ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 },
            blocks: None,
            snapshot: None,
            start: None,
        };
        let remote = Settings {
            rest: "nearby-box:18303".into(),
            enforcer: "nearby-box:50051".into(),
            l1_user: Some("u".into()),
            stack: Some(st.clone()),
            ..Settings::default()
        };
        let mut moved = st.clone();
        moved.ports.rpc = 12;
        let mut s = remote.clone();
        record_stack(&mut s, beta(), &moved, false);
        assert_eq!((s.rest.as_str(), s.l1_user.as_deref()), ("nearby-box:18303", Some("u")), "still on the other computer");
        assert_eq!(s.stack.as_ref().unwrap().ports.rpc, 12);
        let mut s = remote.clone();
        record_stack(&mut s, beta(), &moved, true);
        assert_eq!(s.rest, "127.0.0.1:12", "setup's install takes it");
        // In use before: it follows the move.
        let mut s = Settings::default();
        use_own(&mut s, beta(), &st);
        record_stack(&mut s, beta(), &moved, false);
        assert_eq!(s.rest, "127.0.0.1:12");
    }

    /// A login typed for one computer goes nowhere else: "Use them" with another host forgets it (review M2).
    #[tokio::test]
    async fn a_login_stays_with_its_computer() {
        let d = std::env::temp_dir().join(format!("fblogin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mgr = NodeManager::new(d.join("app"));
        let mut s = mgr.settings.lock().await.clone();
        s.rest = "box-a:18303".into();
        s.l1_user = Some("u".into());
        mgr.save_settings(s).await.unwrap();
        crate::ecash::conn::save_password(&mgr.app_dir, Some("pw")).unwrap();
        let s = use_remote(&mgr, "box-a:38532", "box-a:38551").await.unwrap();
        assert_eq!(s.l1_user.as_deref(), Some("u"), "the same computer keeps it");
        assert!(crate::ecash::conn::password_path(&mgr.app_dir).exists());
        let s = use_remote(&mgr, "box-b:18303", "box-b:50051").await.unwrap();
        assert!(s.l1_user.is_none() && !crate::ecash::conn::password_path(&mgr.app_dir).exists(), "another forgets it");
        // A password on disk with no user name beside it (left by the app's own node) goes too (re-review 1).
        crate::ecash::conn::save_password(&mgr.app_dir, Some("pw")).unwrap();
        use_remote(&mgr, "box-c:18303", "box-c:50051").await.unwrap();
        assert!(!crate::ecash::conn::password_path(&mgr.app_dir).exists());
        assert_eq!(stack_free(d.join("nowhere").to_string_lossy().into_owned()), None, "a folder that isn't there");
        assert!(stack_free(d.to_string_lossy().into_owned()).is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_small_chain_keeps_a_small_floor() {
        assert_eq!(low_disk_floor(beta()), LOW_DISK);
        assert_eq!(low_disk_floor(network::EASYNET.stack.unwrap()), LOW_DISK_SMALL);
    }

    /// A small HTTP server for one file, honouring `Range: bytes=N-`, that cuts the first answer short after `cut` bytes.
    async fn serve(body: Vec<u8>, cut: usize) -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/snap.dat", l.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            let mut first = true;
            loop {
                let Ok((mut c, _)) = l.accept().await else { return };
                let mut buf = vec![0u8; 4096];
                let n = c.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let mut from = req
                    .lines()
                    .find_map(|x| x.to_ascii_lowercase().strip_prefix("range: bytes=").map(|r| r.trim_end_matches('-').parse::<usize>().unwrap_or(0)));
                let if_range = req.lines().find_map(|x| x.strip_prefix("if-range: ").or_else(|| x.strip_prefix("If-Range: ")).map(String::from));
                seen2.lock().unwrap().push(match (&from, &if_range) {
                    (None, _) => "whole".into(),
                    (Some(f), None) => format!("from {f}"),
                    (Some(f), Some(v)) => format!("from {f} if {v}"),
                });
                // If-Range naming another version: the whole file, as HTTP says.
                if if_range.as_deref().is_some_and(|v| v != "\"v1\"") {
                    from = None;
                }
                let part = &body[from.unwrap_or(0)..];
                let head = match from {
                    Some(f) => format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n", part.len(), f, body.len() - 1, body.len()),
                    None => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n", part.len()),
                };
                let _ = c.write_all(head.as_bytes()).await;
                let send = if first { &part[..cut.min(part.len())] } else { part };
                first = false;
                let _ = c.write_all(send).await;
                let _ = c.shutdown().await;
            }
        });
        (url, seen)
    }

    #[tokio::test]
    async fn a_snapshot_download_carries_on_where_it_stopped() {
        let _turn = INSTALL_STATE.lock().await;
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (url, seen) = serve(body.clone(), 100_000).await;
        let x: &'static Snapshot = Box::leak(Box::new(Snapshot {
            id: "t",
            source: "test",
            url: Box::leak(url.into_boxed_str()),
            sha256: "",
            size: body.len() as u64,
            height: 1,
            blockhash: "",
            network: [0; 4],
        }));
        let d = std::env::temp_dir().join(format!("fbsnap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("snap.dat");
        fetch_snapshot(x, &f).await.unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), body, "whole, after one interruption");
        assert_eq!(*seen.lock().unwrap(), vec!["whole".to_string(), "from 100000 if \"v1\"".to_string()], "the resume names the version (L7)");
        // A file already whole isn't fetched again; a part-file left by a stopped app carries on.
        fetch_snapshot(x, &f).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
        std::fs::write(&f, &body[..250_000]).unwrap();
        fetch_snapshot(x, &f).await.unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), body);
        assert_eq!(seen.lock().unwrap().last().unwrap(), "from 250000 if \"v1\"");
        // A part-file of another version (its validator says so): the server sends the whole file, and it starts over.
        std::fs::write(&f, &body[..250_000]).unwrap();
        std::fs::write(validator_file(&f), "\"v0\"").unwrap();
        fetch_snapshot(x, &f).await.unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), body);
        assert_eq!(std::fs::read_to_string(validator_file(&f)).unwrap(), "\"v1\"");
        // Stop and delete ends it at once.
        std::fs::remove_file(&f).unwrap();
        // (A cancel counts while an install runs, L2.)
        progress().lock().unwrap().running = true;
        *cancel_slot().lock().unwrap() = None;
        cancel_install("Stopped: Stop and delete.");
        cancel_install("a later reason doesn't replace the first");
        assert_eq!(fetch_snapshot(x, &f).await.unwrap_err(), "Stopped: Stop and delete.");
        progress().lock().unwrap().running = false;
        assert_eq!(cancelled(), None, "with no install running, nothing is cancelled");
        *cancel_slot().lock().unwrap() = None;
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// v0.5.1, found in the walk: an install holds OPS for as long as it runs (on beta, an hour), so Start refuses, and
    /// Stop and the app's exit end it instead of waiting for it.
    /// Tests that use the install's progress and its cancel take turns: they are the process's.
    static INSTALL_STATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn an_install_under_way_is_never_waited_for() {
        let _turn = INSTALL_STATE.lock().await;
        network::set(&network::BETA);
        let d = std::env::temp_dir().join(format!("fbhalt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mgr = Arc::new(NodeManager::new(d.join("app")));
        *progress().lock().unwrap() = Progress { running: true, stage: "download-snapshot".into(), ..Default::default() };
        let held = OPS.lock().await; // the install's
        assert!(start(&mgr).await.unwrap_err().contains("being installed"));
        // Stop: the install is told to give up (it would see the reason at its next check); here nothing ends it, so
        // Stop returns after its wait, without OPS.
        let m = mgr.clone();
        let stopping = tokio::spawn(async move { stop(&m).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(cancelled().is_some_and(|w| w.contains("Continue")), "{:?}", cancelled());
        progress().lock().unwrap().running = false; // the install gives up
        tokio::time::timeout(Duration::from_secs(5), stopping).await.expect("Stop never waits for OPS").unwrap().unwrap();
        assert!(cancelled().is_none());
        *cancel_slot().lock().unwrap() = None; // the run's end clears it
        drop(held);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_snapshot_is_found_by_its_size() {
        let body: Vec<u8> = vec![7u8; 1234];
        let (url, _) = serve(body, usize::MAX).await;
        let url: &'static str = Box::leak(url.into_boxed_str());
        let mk = |id: &'static str, size: u64| Snapshot { id, source: "t", url, sha256: "", size, height: 1, blockhash: "", network: [0; 4] };
        let s: &'static Stack = Box::leak(Box::new(Stack {
            snapshots: Box::leak(vec![mk("right", 1234), mk("wrong", 999)].into_boxed_slice()),
            ..clone_stack(beta())
        }));
        let got = offers(&reqwest::Client::new(), s).await;
        assert_eq!(got.iter().map(|o| (o.id, o.available)).collect::<Vec<_>>(), [("right", true), ("wrong", false)]);
    }

    #[test]
    fn a_snapshot_gets_the_nodes_network_bytes() {
        let d = std::env::temp_dir().join(format!("fbnetbytes-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // Bitcoin Core's header (magic, version 2, Bitcoin's network bytes), then the base block and the rest.
        let mut bitcoin = b"utxo\xff\x02\x00\xf9\xbe\xb4\xd9".to_vec();
        bitcoin.extend((0..5000u32).map(|i| (i % 251) as u8));
        let f = d.join("utxo-965000-ava.dat");
        std::fs::write(&f, &bitcoin).unwrap();
        assert_eq!(own_network(&f, [0xec, 0xa5, 0xb1, 0x04]), Ok(true));
        let got = std::fs::read(&f).unwrap();
        assert_eq!(got[7..11], [0xec, 0xa5, 0xb1, 0x04]);
        assert_eq!((&got[..7], &got[11..]), (&bitcoin[..7], &bitcoin[11..]), "nothing else changes");
        // Again (a check after a restart): the same file.
        assert_eq!(own_network(&f, [0xec, 0xa5, 0xb1, 0x04]), Ok(true));
        assert_eq!(std::fs::read(&f).unwrap(), got);
        // Not a snapshot (a server's error page, say): left as it is, for the check to delete.
        std::fs::write(&f, b"<html>not found</html>").unwrap();
        assert_eq!(own_network(&f, [0xec, 0xa5, 0xb1, 0x04]), Ok(false));
        assert_eq!(std::fs::read(&f).unwrap(), b"<html>not found</html>");
        std::fs::write(&f, b"utxo").unwrap();
        assert_eq!(own_network(&f, [0xec, 0xa5, 0xb1, 0x04]), Ok(false), "too short");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_update_never_strands_a_snapshot_the_new_node_has_no_entry_for() {
        let s = beta();
        let d = std::env::temp_dir().join(format!("fbstranded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let st = StackSettings {
            tag: "betanet-967000-1".into(),
            dir: d.to_string_lossy().into_owned(),
            ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 },
            blocks: None,
            snapshot: None,
            start: Some("ours".into()),
        };
        assert_eq!(stranded_snapshot(&st, s), None, "no snapshot chainstate");
        let cs = st.node_files(s).join("chainstate_snapshot");
        std::fs::create_dir_all(&cs).unwrap();
        // As the node writes it: the block hash's bytes, last first.
        let raw = |hex: &str| (0..32).rev().map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let ours = "00000000000000000000a6b2523780117f2257d82718a1f43d793d6a058fa294";
        std::fs::write(cs.join("base_blockhash"), raw(ours)).unwrap();
        assert_eq!(stranded_snapshot(&st, s).as_deref(), Some(ours), "967,000 isn't in betanet-965000-1");
        std::fs::write(cs.join("base_blockhash"), raw(s.snapshots[0].blockhash)).unwrap();
        assert_eq!(stranded_snapshot(&st, s), None, "965,000 is");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_snapshot_no_longer_offered_gives_way_to_the_first() {
        let s = beta();
        assert_eq!(pending_snapshot(s, "ours").map(|x| x.id), Some("ava"), "967,000 went in v0.6.3");
        assert_eq!(pending_snapshot(s, "l2l").map(|x| x.id), Some("ava"), "LayerTwo Labs' copy went in v0.6.3");
        assert_eq!(pending_snapshot(s, "ava935").map(|x| x.id), Some("ava935"));
        assert!(pending_snapshot(network::EASYNET.stack.unwrap(), "ours").is_none(), "no snapshots, so none");
        // Its part-file goes; the others' stay, and so does anything that isn't a snapshot's.
        let d = std::env::temp_dir().join(format!("fboldsnap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let st = StackSettings {
            tag: s.tag.into(),
            dir: d.join("data").to_string_lossy().into_owned(),
            ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 },
            blocks: Some(d.to_string_lossy().into_owned()),
            snapshot: Some("ours".into()),
            start: None,
        };
        for f in ["utxo-967000-ours.dat", "utxo-967000-ours.dat.validator", "utxo-965000-ava.dat", "utxo-935000-ava935.dat", "utxo-935000-l2l.dat", "blk00000.dat", "utxo-notes.dat", "utxo-1-x.dat.bak"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        remove_old_snapshots(&st, s);
        let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["blk00000.dat", "utxo-1-x.dat.bak", "utxo-935000-ava935.dat", "utxo-965000-ava.dat", "utxo-notes.dat"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A copy of a stack's facts, to change one in a test.
    fn clone_stack(s: &'static Stack) -> Stack {
        Stack {
            tag: s.tag,
            chain: s.chain,
            node: network::Download { url: s.node.url, linux: s.node.linux, mac: s.node.mac },
            enforcer: network::Download { url: s.enforcer.url, linux: s.enforcer.linux, mac: s.enforcer.mac },
            section: s.section,
            folder: s.folder,
            txindex: s.txindex,
            snapshots: s.snapshots,
            full_gb: s.full_gb,
            p2p: s.p2p,
            rpc: s.rpc,
            zmq: s.zmq,
            grpc: s.grpc,
            erpc: s.erpc,
            enforcer_args: s.enforcer_args,
            seeds: s.seeds,
        }
    }

    #[tokio::test]
    async fn stop_and_delete_takes_only_its_own_folders() {
        let d = std::env::temp_dir().join(format!("fbremove-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mgr = Arc::new(NodeManager::new(d.join("app")));
        let dir = mgr.app_dir.join("stack-data").join("beta");
        std::fs::create_dir_all(dir.join("node/chainstate")).unwrap();
        std::fs::write(dir.join("node/chainstate/000001.ldb"), [1u8; 5000]).unwrap();
        mark(&dir).unwrap();
        // A blocks folder that lost its mark (someone else's now): it stays.
        let blocks = d.join("hdd/blocks");
        std::fs::create_dir_all(&blocks).unwrap();
        std::fs::write(blocks.join("blk00000.dat"), [2u8; 7000]).unwrap();
        let st = StackSettings {
            tag: network::BETA.stack.unwrap().tag.into(),
            dir: dir.to_string_lossy().into_owned(),
            ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 },
            blocks: Some(blocks.to_string_lossy().into_owned()),
            snapshot: Some("ours".into()),
            start: None,
        };
        let mut s = mgr.settings.lock().await.clone();
        use_own(&mut s, beta(), &st);
        mgr.save_settings(s).await.unwrap();
        let r = remove_locked(&mgr, &st).await.unwrap();
        assert_eq!(r.deleted, vec![dir.display().to_string()]);
        assert!(r.freed >= 5000, "{r:?}");
        assert_eq!(r.kept, vec![blocks.display().to_string()]);
        assert!(r.in_use, "it was the app's eCash node: the app goes back to setup");
        assert!(!dir.exists() && blocks.join("blk00000.dat").exists());
        let s = mgr.settings.lock().await.clone();
        assert!(s.stack.is_none());
        assert_eq!((s.rest.as_str(), s.enforcer.as_str()), (network::BETA.rest, network::BETA.enforcer), "back to looking for BitWindow's");
        assert!(s.l1_rpc.is_none() && s.l1_datadir.is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_enforcer_listens_on_this_computer_only() {
        let st = StackSettings { tag: "t".into(), dir: "/d".into(), ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 }, ..Default::default() };
        let a = enforcer_args(easy(), &st);
        assert!(a.contains(&"--serve-grpc-addr=127.0.0.1:4".to_string()));
        assert!(a.contains(&"--serve-rpc-addr=127.0.0.1:5".to_string()));
        assert!(a.contains(&"--node-rpc-cookie-path=/d/node/easynet/.cookie".to_string()));
        assert!(a.contains(&"--fork=ecash".to_string()) && !a.iter().any(|x| x.starts_with("--network-")), "{a:?}");
        assert!(!a.iter().any(|x| x.contains("enable-wallet")), "no enforcer wallet: the app's eCash wallet is in the node");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_net_tcp_is_read() {
        let t = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
                 0: 0100007F:C383 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0\n   \
                 1: 0100007F:C383 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 4343 1 0\n";
        assert_eq!(listening_inodes(t, 50051), vec!["4242".to_string()]);
        assert!(listening_inodes(t, 8122).is_empty());
    }

    #[test]
    fn holders_are_named() {
        assert_eq!(label("/home/u/.local/share/bitwindow/assets/bin/bip300301_enforcer --x"), "bip300301_enforcer (BitWindow's)");
        assert_eq!(label("/home/u/easynet/current/bin/bitcoind -datadir=/home/u/easynet/node"), "bitcoind (easynet's installer)");
        assert_eq!(label(""), "another program");
    }

    /// The real thing, on easynet: `FB_STACK_DIR=<empty folder> cargo test --lib -- --ignored stack_real_easynet`. It
    /// downloads easynet's release from GitHub, checks it, starts the node and the enforcer, waits until the node has
    /// synced from easynet's public seed and the enforcer answers, then stops both.
    #[tokio::test]
    #[ignore]
    async fn stack_real_easynet() {
        network::set(&network::EASYNET);
        let dir = PathBuf::from(std::env::var("FB_STACK_DIR").expect("FB_STACK_DIR"));
        let mgr = Arc::new(NodeManager::new(dir.join("app")));
        let t0 = Instant::now();
        install_stack(&mgr, Choice::default()).await.expect("install");
        let st = mgr.settings.lock().await.stack.clone().unwrap();
        println!("installed {} in {:?}, ports {:?}", st.tag, t0.elapsed(), st.ports);
        let s = mgr.settings.lock().await.clone();
        assert_eq!(s.rest, format!("127.0.0.1:{}", st.ports.rpc));
        assert_eq!(s.enforcer, format!("127.0.0.1:{}", st.ports.grpc));
        let until = Instant::now() + Duration::from_secs(1200);
        loop {
            let x = status(&mgr, false).await;
            println!("node {} enforcer {} blocks {:?} headers {:?} peers {:?}", x.node_up, x.enforcer_up, x.blocks, x.headers, x.peers);
            if x.node_up && x.enforcer_up && x.blocks.is_some_and(|b| b > 2000) && x.blocks == x.headers {
                break;
            }
            assert!(Instant::now() < until, "not synced in 20 minutes");
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        let pinned = node_call(&st, network::EASYNET.stack.unwrap(), "getblockchaininfo").await.unwrap();
        assert_eq!(pinned["chain"], "easynet");
        // The app's eCash wallet connects to it as it would to any eCash node: chain, pin, cookie.
        let conn = crate::ecash::conn::connect(&mgr.http, &s, Some(&mgr.app_dir)).await.expect("the eCash wallet connects");
        assert_eq!(conn.chain, crate::seed::Chain::Test);
        // Start again: both already up, nothing new is started.
        let pids = (node_pid(&st, network::EASYNET.stack.unwrap()), enforcer_pid(&st));
        start(&mgr).await.unwrap();
        assert_eq!(pids, (node_pid(&st, network::EASYNET.stack.unwrap()), enforcer_pid(&st)));
        stop(&mgr).await.unwrap();
        let x = status(&mgr, false).await;
        assert!(!x.node_up && !x.enforcer_up, "{x:?}");
        println!("stopped; whole run {:?}", t0.elapsed());
    }

    /// After `stack_real_easynet` (its FB_STACK_DIR, with the stack installed): another program on the node's RPC port
    /// at the next start moves the app's stack to free ports, and the FreeBank node's addresses follow.
    #[tokio::test]
    #[ignore]
    async fn stack_moves_off_a_taken_port() {
        network::set(&network::EASYNET);
        let dir = PathBuf::from(std::env::var("FB_STACK_DIR").expect("FB_STACK_DIR"));
        let mgr = Arc::new(NodeManager::new(dir.join("app")));
        let before = mgr.settings.lock().await.stack.clone().expect("run stack_real_easynet first");
        let squatter = std::net::TcpListener::bind(("127.0.0.1", before.ports.rpc)).expect("the port is free to take");
        start(&mgr).await.expect("start on other ports");
        let s = mgr.settings.lock().await.clone();
        let after = s.stack.clone().unwrap();
        println!("ports {:?} -> {:?}", before.ports, after.ports);
        assert_ne!(after.ports.rpc, before.ports.rpc);
        assert_eq!(s.rest, format!("127.0.0.1:{}", after.ports.rpc));
        assert_eq!(s.l1_rpc.as_deref(), Some(format!("127.0.0.1:{}", after.ports.rpc).as_str()));
        let x = status(&mgr, false).await;
        assert!(x.node_up && x.enforcer_up, "{x:?}");
        stop(&mgr).await.unwrap();
        drop(squatter);
    }

    /// After `stack_real_easynet` (its FB_STACK_DIR): the watch (v0.6.1) with the real programs. The eCash node stopped
    /// from outside is noted with its log's last words and started again; the enforcer killed within the hour is noted
    /// and left for Start; a stop by the app is never taken for one on its own.
    #[tokio::test]
    #[ignore]
    async fn stack_watch_real_easynet() {
        network::set(&network::EASYNET);
        let s = network::EASYNET.stack.unwrap();
        let dir = PathBuf::from(std::env::var("FB_STACK_DIR").expect("FB_STACK_DIR"));
        let mgr = Arc::new(NodeManager::new(dir.join("app")));
        let st = mgr.settings.lock().await.stack.clone().expect("run stack_real_easynet first");
        let (node_bin, enf_bin) = (bitcoind(&mgr, &st.tag), enforcer_bin(&mgr, &st.tag));
        let kill = |pid: u32, sig: i32, bin: PathBuf| async move {
            unsafe { libc::kill(pid as libc::pid_t, sig) };
            wait_gone(pid, &bin, 120).await;
            assert!(!runs_bin(pid, &bin), "{pid} still runs");
        };
        start(&mgr).await.expect("start");
        check_gone(&mgr, &st, s).await;
        assert_eq!(status(&mgr, false).await.stopped_own, None, "both up: nothing to say");

        // The eCash node asked to stop from outside (SIGTERM: its orderly stop).
        let node = node_pid(&st, s).unwrap();
        kill(node, libc::SIGTERM, node_bin.clone()).await;
        check_gone(&mgr, &st, s).await;
        let x = status(&mgr, false).await;
        let gone = x.stopped_own.clone().expect("noted");
        println!("{gone:?}");
        assert_eq!((gone.what.as_str(), gone.restarted), ("node", true));
        assert_eq!(gone.line.as_deref(), Some("It stopped normally, as if it had been asked to."));
        assert!(x.node_up && x.enforcer_up && node_pid(&st, s) != Some(node), "started again: {x:?}");

        // The enforcer killed within the hour: the other program's first stop, so started again too (v0.6.1 review).
        let killed = "Its log ends with no error and no stop, as when a program is killed or crashes.";
        let enf = enforcer_pid(&st).unwrap();
        kill(enf, libc::SIGKILL, enf_bin.clone()).await;
        check_gone(&mgr, &st, s).await;
        let x = status(&mgr, false).await;
        let gone = x.stopped_own.clone().expect("noted");
        println!("{gone:?}");
        assert_eq!((gone.what.as_str(), gone.restarted), ("enforcer", true));
        assert_eq!(gone.line.as_deref(), Some(killed));
        assert!(x.node_up && x.enforcer_up && enforcer_pid(&st) != Some(enf), "started again: {x:?}");

        // The eCash node stops again within the hour: noted, not started again, though the enforcer's restart came in
        // between (v0.6.1 re-review 1: one time for both let the two take turns).
        let node = node_pid(&st, s).unwrap();
        kill(node, libc::SIGTERM, node_bin.clone()).await;
        check_gone(&mgr, &st, s).await;
        let x = status(&mgr, false).await;
        let gone = x.stopped_own.clone().expect("noted");
        println!("{gone:?}");
        assert_eq!((gone.what.as_str(), gone.restarted, gone.restart_error.as_deref()), ("node", false, None));
        assert!(!x.node_up, "left for Start: {x:?}");
        check_gone(&mgr, &st, s).await;
        assert_eq!(status(&mgr, false).await.stopped_own, Some(gone), "noted once");

        // Start by hand: both up, the note gone.
        start(&mgr).await.expect("start by hand");
        let x = status(&mgr, false).await;
        assert!(x.node_up && x.enforcer_up && x.stopped_own.is_none(), "{x:?}");
        // A launch that finds them running ("Keep running"): the watch takes them on (v0.6.1 review 3).
        kept().lock().unwrap().want = false;
        adopt(&mgr, &st, s).await;
        assert!(kept().lock().unwrap().want, "adopted");
        // The app's own stop is not a stop on its own.
        stop(&mgr).await.unwrap();
        check_gone(&mgr, &st, s).await;
        let x = status(&mgr, false).await;
        assert!(!x.node_up && !x.enforcer_up && x.stopped_own.is_none(), "{x:?}");
    }

    #[test]
    fn another_computers_address() {
        assert_eq!(split_host("nearby-box"), ("nearby-box".into(), None));
        assert_eq!(split_host(" 100.64.0.2:18303 "), ("100.64.0.2".into(), Some(18303)));
        assert_eq!(split_host("http://box.tail1.ts.net:8532/"), ("box.tail1.ts.net".into(), Some(8532)));
        assert_eq!(split_host("[fd7a:115c:a1e0::5]:50051"), ("fd7a:115c:a1e0::5".into(), Some(50051)));
        assert_eq!(split_host("fd7a:115c:a1e0::5"), ("fd7a:115c:a1e0::5".into(), None));
        assert_eq!(join_host("fd7a::5", 1), "[fd7a::5]:1");
        assert!(is_tailscale("100.64.0.2".parse().unwrap()));
        assert!(is_tailscale(std::net::Ipv4Addr::new(100, 127, 0, 1).into()));
        assert!(!is_tailscale("100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale("192.168.1.20".parse().unwrap()));
        assert!(is_tailscale("fd7a:115c:a1e0::5".parse().unwrap()));
        // A carrier's shared address reached from an ordinary one isn't Tailscale's (L6); from a tailnet address it is.
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        assert!(!tailscale_route(ip("100.64.0.2"), Some(ip("192.168.1.5"))));
        assert!(tailscale_route(ip("100.64.0.2"), Some(ip("100.64.0.9"))));
        assert!(!tailscale_route(ip("192.168.1.20"), Some(ip("100.64.0.9"))));
        assert!(!tailscale_route(ip("100.64.0.2"), None));
        assert!(!via_tailscale(ip("127.0.0.1")));
    }

    #[tokio::test]
    async fn another_computer_that_isnt_there() {
        let http = reqwest::Client::new();
        let r = find_remote(&http, "no-such-host.invalid").await;
        assert!(r.rest.is_none() && r.detail.contains("couldn't find"), "{r:?}");
        // This computer, where nothing listens on those ports (a free port as the node's).
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let free = l.local_addr().unwrap().port();
        drop(l);
        let r = find_remote(&http, &format!("127.0.0.1:{free}")).await;
        assert_eq!(r.address.as_deref(), Some("127.0.0.1"));
        assert!(!r.tailscale);
        assert_eq!(r.tried[0], format!("127.0.0.1:{free}"));
        assert!(r.rest.is_none(), "{r:?}");
    }

    #[test]
    fn a_port_in_use_is_found() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = holder(port).expect("held");
        #[cfg(target_os = "linux")]
        assert_eq!(h.pid, Some(std::process::id()), "{h:?}");
        drop(l);
        assert_eq!(holder(port), None);
    }
}
