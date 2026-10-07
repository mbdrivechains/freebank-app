//! Running freebankd as this app's child: start, stop (RPC `stop`, then SIGTERM), and the
//! progress/status reads the screens poll. A node the app didn't start is only read, never stopped.
//! A node the app started before it last closed, recognised again at launch (background.rs), is
//! managed like its own child.

use super::background::{self, PidFile};
use super::{conf_tag, detect, install, lock, NodeManager, EXPLORER, PIN_HASH, PIN_HEIGHT};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Start freebankd with the settings on file. Errors are written for the screen. With "Keep
/// FreeBank's node running after I close the app" on, the node starts in its own session, so it
/// outlives the app; otherwise (on Linux) the kernel stops it if the app dies.
pub async fn start(mgr: &NodeManager) -> Result<(), String> {
    start_opts(mgr, false).await
}

/// No -reindex start pending.
pub const REINDEX_IDLE: u8 = 0;
/// The app started the node with -reindex and it hasn't answered yet.
pub const REINDEX_RUNNING: u8 = 1;
/// That start failed too: say so, and wait for the user's next Start.
pub const REINDEX_GAVE_UP: u8 = 2;

/// freebankd stops with "Restart with -reindex" when its data folder needs rebuilding: one an older release wrote
/// in another record format (v0.2.18 moved to format 3, a consensus change: "every node must upgrade and restart
/// once with -reindex"), or one an unclean stop left out of step. Do that once, by ourselves: it rebuilds the
/// node's indexes from the blocks it already has; the wallet stays as it is. If that start fails too, say so, and
/// wait for the user's next Start.
pub(crate) fn needs_reindex(why: &str) -> bool {
    why.contains("Restart with -reindex")
}

async fn reap_or_reindex(mgr: &NodeManager, datadir: &Path) -> (bool, Option<String>) {
    let (running, exited) = reap(mgr, datadir).await;
    if running {
        return (running, exited);
    }
    match mgr.reindex.load(Ordering::SeqCst) {
        REINDEX_RUNNING => {
            mgr.reindex.store(REINDEX_GAVE_UP, Ordering::SeqCst);
            return (running, exited);
        }
        REINDEX_GAVE_UP => return (running, exited),
        _ => {}
    }
    if !exited.as_deref().is_some_and(needs_reindex) {
        return (running, exited);
    }
    // Never in the middle of something else (a stop, an update, Remove FreeBank): that one decides.
    let Ok(_busy) = mgr.busy("Starting FreeBank to rebuild its data…") else {
        return (running, exited);
    };
    crate::activity::note("node: asked for -reindex; starting it once with -reindex");
    match start_opts(mgr, true).await {
        Ok(()) => (true, None),
        Err(e) => {
            mgr.reindex.store(REINDEX_GAVE_UP, Ordering::SeqCst);
            let msg = format!("{} Starting it with -reindex failed: {}", exited.unwrap_or_default(), e);
            *mgr.last_exit.lock().unwrap() = Some(msg.clone());
            (false, Some(msg))
        }
    }
}

/// A `-reindex` start ends when freebankd logs "Reindexing finished" (init.cpp, ThreadImport): its RPC answers long
/// before, while it is still replaying the blocks. Settled only for our own running node, so an answer from
/// another node on our port never clears a give-up.
fn settle_reindex(mgr: &NodeManager, datadir: &Path, ours_up: bool) {
    if ours_up && mgr.reindex.load(Ordering::SeqCst) == REINDEX_RUNNING && reindex_finished(mgr, datadir) {
        mgr.reindex.store(REINDEX_IDLE, Ordering::SeqCst);
        crate::activity::note("node: rebuilt its data");
    }
}

