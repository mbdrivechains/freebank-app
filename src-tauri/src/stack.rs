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

use crate::network::{self, Stack};
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
pub struct StackSettings {
    /// The release installed, e.g. "easynet-3.1".
    pub tag: String,
    /// Its data folder (node/ and enforcer/ inside).
    pub dir: String,
    pub ports: Ports,
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
    /// The folder the node keeps this network's files in (its cookie, debug.log, wallets).
    pub fn node_files(&self, s: &Stack) -> PathBuf {
        self.node_dir().join(s.section)
    }
    pub fn enforcer_dir(&self) -> PathBuf {
        Path::new(&self.dir).join("enforcer")
    }
}

/// Every port another stack on this computer may use by default: BitWindow's beta node and enforcer, FreeBank on beta
/// and mainnet, easynet's installer (its node, ZMQ and enforcer). The app's own stacks keep off them.
pub const WELL_KNOWN: &[u16] = &[8122, 8454, 8455, 18301, 18302, 18303, 28532, 28533, 28534, 28535, 28454, 28455, 50051];

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
/// FreeBank node reads deposits from blocks over it), the transaction index (the enforcer's), no Tor listener.
pub fn node_conf(s: &Stack, p: &Ports) -> String {
    let seeds: String = s.seeds.iter().map(|a| format!("addnode={}\n", a)).collect();
    format!(
        "# Written by the FreeBank app for its own {sec} node. Other eCash nodes on this computer keep their own files.\n\
         [{sec}]\nserver=1\ntxindex=1\nrest=1\nlistenonion=0\nrpcbind=127.0.0.1\nrpcallowip=127.0.0.1\nrpcport={rpc}\n\
         port={p2p}\nzmqpubsequence=tcp://127.0.0.1:{zmq}\nfallbackfee=0.00001\n{seeds}",
        sec = s.section,
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
    let cookie = std::fs::read_to_string(st.node_files(s).join(".cookie")).map_err(|_| "The eCash node isn't up yet.".to_string())?;
    let (user, pass) = cookie.trim().split_once(':').ok_or("The eCash node's cookie is damaged.")?;
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
    let r: serde_json::Value = http
        .post(format!("http://127.0.0.1:{}/", st.ports.rpc))
        .basic_auth(user, Some(pass))
        .json(&serde_json::json!({"jsonrpc": "1.0", "id": "fb", "method": method, "params": []}))
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

/// Wait (at most `secs`) until the node has caught up with its peers and the enforcer has every block it has. A FreeBank
/// node started earlier drops the first headers its peer sends when it can't yet check them against the eCash chain,
/// and asks again only when the peer announces its next block (minutes on easynet). True once caught up.
async fn wait_synced(st: &StackSettings, s: &Stack, secs: u64) -> bool {
    let started = Instant::now();
    let until = started + Duration::from_secs(secs);
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
        // No peer in 30 seconds: this computer is offline, or the network's seed is down. Waiting longer only holds the
        // FreeBank node back (review finding 7).
        if Instant::now() > until || (peers == 0 && started.elapsed() > Duration::from_secs(30)) {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
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

/// One stack operation at a time: two starts at once could each spawn an enforcer (review finding 5).
static OPS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start the node and the enforcer if they aren't running; each one already up is left as it is.
pub async fn start(mgr: &NodeManager) -> Result<(), String> {
    let _one = OPS.lock().await;
    start_locked(mgr, false).await
}

/// `start`, under OPS. `installing`: the install's own start, which may wait the whole first sync.
async fn start_locked(mgr: &NodeManager, installing: bool) -> Result<(), String> {
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
    let node_bin = bitcoind(mgr, &st.tag);
    let enf_bin = enforcer_bin(mgr, &st.tag);
    let node_up = node_pid(&st, s).is_some_and(|p| runs_bin(p, &node_bin));
    let enf_up = enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enf_bin));
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
            use_own(&mut settings, s, &st);
            mgr.save_settings(settings).await?;
        }
    }
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
            f.write_all(node_conf(s, &st.ports).as_bytes()).map_err(|e| e.to_string())?;
        }
        let mut c = std::process::Command::new(&node_bin);
        c.arg(format!("-datadir={}", st.node_dir().display())).arg(format!("-conf={}", st.node_dir().join("ecash.conf").display()));
        spawn_detached(&mut c).map_err(|e| format!("Couldn't start the eCash node: {}", e))?;
        crate::activity::note("stack: eCash node started");
    }
    // The enforcer needs the node's RPC.
    let until = Instant::now() + Duration::from_secs(120);
    while node_call(&st, s, "getblockcount").await.is_err() {
        if Instant::now() > until {
            return Err(format!("The eCash node didn't answer within 2 minutes: see {}", st.node_files(s).join("debug.log").display()));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if !enforcer_pid(&st).is_some_and(|p| runs_bin(p, &enf_bin)) {
        for port in [st.ports.grpc, st.ports.erpc] {
            if let Some(h) = holder(port) {
                return Err(format!("Port {} is in use by {}: the enforcer can't start. Stop that program, or stop and start FreeBank's eCash node to move it.", port, h.name));
            }
        }
        std::fs::create_dir_all(st.enforcer_dir().join("logs")).map_err(|e| e.to_string())?;
        let mut c = std::process::Command::new(&enf_bin);
        c.args(enforcer_args(s, &st));
        let child = spawn_detached(&mut c).map_err(|e| format!("Couldn't start the enforcer: {}", e))?;
        std::fs::write(st.enforcer_dir().join("enforcer.pid"), child.id().to_string()).map_err(|e| e.to_string())?;
        crate::activity::note("stack: enforcer started");
    }
    // Both were up already: nothing to wait for (review finding 7).
    if node_up && enf_up {
        return Ok(());
    }
    set(|p| {
        if p.running {
            p.stage = "sync".into();
        }
    });
    if !wait_synced(&st, s, if installing { 900 } else { 120 }).await {
        crate::activity::note("stack: not caught up yet; FreeBank starts anyway");
    }
    Ok(())
}

/// Stop the enforcer, then the node (RPC `stop`, else SIGTERM), and wait for both. Only processes running the app's own
/// programs are signalled.
pub async fn stop(mgr: &NodeManager) -> Result<(), String> {
    let _one = OPS.lock().await;
    let Some(st) = mgr.settings.lock().await.stack.clone() else { return Ok(()) };
    stop_locked(mgr, &st).await
}

/// `stop` for the stack `st` describes (its own release's programs), under OPS. An error if either is still running.
async fn stop_locked(mgr: &NodeManager, st: &StackSettings) -> Result<(), String> {
    let Some(s) = network::current().stack else { return Ok(()) };
    let st = st.clone();
    let enf_bin = enforcer_bin(mgr, &st.tag);
    if let Some(p) = enforcer_pid(&st).filter(|&p| runs_bin(p, &enf_bin)) {
        #[cfg(unix)]
        unsafe {
            libc::kill(p as libc::pid_t, libc::SIGTERM);
        }
        wait_gone(p, &enf_bin, 30).await;
    }
    let node_bin = bitcoind(mgr, &st.tag);
    if let Some(p) = node_pid(&st, s).filter(|&p| runs_bin(p, &node_bin)) {
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
    crate::activity::note("stack: stopped");
    Ok(())
}

async fn wait_gone(pid: u32, bin: &Path, secs: u64) {
    let until = Instant::now() + Duration::from_secs(secs);
    while runs_bin(pid, bin) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// At the app's exit: the stack stops with it, unless "Keep running" keeps the FreeBank node (which needs it) up.
pub async fn at_exit(mgr: &NodeManager) {
    let s = mgr.settings.lock().await.clone();
    if s.stack.as_ref().is_some_and(|st| st.in_use(&s)) && !s.keep_running {
        let _ = stop(mgr).await;
    }
}

// ---- Install

/// What the install screen shows (stack_install_progress).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub done: bool,
    /// download-node, download-enforcer, check, unpack, config, start
    pub stage: String,
    pub bytes: u64,
    pub total: Option<u64>,
    /// While it catches up (stage "sync"): the node's blocks and the headers it knows.
    pub blocks: Option<u64>,
    pub headers: Option<u64>,
    pub error: Option<String>,
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

async fn install_stack(mgr: &Arc<NodeManager>) -> Result<(), String> {
    let _one = OPS.lock().await;
    mgr.still_here()?;
    let s = network::current().stack.ok_or("This network has no eCash node the app installs.")?;
    // An earlier release still running stops first, by its own programs' paths (review finding 2).
    if let Some(old) = mgr.settings.lock().await.stack.clone().filter(|o| o.tag != s.tag) {
        stop_locked(mgr, &old).await?;
    }
    let dir = programs(mgr, s.tag);
    if !installed(mgr, s.tag) {
        let tmp = mgr.app_dir.join("tmp-stack");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let node = fetch(mgr, &s.node, "download-node", &tmp).await?;
        let enf = fetch(mgr, &s.enforcer, "download-enforcer", &tmp).await?;
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

    set(|p| p.stage = "config".into());
    let mut settings = mgr.settings.lock().await.clone();
    let old = settings.stack.clone();
    let ports = plan(s, |p| crate::node::detect::port_busy(p), old.as_ref().map(|o| &o.ports));
    // A folder per chain: a new release on the same chain keeps its data; a reset starts afresh, as its node would refuse
    // the old chain's (review finding 2).
    let dir = mgr.app_dir.join("stack-data").join(s.chain).to_string_lossy().into_owned();
    let st = StackSettings { tag: s.tag.into(), dir, ports };
    use_own(&mut settings, s, &st);
    mgr.save_settings(settings).await?;

    set(|p| p.stage = "start".into());
    start_locked(mgr, true).await
}

/// A stack install is under way (the network switch and Obliterate wait for it).
pub fn installing() -> bool {
    progress().lock().unwrap().running
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
    pub ports: Option<Ports>,
    /// Before an install: the default ports other programs hold, and by whom.
    pub clashes: Vec<Holder>,
}

pub async fn status(mgr: &NodeManager) -> Status {
    let Some(s) = network::current().stack else { return Status::default() };
    let st = mgr.settings.lock().await.stack.clone();
    let mut out = Status { offered: true, ..Default::default() };
    match st {
        None => {
            out.clashes = [s.p2p, s.rpc, s.zmq, s.grpc, s.erpc].into_iter().filter_map(holder).collect();
        }
        Some(st) => {
            out.installed = installed(mgr, &st.tag).then(|| st.tag.clone());
            out.update = (st.tag != s.tag).then(|| s.tag.to_string());
            if let Ok(i) = node_call(&st, s, "getblockchaininfo").await {
                out.node_up = true;
                out.blocks = i["blocks"].as_u64();
                out.headers = i["headers"].as_u64();
            }
            if out.node_up {
                out.peers = node_call(&st, s, "getconnectioncount").await.ok().and_then(|v| v.as_u64());
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
pub async fn stack_status(mgr: tauri::State<'_, Arc<NodeManager>>) -> Result<Status, String> {
    Ok(status(&mgr).await)
}

#[tauri::command]
pub async fn stack_install(mgr: tauri::State<'_, Arc<NodeManager>>) -> Result<(), String> {
    {
        let mut p = progress().lock().unwrap();
        if p.running {
            return Err("The eCash node is already being installed.".into());
        }
        *p = Progress { running: true, ..Default::default() };
    }
    let m = mgr.inner().clone();
    tauri::async_runtime::spawn(async move {
        let r = install_stack(&m).await;
        set(|p| {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn easy() -> &'static Stack {
        network::EASYNET.stack.unwrap()
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
        for q in [p.p2p, p.p2p + 1, p.rpc, p.zmq, p.grpc, p.erpc] {
            assert!(!held.contains(&q) && !WELL_KNOWN.contains(&q), "{q} in {p:?}");
        }
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
        let c = node_conf(easy(), &Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 });
        assert!(c.contains("\n[easynet]\n"));
        for l in ["rpcbind=127.0.0.1", "rpcallowip=127.0.0.1", "rpcport=2", "port=1", "zmqpubsequence=tcp://127.0.0.1:3", "rest=1", "txindex=1", "listenonion=0", "addnode=163.47.9.132:28533"] {
            assert!(c.lines().any(|x| x == l), "{l}");
        }
        assert!(!c.contains("rpcpassword") && !c.contains("0.0.0.0"));
    }

    #[test]
    fn the_enforcer_listens_on_this_computer_only() {
        let st = StackSettings { tag: "t".into(), dir: "/d".into(), ports: Ports { p2p: 1, rpc: 2, zmq: 3, grpc: 4, erpc: 5 } };
        let a = enforcer_args(easy(), &st);
        assert!(a.contains(&"--serve-grpc-addr=127.0.0.1:4".to_string()));
        assert!(a.contains(&"--serve-rpc-addr=127.0.0.1:5".to_string()));
        assert!(a.contains(&"--node-rpc-cookie-path=/d/node/easynet/.cookie".to_string()));
        assert!(a.contains(&"--network-preset=easynet".to_string()));
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
        install_stack(&mgr).await.expect("install");
        let st = mgr.settings.lock().await.stack.clone().unwrap();
        println!("installed {} in {:?}, ports {:?}", st.tag, t0.elapsed(), st.ports);
        let s = mgr.settings.lock().await.clone();
        assert_eq!(s.rest, format!("127.0.0.1:{}", st.ports.rpc));
        assert_eq!(s.enforcer, format!("127.0.0.1:{}", st.ports.grpc));
        let until = Instant::now() + Duration::from_secs(1200);
        loop {
            let x = status(&mgr).await;
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
        let x = status(&mgr).await;
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
        let x = status(&mgr).await;
        assert!(x.node_up && x.enforcer_up, "{x:?}");
        stop(&mgr).await.unwrap();
        drop(squatter);
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