/// Where the node's two logs ended when the app last started it.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogMark {
    pub debug_log: u64,
    pub out: u64,
    /// How far reindex_finished has read debug.log.
    pub scanned: u64,
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Bytes of a file from `from` (capped to its last `max`), or None if it is shorter than `from` (rewritten since).
fn read_since(p: &Path, from: u64, max: u64) -> Option<String> {
    let mut f = std::fs::File::open(p).ok()?;
    let len = f.metadata().ok()?.len();
    if len < from {
        return None;
    }
    f.seek(SeekFrom::Start(from.max(len.saturating_sub(max)))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn reindex_finished(mgr: &NodeManager, datadir: &Path) -> bool {
    let mut mark = mgr.log_mark.lock().unwrap();
    let Some(m) = mark.as_mut() else { return false };
    let Ok(mut f) = std::fs::File::open(datadir.join("debug.log")) else { return false };
    let len = f.metadata().map(|x| x.len()).unwrap_or(0);
    if len < m.scanned.max(m.debug_log) {
        // freebankd shrank its log as it started: read it again from the top.
        m.debug_log = 0;
        m.scanned = 0;
    }
    let from = m.scanned.max(m.debug_log);
    // In steps of 4 MiB at most; the next read starts 64 bytes back, so a line split between two reads is seen whole.
    let until = len.min(from + (4 << 20));
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_err() || f.take(until - from).read_to_end(&mut buf).is_err() {
        return false;
    }
    m.scanned = until.saturating_sub(64).max(from);
    String::from_utf8_lossy(&buf).contains("Reindexing finished")
}

/// Why our node stopped, from what this run wrote: debug.log's error, else the last line it printed (where a
/// program the system refuses to run says why, e.g. "built for macOS 15.0 which is newer than running OS").
fn exit_reason(mgr: &NodeManager, datadir: &Path) -> Option<String> {
    let mark = *mgr.log_mark.lock().unwrap();
    let Some(m) = mark else { return last_log_line(datadir) };
    if let Some(why) = read_since(&datadir.join("debug.log"), m.debug_log, 16384).and_then(|t| log_line_in(&t)) {
        return Some(why);
    }
    let out = read_since(&mgr.app_dir.join("logs").join("freebankd.out"), m.out, 4096)?;
    out.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(400).collect())
}

async fn start_opts(mgr: &NodeManager, reindex: bool) -> Result<(), String> {
    mgr.still_here()?;
    // The node this app left running when it last closed is the one to use, if it still runs.
    background::adopt_now(mgr).await;
    let mut child = mgr.child.lock().await;
    if let Some(c) = child.as_mut() {
        if c.try_wait().ok().flatten().is_none() {
            return Ok(());
        }
    }
    if background::adopted_running(mgr) {
        return Ok(());
    }
    // A new start: why an earlier node stopped no longer applies. Cleared before the checks below,
    // so a start that fails here isn't reported as that old exit. (Only `reap` sets it, under the
    // child lock held here.)
    *mgr.last_exit.lock().unwrap() = None;
    let s = mgr.settings.lock().await.clone();
    // Hosted copies whose owner deleted them (v0.2.8): their wallet files move aside while the node is down.
    crate::phone::hosted::sweep_removed(&mgr.app_dir, Path::new(&s.datadir));
    let tag = s.installed_tag.clone().ok_or("FreeBank isn't installed yet.")?;
    let bin = mgr.freebankd(&tag);
    if !bin.is_file() {
        return Err(format!("{} is missing; please install again.", bin.display()));
    }
    if !mgr.verified(&tag) {
        return Err(format!(
            "FreeBank {} on this computer was installed before the app checked release signatures, so it \
             wasn't started. Download it again on the Node tab, or install again.",
            tag
        ));
    }
    // Another node in this folder, on whatever port: freebankd would refuse to start beside it.
    if let Some(u) = lock::in_use(Path::new(&s.datadir), &[]) {
        return Err(format!("{} Stop it first, or choose another data folder under Advanced.", u.say()));
    }
    if detect::port_busy(s.rpc_port) {
        return Err(format!(
            "Something is already using port {}. If it is another FreeBank node, stop it first, \
             or choose other ports under Advanced.",
            s.rpc_port
        ));
    }
    if detect::port_busy(s.p2p_port) {
        return Err(format!(
            "Something is already using port {}. Choose other ports under Advanced.",
            s.p2p_port
        ));
    }

    let logs = mgr.app_dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs.join("freebankd.out"))
        .map_err(|e| e.to_string())?;
    let err = out.try_clone().map_err(|e| e.to_string())?;

    let mut cmd = node_command(&bin);
    // Its own folder as working folder: not the app's, which in an AppImage is the AppImage's mount and would
    // keep it mounted as long as the node runs.
    if Path::new(&s.datadir).is_dir() {
        cmd.current_dir(&s.datadir);
    }
    cmd.arg(format!("-datadir={}", s.datadir))
        .arg("-server=1")
        .arg("-mainchaintransport=enforcer")
        .arg(format!("-enforceraddr={}", s.enforcer))
        .arg(format!("-mainchainrest={}", s.rest))
        .arg("-mainchainchain=main")
        .arg(format!("-mainchainblockpin={}:{}", PIN_HEIGHT, PIN_HASH))
        // Always explicit, so an rpcport= line in freebank.conf can't hide the node from us.
        .arg(format!("-rpcport={}", s.rpc_port))
        .arg(format!("-port={}", s.p2p_port))
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    if reindex {
        cmd.arg("-reindex");
    }
    let detach = s.keep_running;
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            if detach {
                // Its own session: it outlives the app, and nothing sent to the app's terminal or
                // process group reaches it.
                libc::setsid();
            } else {
                // If the app dies without a clean exit, the kernel asks the node to shut down too.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            }
            Ok(())
        });
    }
    let debug_log = file_len(&Path::new(&s.datadir).join("debug.log"));
    let out_len = file_len(&logs.join("freebankd.out"));
    let c = cmd
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {}", bin.display(), e))?;
    *mgr.log_mark.lock().unwrap() = Some(LogMark { debug_log, out: out_len, scanned: debug_log });
    mgr.detached.store(detach, Ordering::SeqCst);
    mgr.reindex.store(if reindex { REINDEX_RUNNING } else { REINDEX_IDLE }, Ordering::SeqCst);
    // So the next launch can recognise it, should it outlive the app (on purpose, or after a crash).
    if let Some(pid) = c.id() {
        let _ = background::write_pid(
            mgr,
            &PidFile {
                pid,
                datadir: s.datadir.clone(),
                rpc_port: s.rpc_port,
                tag,
                started: background::now(),
                detached: detach,
                left: None,
            },
        );
    }
    *child = Some(c);
    crate::activity::note(match (reindex, detach) {
        (true, _) => "node: started with -reindex",
        (false, true) => "node: started (keeps running when the app closes)",
        (false, false) => "node: started",
    });
    Ok(())
}

/// The node's command. Daemon mode on Linux: when FreeBank runs from its autostart entry, systemd keeps whatever it
/// starts in that entry's unit, which at logout gets SIGTERM and then SIGKILL after 5 seconds (and, before systemd
/// 250, is stopped as soon as FreeBank's background part exits). So there the node gets a scope of its own, with time to
/// shut down (`systemd-run --scope` runs it in place: the same process). Anywhere else, the node itself.
fn node_command(bin: &Path) -> tokio::process::Command {
    #[cfg(target_os = "linux")]
    {
        let autostart = std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|c| c.contains("autostart.service"));
        let systemd_run = Path::new("/usr/bin/systemd-run");
        if autostart && systemd_run.is_file() {
            let mut cmd = tokio::process::Command::new(systemd_run);
            cmd.args(["--user", "--scope", "--collect", "--quiet", "-p", "TimeoutStopSec=180", "--"]).arg(bin);
            return cmd;
        }
    }
    tokio::process::Command::new(bin)
}

/// Stop the node this app started: RPC `stop`, else SIGTERM (the RPC refuses during warm-up),
/// then wait for it to exit. Never SIGKILL: a hard kill risks the block index. A node recognised
/// from an earlier launch is stopped the same way (background::stop_adopted).
pub async fn stop(mgr: &NodeManager) -> Result<(), String> {
    crate::activity::note("node: stopping");
    let mut guard = mgr.child.lock().await;
    let Some(child) = guard.as_mut() else {
        drop(guard);
        return background::stop_adopted(mgr).await;
    };
    if child.try_wait().ok().flatten().is_some() {
        *guard = None;
        background::remove_pid(mgr);
        return Ok(());
    }
    let s = mgr.settings.lock().await.clone();
    let asked = detect::local_client(&mgr.http, &s)
        .call("stop", vec![])
        .await
        .is_ok();
    #[cfg(unix)]
    if !asked {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = asked;
    match tokio::time::timeout(Duration::from_secs(180), child.wait()).await {
        Ok(_) => {
            *guard = None;
            background::remove_pid(mgr);
            Ok(())
        }
        Err(_) => Err("FreeBank is still shutting down; give it a minute.".into()),
    }
}

/// The process ids of the node the app manages now (its child, or one recognised from an earlier
/// launch), for the lock checks: their locks are the app's own.
pub async fn managed_pids(mgr: &NodeManager) -> Vec<u32> {
    let mut pids = Vec::new();
    if let Some(c) = mgr.child.lock().await.as_mut() {
        if c.try_wait().ok().flatten().is_none() {
            pids.extend(c.id());
        }
    }
    if let Some(a) = mgr.adopted.lock().unwrap().as_ref() {
        pids.push(a.pid);
    }
    pids
}

/// Lines freebankd logs for every block that read like errors but aren't (v0.2.17,
/// validation.cpp:8886).
const LOG_NOISE: &[&str] = &["Failed to get latest withdrawal bundle from ldb"];

/// The last line of debug.log worth showing, without its timestamp: what the node is doing, for
/// the "warming up" screen, or once it has stopped, why. Skips freebankd's per-block noise. After
/// a stop ("Shutdown: In progress..." with no start since) it is the "Error: …" line before the
/// shutdown, or nothing for a clean stop.
pub(crate) fn last_log_line(datadir: &Path) -> Option<String> {
    log_line_in(&read_since(&datadir.join("debug.log"), 0, 8192)?)
}

/// last_log_line's choice within a piece of debug.log.
fn log_line_in(text: &str) -> Option<String> {
    // "2026-09-26 10:00:00 init message: Loading…" -> "Loading…"
    let msgs: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let b = line.as_bytes();
            let stamped = b.len() > 20 && b[4] == b'-' && b[10] == b' ' && b[13] == b':' && b[19] == b' ';
            let msg = if stamped { &line[20..] } else { line };
            msg.strip_prefix("init message: ").unwrap_or(msg)
        })
        .collect();
    let started = msgs.iter().rposition(|m| m.starts_with("FreeBank version v"));
    let stopped = msgs.iter().rposition(|m| m.starts_with("Shutdown: In progress"));
    match stopped {
        // A reason may be long ("… Restart with -reindex …" comes late in freebankd's). Most start with
        // "Error: "; the ones freebankd asks as a question in a GUI (v0.2.18's "on-disk records … are record
        // format 2 … Restart with -reindex") have an empty caption, so ": ".
        Some(s) if started.map_or(true, |b| s > b) => msgs[started.map_or(0, |b| b + 1)..s]
            .iter()
            .rev()
            .find_map(|m| m.strip_prefix(": ").or_else(|| m.starts_with("Error: ").then_some(*m)))
            .map(|m| m.chars().take(400).collect()),
        _ => msgs
            .iter()
            .rev()
            .find(|m| !LOG_NOISE.iter().any(|n| m.contains(n)))
            .map(|m| m.chars().take(160).collect()),
    }
}

/// The node this manager started (or recognised) still runs; Err with why once it has stopped (daemon mode's wake
/// watches a node it started this way).
pub async fn alive(mgr: &NodeManager) -> Result<(), String> {
    let datadir = PathBuf::from(mgr.settings.lock().await.datadir.clone());
    match reap(mgr, &datadir).await {
        (true, _) => Ok(()),
        (false, why) => Err(why.unwrap_or_else(|| "the node isn't running".into())),
    }
}

/// If our child has exited, forget it and say why (once; later polls read `last_exit`). The same
/// for a node recognised from an earlier launch, which has gone once it lets go of its data folder.
async fn reap(mgr: &NodeManager, datadir: &Path) -> (bool, Option<String>) {
    let mut guard = mgr.child.lock().await;
    let Some(child) = guard.as_mut() else {
        drop(guard);
        if background::adopted_running(mgr) {
            return (true, None);
        }
        if background::forget_adopted(mgr) {
            let why = last_log_line(datadir).unwrap_or_default();
            let msg = format!("FreeBank stopped. {}", why).trim().to_string();
            crate::activity::note("node: stopped");
            *mgr.last_exit.lock().unwrap() = Some(msg.clone());
            return (false, Some(msg));
        }
        return (false, mgr.last_exit.lock().unwrap().clone());
    };
    match child.try_wait() {
        Ok(None) => (true, None),
        Ok(Some(status)) => {
            let why = exit_reason(mgr, datadir).unwrap_or_default();
            let msg = format!("FreeBank stopped ({}). {}", status, why).trim().to_string();
            crate::activity::note(&format!("node: stopped ({status})"));
            *guard = None;
            background::remove_pid(mgr);
            *mgr.last_exit.lock().unwrap() = Some(msg.clone());
            (false, Some(msg))
        }
        Err(e) => (true, Some(e.to_string())),
    }
}

#[derive(Debug, Serialize)]
pub struct NodeProgress {
    /// This app's freebankd process is alive.
    pub running: bool,
    pub exited: Option<String>,
    pub rpc: detect::Probe,
    pub explorer_tip: Option<u64>,
    pub peers: Option<u64>,
    pub log_line: Option<String>,
    /// The node is rebuilding its data with -reindex, once, for a new release.
    pub reindexing: bool,
}

pub async fn progress(mgr: &NodeManager) -> NodeProgress {
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    background::adopt(mgr).await;
    let (running, exited) = reap_or_reindex(mgr, &datadir).await;
    let rpc = detect::probe(&mgr.http, &s).await;
    settle_reindex(mgr, &datadir, running && rpc.state == detect::RpcState::Up);
    let peers = if rpc.state == detect::RpcState::Up {
        detect::local_client(&mgr.http, &s)
            .call("getconnectioncount", vec![])
            .await
            .ok()
            .and_then(|v| v.as_u64())
    } else {
        None
    };
    NodeProgress {
        running,
        exited,
        rpc,
        explorer_tip: mgr.explorer_tip().await,
        peers,
        log_line: last_log_line(&datadir),
        reindexing: running && mgr.reindex.load(Ordering::SeqCst) == REINDEX_RUNNING,
    }
}

#[derive(Debug, Serialize)]
pub struct Peer {
    pub addr: String,
    pub subver: String,
    pub inbound: bool,
    pub synced_blocks: Option<i64>,
}

/// What the Node tab shows. `state` says which of the fields are filled: the chain numbers and
/// peers only when "up"; `activity` when "busy" (stopping, restarting, updating).
#[derive(Debug, Serialize)]
pub struct NodeStatus {
    pub state: detect::RpcState,
    pub activity: Option<String>,
    pub message: String,
    /// The app started this node (now, or before it last closed) and manages it.
    pub managed: bool,
    /// The app has a freebankd it can start.
    pub installed: bool,
    /// The installed freebankd was put there before the app checked release signatures, so it
    /// won't be started until it is downloaded and checked again.
    pub unverified: bool,
    /// The node was started before the app last closed, and recognised again at this launch.
    pub adopted: bool,
    /// For such a node: since when it has run without the app (unix seconds; when the app closed
    /// and left it, or when it started if the app didn't close cleanly).
    pub background_since: Option<u64>,
    /// "Keep FreeBank's node running after I close the app" is on.
    pub keep_running: bool,
    /// The node the app manages will keep running when the app closes (the setting is on, and it
    /// can: see background::outlives_now).
    pub keeps_running: bool,
    pub exited: Option<String>,
    pub log_line: Option<String>,
    /// The node is rebuilding its data with -reindex, once, for a new release.
    pub reindexing: bool,
    pub version: String,
    pub blocks: u64,
    pub headers: u64,
    pub explorer_tip: Option<u64>,
    pub l1_blocks: Option<u64>,
    pub peers: Vec<Peer>,
    pub tag: Option<String>,
    pub explorer: String,
    pub datadir: String,
    pub rest: String,
    pub enforcer: String,
    pub release: Option<String>,
    pub p2p_port: u16,
    /// False when freebank.conf says listen=0: no incoming peers.
    pub listens: bool,
    /// freebank.conf binds the peer port to this computer only.
    pub peers_local: bool,
    pub versions: Versions,
}

/// "FreeBank app 0.1.0 · node v0.2.15 (843ccae)"
#[derive(Debug, Serialize, Clone, Default)]
pub struct Versions {
    pub app: String,
    pub node: Option<String>,
    pub commit: Option<String>,
}

/// Is the node this app started still running? That covers a node it started before it last
/// closed and has recognised again (background.rs).
pub async fn child_alive(mgr: &NodeManager) -> bool {
    background::adopt(mgr).await;
    {
        let mut guard = mgr.child.lock().await;
        if guard
            .as_mut()
            .map(|c| c.try_wait().ok().flatten().is_none())
            .unwrap_or(false)
        {
            return true;
        }
    }
    background::adopted_running(mgr)
}

/// The running node's version: the installed release's own `-version` when the app started it,
/// else the node's user agent ("/FreeBank:0.2.15/") and the commit from its debug.log.
fn node_versions(mgr: &NodeManager, managed: bool, release: Option<&str>, subver: &str, datadir: &Path) -> Versions {
    let mut v = Versions {
        app: super::APP_VERSION.to_string(),
        ..Default::default()
    };
    if managed {
        if let Some((ver, commit)) = release.and_then(|t| mgr.release_version(t)) {
            v.node = Some(ver);
            v.commit = commit;
            return v;
        }
    }
    let from_agent = subver
        .trim_matches('/')
        .split_once(':')
        .map(|(_, n)| format!("v{}", n.split('(').next().unwrap_or(n).trim()));
    let from_log = last_version_in_log(datadir);
    match (from_agent, from_log) {
        (Some(a), Some((l, c))) if a == l => {
            v.node = Some(a);
            v.commit = c;
        }
        (Some(a), _) => v.node = Some(a),
        (None, Some((l, c))) if !subver.is_empty() => {
            v.node = Some(l);
            v.commit = c;
        }
        _ => {}
    }
    v
}

/// The newest "FreeBank version v…" line in the last 2 MB of debug.log.
fn last_version_in_log(datadir: &Path) -> Option<(String, Option<String>)> {
    let mut f = std::fs::File::open(datadir.join("debug.log")).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(2 << 20))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .rev()
        .find(|l| l.contains("FreeBank version v"))
        .and_then(super::parse_version_line)
}

pub async fn status(mgr: &NodeManager) -> Result<NodeStatus, String> {
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    let mut st = NodeStatus {
        state: detect::RpcState::Down,
        activity: None,
        message: String::new(),
        managed: false,
        installed: s
            .installed_tag
            .as_deref()
            .map(|t| mgr.freebankd(t).is_file())
            .unwrap_or(false),
        unverified: mgr.unverified(&s).is_some(),
        adopted: false,
        background_since: None,
        keep_running: s.keep_running,
        keeps_running: false,
        exited: None,
        log_line: None,
        reindexing: false,
        version: String::new(),
        blocks: 0,
        headers: 0,
        explorer_tip: None,
        l1_blocks: None,
        peers: Vec::new(),
        tag: conf_tag(&datadir),
        explorer: EXPLORER.to_string(),
        datadir: s.datadir.clone(),
        rest: s.rest.clone(),
        enforcer: s.enforcer.clone(),
        release: s.installed_tag.clone(),
        p2p_port: s.p2p_port,
        listens: std::fs::read_to_string(datadir.join("freebank.conf"))
            .map_or(true, |c| crate::security::parse_conf(&c).listen != Some(false)),
        peers_local: std::fs::read_to_string(datadir.join("freebank.conf"))
            .is_ok_and(|c| crate::security::parse_conf(&c).peers_local_only()),
        versions: Versions {
            app: super::APP_VERSION.to_string(),
            ..Default::default()
        },
    };
    // Mid-operation the node may not answer for a while; say what is happening instead.
    let activity = mgr.activity.lock().unwrap().clone();
    if let Some(a) = activity {
        st.state = detect::RpcState::Busy;
        st.activity = Some(a);
        st.log_line = last_log_line(&datadir);
        return Ok(st);
    }
    background::adopt(mgr).await;
    let (managed, exited) = reap_or_reindex(mgr, &datadir).await;
    st.managed = managed;
    st.exited = exited;
    if managed {
        if let Some(a) = mgr.adopted.lock().unwrap().clone() {
            st.adopted = true;
            st.background_since = Some(a.left.unwrap_or(a.started));
        }
        st.keeps_running = s.keep_running && background::outlives_now(mgr);
    }
    let probe = detect::probe(&mgr.http, &s).await;
    // Our node is alive but hasn't opened its RPC port yet: it is starting, not stopped. The same
    // for a node another program runs in our folder (it holds the folder's lock).
    st.state = match probe.state {
        detect::RpcState::Down if managed => detect::RpcState::Warming,
        detect::RpcState::Down if lock::in_use(&datadir, &[]).is_some() => detect::RpcState::Warming,
        other => other,
    };
    st.message = probe.message;
    settle_reindex(mgr, &datadir, managed && probe.state == detect::RpcState::Up);
    st.reindexing = managed && mgr.reindex.load(Ordering::SeqCst) == REINDEX_RUNNING;
    st.explorer_tip = mgr.explorer_tip().await;
    if st.state != detect::RpcState::Up {
        st.log_line = last_log_line(&datadir);
        // Starting or stopped: the release the app runs is the one to name.
        if managed || (st.installed && st.state == detect::RpcState::Down) {
            st.versions = node_versions(mgr, true, s.installed_tag.as_deref(), "", &datadir);
        }
        return Ok(st);
    }

    let c = detect::local_client(&mgr.http, &s);
    let (net, chain, peers, l1) = tokio::join!(
        c.call("getnetworkinfo", vec![]),
        c.call("getblockchaininfo", vec![]),
        c.call("getpeerinfo", vec![]),
        detect::l1_blocks(&mgr.http, &s.rest),
    );
    let chain = chain?;
    st.peers = peers?
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| Peer {
                    addr: p["addr"].as_str().unwrap_or("").to_string(),
                    subver: p["subver"].as_str().unwrap_or("").to_string(),
                    inbound: p["inbound"].as_bool().unwrap_or(false),
                    synced_blocks: p["synced_blocks"].as_i64(),
                })
                .collect()
        })
        .unwrap_or_default();
    st.version = net
        .ok()
        .and_then(|n| n["subversion"].as_str().map(String::from))
        .unwrap_or_default();
    st.blocks = chain["blocks"].as_u64().unwrap_or(0);
    st.headers = chain["headers"].as_u64().unwrap_or(0);
    st.l1_blocks = l1;
    st.versions = node_versions(mgr, managed, s.installed_tag.as_deref(), &st.version, &datadir);
    Ok(st)
}

/// Is a node the app didn't start answering on our port, or running in our data folder on another
/// port (it holds the folder's lock)?
pub(crate) async fn someone_elses_node(mgr: &NodeManager) -> bool {
    if child_alive(mgr).await {
        return false;
    }
    let s = mgr.settings.lock().await.clone();
    if lock::in_use(Path::new(&s.datadir), &[]).is_some() {
        return true;
    }
    detect::probe(&mgr.http, &s).await.state != detect::RpcState::Down
}

/// Change the name on your blocks: rewrite `coinbasetag=` and restart the node if the app runs it.
/// Returns "restarted", "external" (another program runs the node) or "saved" (not running).
pub async fn set_tag(mgr: &NodeManager, tag: &str) -> Result<&'static str, String> {
    mgr.still_here()?;
    install::validate_tag(tag)?;
    let s = mgr.settings.lock().await.clone();
    install::write_conf(Path::new(&s.datadir), tag)?;
    if child_alive(mgr).await {
        let _busy = mgr.busy("Restarting FreeBank with the new name…")?;
        stop(mgr).await?;
        start(mgr).await?;
        return Ok("restarted");
    }
    if someone_elses_node(mgr).await {
        return Ok("external");
    }
    Ok("saved")
}

/// Stop the app's node, as its own step on the Node tab.
pub async fn stop_managed(mgr: &NodeManager) -> Result<(), String> {
    let _busy = mgr.busy("Stopping FreeBank…")?;
    stop(mgr).await
}

#[derive(Debug, Serialize)]
pub struct Removed {
    pub datadir: String,
    /// The wallets left in the data folder (usually one).
    pub wallets: Vec<String>,
}

/// "Remove FreeBank": stop the app's node and delete what the app downloaded (releases, grpcurl).
/// Only paths inside the app's own data folder are touched; the node's data folder and wallet stay.
pub async fn remove_programs(mgr: &NodeManager) -> Result<Removed, String> {
    mgr.still_here()?;
    // Nothing to start at login once the programs are gone.
    crate::phone::login_item::set(&mgr.app_dir, false)?;
    let _busy = mgr.busy("Removing FreeBank's programs…")?;
    stop(mgr).await?;
    for name in ["releases", "tools", "tmp"] {
        super::remove_inside(&mgr.app_dir, &mgr.app_dir.join(name))?;
    }
    let mut s = mgr.settings.lock().await.clone();
    s.installed_tag = None;
    if s.grpcurl.as_deref().map(|g| !Path::new(g).is_file()).unwrap_or(false) {
        s.grpcurl = None;
    }
    mgr.save_settings(s.clone()).await?;
    Ok(Removed {
        wallets: super::wallet_files(Path::new(&s.datadir))
            .iter()
            .map(|w| w.to_string_lossy().into_owned())
            .collect(),
        datadir: s.datadir,
    })
}

/// "Delete chain data": stop the app's node, remove the blocks, chain state and indexes (FreeBank's
/// houses, bills and pools live under blocks/), keep the wallet and freebank.conf, then start again
/// so it re-syncs from its peers.
pub async fn delete_chain_data(mgr: &NodeManager) -> Result<(), String> {
    mgr.still_here()?;
    if someone_elses_node(mgr).await {
        return Err("This node was started by another program. Stop it there first.".into());
    }
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    if detect::check_datadir(&datadir).kind != "ours" {
        return Err(format!(
            "{} doesn't hold a FreeBank beta node's chain data, so nothing was deleted.",
            s.datadir
        ));
    }
    {
        let _busy = mgr.busy("Deleting chain data…")?;
        stop(mgr).await?;
        // Nothing may be using the folder now, whatever port it answers on.
        if let Some(u) = lock::in_use(&datadir, &[]) {
            return Err(format!("{} Stop it first. Nothing was deleted.", u.say()));
        }
        for name in super::CHAIN_DATA {
            super::remove_inside(&datadir, &datadir.join(name))?;
        }
    }
    let installed = s
        .installed_tag
        .as_deref()
        .map(|t| mgr.freebankd(t).is_file())
        .unwrap_or(false);
    if installed {
        start(mgr).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The warm-up line skips freebankd's per-block noise; after a stop it is the error, or nothing
    /// (not "Shutdown: done", which a protect stage used to show).
    #[test]
    fn the_log_line_skips_noise_and_shutdown() {
        let d = std::env::temp_dir().join(format!("fblogline-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let log = |lines: &[&str]| {
            let text: String = lines.iter().map(|l| format!("2026-09-29 10:48:47 {}\n", l)).collect();
            std::fs::write(d.join("debug.log"), format!("\n\n\n\n{}", text)).unwrap();
            last_log_line(&d)
        };
        let start = "FreeBank version v0.2.17.0-2afa30c (release build)";
        let tip = "UpdateTip: new best=19f4 height=402 progress=1.000000";
        let noise = "ConnectBlock: Failed to get latest withdrawal bundle from ldb: 0000!";
        let error = "Error: This datadir's undo data (blocks/rev*.dat) is record format 1, but this build reads and writes \
                     format 2. Restart with -reindex to regenerate it (-reindex-chainstate is NOT sufficient).";
        assert_eq!(log(&[start, "init message: Loading block index…"]).as_deref(), Some("Loading block index…"));
        assert_eq!(log(&[start, tip, noise]).as_deref(), Some(tip));
        let failed = [start, error, "Shutdown: In progress...", "net thread exit", "Shutdown: done"];
        assert_eq!(log(&failed).as_deref(), Some(error));
        assert_eq!(log(&[start, tip, noise, "Shutdown: In progress...", "Shutdown: done"]), None);
        // v0.2.18 on a v0.2.17 data folder (2026-10-03, the release build): an empty caption, then a line with no
        // time stamp.
        let format = ": This datadir's on-disk records (block index, blocks/rev*.dat, chainstate) are record format 2, \
                      but this build reads and writes format 3. Restart with -reindex to regenerate them \
                      (-reindex-chainstate is NOT sufficient)..";
        let refused = [start, "Opened LevelDB successfully", format, "Please restart with -reindex or -reindex-chainstate to recover.",
                       "Aborted block database rebuild. Exiting.", "Shutdown: In progress...", "Shutdown: done"];
        let why = log(&refused).unwrap();
        assert!(why.starts_with("This datadir's on-disk records"), "{why}");
        assert!(needs_reindex(&why));
        assert!(needs_reindex(error) && !needs_reindex(noise) && !needs_reindex(tip));
        // Started again since: the new run's line, not the old error.
        let again = [start, error, "Shutdown: In progress...", "Shutdown: done", start, "init message: Verifying blocks…"];
        assert_eq!(log(&again).as_deref(), Some("Verifying blocks…"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// A node that refuses its data folder ("Restart with -reindex", as v0.2.18 does on v0.2.17's) is started once
    /// more with -reindex by the next poll; a refusal that start doesn't cure is shown, and not tried again.
    #[tokio::test]
    async fn a_node_asking_for_reindex_is_started_once_with_it() {
        let (d, mgr) = super::super::testnode::manager("reidx", false).await;
        let datadir = d.join("node");
        let starts = || std::fs::read_to_string(datadir.join("fake-args.log")).unwrap_or_default();
        let poll = |want_up: bool| {
            let (mgr, datadir) = (&mgr, &datadir);
            async move {
                let until = std::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let (running, exited) = reap_or_reindex(mgr, datadir).await;
                    let s = mgr.settings.lock().await.clone();
                    if want_up && detect::probe(&mgr.http, &s).await.state == detect::RpcState::Up {
                        return (running, exited);
                    }
                    if !want_up && !running && mgr.reindex.load(Ordering::SeqCst) == REINDEX_GAVE_UP {
                        return (running, exited);
                    }
                    assert!(std::time::Instant::now() < until, "no settled state: {:?}", exited);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };

        std::fs::write(datadir.join("fake-old-format"), b"").unwrap();
        start(&mgr).await.unwrap();
        let _guard = poll(true).await;
        let pid = managed_pids(&mgr).await[0];
        let _kill = super::super::testnode::KillOnDrop { pid, datadir: datadir.clone() };
        let args = starts();
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines.len(), 2, "{args}");
        let has = |l: &str| l.split_whitespace().any(|a| a == "-reindex");
        assert!(!has(lines[0]) && has(lines[1]), "{args}");
        assert_eq!(mgr.reindex.load(Ordering::SeqCst), REINDEX_RUNNING, "up, but only progress/status settle it");
        stop(&mgr).await.unwrap();

        // Refused even with -reindex: one more start, then the reason, and no further starts.
        std::fs::write(datadir.join("fake-always-refuse"), b"").unwrap();
        start(&mgr).await.unwrap();
        let (running, exited) = poll(false).await;
        assert!(!running);
        assert!(exited.as_deref().is_some_and(needs_reindex), "{exited:?}");
        for _ in 0..5 {
            reap_or_reindex(&mgr, &datadir).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(starts().lines().count(), 4, "{}", starts());
        // The user's next Start tries once more.
        start(&mgr).await.unwrap();
        poll(false).await;
        assert_eq!(starts().lines().count(), 6, "{}", starts());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The stop reason comes from what this run wrote: an older run's "Restart with -reindex" doesn't count, and a
    /// program the system refused to run says why in freebankd.out. A -reindex start ends at "Reindexing finished",
    /// not when the RPC first answers.
    #[test]
    fn this_runs_reason_and_the_end_of_a_rebuild() {
        use std::io::Write;
        let d = std::env::temp_dir().join(format!("fbmark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("app/logs")).unwrap();
        let datadir = d.join("node");
        std::fs::create_dir_all(&datadir).unwrap();
        let old = "2026-10-03 01:40:31 FreeBank version v0.2.18.0-x (release build)\n\
                   2026-10-03 01:40:31 : This datadir's records are format 2. Restart with -reindex to regenerate them\n\
                   2026-10-03 01:40:31 Shutdown: In progress...\n2026-10-03 01:40:31 Shutdown: done\n";
        std::fs::write(datadir.join("debug.log"), old).unwrap();
        std::fs::write(d.join("app/logs/freebankd.out"), "earlier output\n").unwrap();
        let mgr = NodeManager::new(d.join("app"));
        *mgr.log_mark.lock().unwrap() = Some(LogMark { debug_log: old.len() as u64, out: 15, scanned: old.len() as u64 });
        assert_eq!(exit_reason(&mgr, &datadir), None, "nothing written since: not the old reason");
        let mut out = std::fs::OpenOptions::new().append(true).open(d.join("app/logs/freebankd.out")).unwrap();
        writeln!(out, "dyld[123]: Symbol not found: _foo\n  Referenced from: freebankd (built for macOS 15.0 which is newer than running OS)").unwrap();
        assert!(exit_reason(&mgr, &datadir).unwrap().contains("built for macOS 15.0"));
        let mut log = std::fs::OpenOptions::new().append(true).open(datadir.join("debug.log")).unwrap();
        writeln!(log, "2026-10-03 01:41:00 FreeBank version v0.2.18.0-x (release build)\n2026-10-03 01:41:01 Reindexing block file blk00000.dat...").unwrap();
        assert!(!reindex_finished(&mgr, &datadir));
        writeln!(log, "2026-10-03 01:42:13 Reindexing finished").unwrap();
        assert!(reindex_finished(&mgr, &datadir));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn failed_start_drops_the_old_exit() {
        let d = std::env::temp_dir().join(format!("fbstart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        // Settings of its own, so nothing points at a real data folder.
        let s = super::super::Settings { datadir: d.join("node").to_string_lossy().into_owned(), ..Default::default() };
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("settings.json"), serde_json::to_vec(&s).unwrap()).unwrap();
        let mgr = NodeManager::new(d.clone());
        // An earlier node stopped during warm-up; the next start fails before anything runs.
        *mgr.last_exit.lock().unwrap() = Some("FreeBank stopped (exit status: 1).".into());
        let err = start(&mgr).await.unwrap_err();
        assert!(err.contains("isn't installed"), "{}", err);
        // What the screen's poll reads as `exited`: nothing, so it keeps the new reason.
        assert_eq!(reap(&mgr, &d.join("node")).await, (false, None));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Finding 3 (the review's probe ran an unmarked release): a release an earlier app build
    /// unpacked without the signature check is never run, not even for -version.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unchecked_release_is_never_run() {
        use std::os::unix::fs::PermissionsExt;
        let (d, mgr) = super::super::testnode::manager("unchecked", false).await;
        let s = mgr.settings.lock().await.clone();
        let bin = mgr.freebankd("v0.2.16");
        let ran = d.join("ran");
        std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}'\necho 'FreeBank Daemon version v0.2.16.0-x'\n", ran.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_file(mgr.release_dir("v0.2.16").join(".verified")).unwrap();

        assert!(!mgr.can_start(&s));
        assert_eq!(mgr.unverified(&s).as_deref(), Some("v0.2.16"));
        let err = start(&mgr).await.unwrap_err();
        assert!(err.contains("before the app checked release signatures"), "{}", err);
        assert_eq!(mgr.release_version("v0.2.16"), None);
        let st = status(&mgr).await.unwrap();
        assert!(st.unverified && st.installed && !st.managed, "{:?}", st);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!ran.exists(), "the unchecked freebankd ran");

        // Once checked again (the marker fetch_release writes), it may start.
        std::fs::write(mgr.release_dir("v0.2.16").join(".verified"), "b19da93f\n").unwrap();
        assert!(mgr.can_start(&s));
        assert_eq!(mgr.unverified(&s), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Finding 6: a node in the app's data folder on another port (BitWindow's FreeBank uses the same
    /// folder) is seen by its lock: it is someone else's, the Node tab says a node is starting there,
    /// and the app won't start a second one.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_node_on_another_port_is_seen_by_its_lock() {
        use super::super::testnode::{free_port, manager, FakeNode, Opts};
        let (d, mgr) = manager("otherport", false).await;
        let s = mgr.settings.lock().await.clone();
        assert!(!someone_elses_node(&mgr).await);
        let node = FakeNode::spawn(Path::new(&s.datadir), free_port(), Opts::default());
        assert!(someone_elses_node(&mgr).await);
        let err = start(&mgr).await.unwrap_err();
        assert!(err.contains(&format!("process {}", node.pid())), "{}", err);
        let st = status(&mgr).await.unwrap();
        assert!(!st.managed && st.state == detect::RpcState::Warming, "{:?}", st);
        let err = delete_chain_data(&mgr).await.unwrap_err();
        assert!(err.contains("another program"), "{}", err);
        drop(node);
        assert!(!someone_elses_node(&mgr).await);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
