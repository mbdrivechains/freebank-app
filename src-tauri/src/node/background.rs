//! A node that outlives the app. With "Keep FreeBank's node running after I close the app" on
//! (Settings), the app starts its node in its own session and leaves it running when it closes.
//! Whenever the app starts its node it writes `<app data>/node.pid`, so a later launch can
//! recognise that node and manage it again (Stop, Update, Rename): re-adoption. That also covers a
//! node left behind when the app crashed (macOS has no parent-death signal). A node is adopted only
//! when all of these hold:
//! - the pid file names the data folder and RPC port the settings name now;
//! - that folder carries the app's mark (`.freebank-node`);
//! - the process in the pid file holds the folder's lock (`.lock`), so it is the node running there;
//! - the node answers RPC with the cookie in that folder.
//! Anything else stays "started by another program", as before.

use super::{detect, lock, process, NodeManager, Settings, DATADIR_MARK};
use crate::rpc::RpcError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// In the app's data folder.
pub const PID_FILE: &str = "node.pid";

/// What the app knows about the node it started.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PidFile {
    pub pid: u32,
    pub datadir: String,
    pub rpc_port: u16,
    /// The release it runs.
    pub tag: String,
    /// When it started, unix seconds.
    pub started: u64,
    /// Started in its own session, without the parent-death signal: it can outlive the app.
    pub detached: bool,
    /// When the app closed and left it running, unix seconds.
    #[serde(default)]
    pub left: Option<u64>,
}

/// The node from an earlier launch that this launch manages.
#[derive(Debug, Clone, PartialEq)]
pub struct Adopted {
    pub pid: u32,
    pub datadir: PathBuf,
    pub started: u64,
    pub left: Option<u64>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn read_pid(mgr: &NodeManager) -> Option<PidFile> {
    let bytes = std::fs::read(mgr.app_dir.join(PID_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write the pid file (only its owner can read it). Nothing is written once "Obliterate" has run.
pub fn write_pid(mgr: &NodeManager, pf: &PidFile) -> Result<(), String> {
    mgr.still_here()?;
    std::fs::create_dir_all(&mgr.app_dir).map_err(|e| e.to_string())?;
    let path = mgr.app_dir.join(PID_FILE);
    let tmp = mgr.app_dir.join(format!("{}.new", PID_FILE));
    let _ = std::fs::remove_file(&tmp);
    let json = serde_json::to_vec_pretty(pf).map_err(|e| e.to_string())?;
    let mut f = super::install::private_file(&tmp).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

pub fn remove_pid(mgr: &NodeManager) {
    let _ = std::fs::remove_file(mgr.app_dir.join(PID_FILE));
}

/// Is process `pid` alive? None when it is another user's (it can't be this app's node).
fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return Some(false);
        }
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return Some(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Some(false)
    }
}

/// What the checks say about the node in the pid file.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// It is the app's node: manage it.
    Adopt,
    /// It may be, but its RPC doesn't answer yet: look again later.
    NotYet,
    /// Something doesn't match: it stays "started by another program".
    NotOurs(&'static str),
    /// It has gone (or the process is something else now): the pid file is stale.
    Gone,
}

/// The checks, in order (see the module comment).
pub async fn check(http: &reqwest::Client, s: &Settings, pf: &PidFile) -> Verdict {
    if pf.datadir != s.datadir || pf.rpc_port != s.rpc_port {
        return Verdict::NotOurs("the settings name another data folder or port");
    }
    let datadir = Path::new(&s.datadir);
    if !datadir.join(DATADIR_MARK).is_file() {
        return Verdict::NotOurs("its data folder doesn't carry FreeBank's mark");
    }
    match pid_alive(pf.pid) {
        Some(false) => return Verdict::Gone,
        None => return Verdict::NotOurs("the process is another user's"),
        Some(true) => {}
    }
    match lock::holder(&datadir.join(".lock")) {
        Ok(Some(Some(pid))) if pid == pf.pid => {}
        // The folder is free, or another process has it: the node in the pid file isn't running there.
        Ok(None) | Ok(Some(Some(_))) => return Verdict::Gone,
        Ok(Some(None)) | Err(_) => return Verdict::NotOurs("the system doesn't say who holds the data folder"),
    }
    // It must answer with the cookie in its folder. Any JSON-RPC answer means the cookie was taken
    // (a node still loading answers -28 only after checking it).
    let client = detect::local_client(http, s).with_timeout(Duration::from_secs(3));
    match client.call_typed("getblockchaininfo", vec![]).await {
        Ok(_) | Err(RpcError::Rpc { .. }) => Verdict::Adopt,
        Err(RpcError::Http(401)) | Err(RpcError::Http(403)) => {
            Verdict::NotOurs("it doesn't take the RPC cookie in its data folder")
        }
        Err(_) => Verdict::NotYet,
    }
}

/// Look for the app's node from an earlier launch, and manage it if the checks pass. Nothing to do
/// (and nothing read but one file name) when a node is managed already or there is no pid file.
/// Tried at most every 2 s.
pub async fn adopt(mgr: &NodeManager) {
    if mgr.adopted.lock().unwrap().is_some() || !mgr.app_dir.join(PID_FILE).exists() {
        return;
    }
    {
        let mut last = mgr.adopt_tried.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        *last = Some(Instant::now());
    }
    adopt_now(mgr).await;
}

/// `adopt` without the wait between tries. Returns what the checks said (NotYet when a node is
/// managed already).
pub async fn adopt_now(mgr: &NodeManager) -> Verdict {
    if mgr.adopted.lock().unwrap().is_some() {
        return Verdict::NotYet;
    }
    // A child of this run is the app's node already. (try_lock: a start or stop under way holds it.)
    let child_running = match mgr.child.try_lock() {
        Ok(mut g) => g.as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none()),
        Err(_) => true,
    };
    if child_running {
        return Verdict::NotYet;
    }
    let Some(pf) = read_pid(mgr) else {
        return Verdict::NotYet;
    };
    let s = mgr.settings.lock().await.clone();
    let verdict = check(&mgr.http, &s, &pf).await;
    match verdict {
        Verdict::Adopt => {
            *mgr.adopted.lock().unwrap() = Some(Adopted {
                pid: pf.pid,
                datadir: PathBuf::from(&pf.datadir),
                started: pf.started,
                left: pf.left,
            });
            *mgr.last_exit.lock().unwrap() = None;
        }
        Verdict::Gone => remove_pid(mgr),
        Verdict::NotYet | Verdict::NotOurs(_) => {}
    }
    verdict
}

/// Is the adopted node still running (holding its data folder's lock)?
pub fn adopted_running(mgr: &NodeManager) -> bool {
    mgr.adopted
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|a| lock::datadir_holder(&a.datadir) == Some(a.pid))
}

/// Forget the adopted node if it has stopped. True when it was forgotten now.
pub fn forget_adopted(mgr: &NodeManager) -> bool {
    let mut a = mgr.adopted.lock().unwrap();
    let gone = a
        .as_ref()
        .is_some_and(|a| lock::datadir_holder(&a.datadir) != Some(a.pid));
    if gone {
        *a = None;
        remove_pid(mgr);
    }
    gone
}

/// Stop the adopted node: RPC `stop`, else SIGTERM, but only while the process still holds its
/// data folder's lock, so a process id reused by now is never signalled. Then wait (up to 3
/// minutes) for it to let go of the folder. Never SIGKILL.
pub async fn stop_adopted(mgr: &NodeManager) -> Result<(), String> {
    let Some(a) = mgr.adopted.lock().unwrap().clone() else {
        return Ok(());
    };
    let holds = || lock::datadir_holder(&a.datadir) == Some(a.pid);
    if holds() {
        let s = mgr.settings.lock().await.clone();
        let asked = detect::local_client(&mgr.http, &s).call("stop", vec![]).await.is_ok();
        #[cfg(unix)]
        if !asked && holds() {
            unsafe {
                libc::kill(a.pid as libc::pid_t, libc::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        let _ = asked;
        let until = Instant::now() + Duration::from_secs(180);
        while holds() {
            if Instant::now() > until {
                return Err("FreeBank is still shutting down; give it a minute.".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    *mgr.adopted.lock().unwrap() = None;
    remove_pid(mgr);
    Ok(())
}

/// Can the node the app manages outlive the app? One from an earlier launch has. One started now
/// can when it was started in its own session; on macOS always, as nothing there stops a child
/// when its parent ends. (On Linux a node started with the setting off gets the parent-death signal.)
pub fn outlives_now(mgr: &NodeManager) -> bool {
    adopted_running(mgr) || mgr.detached.load(Ordering::SeqCst) || !cfg!(target_os = "linux")
}

/// What happens to the app's node when the app closes: it is stopped, unless "Keep FreeBank's node
/// running after I close the app" is on and the node can outlive the app. Then it stays, and the
/// pid file records when the app left it.
pub async fn at_exit(mgr: &NodeManager) {
    let keep = mgr.settings.lock().await.keep_running;
    if keep && !mgr.obliterated.load(Ordering::SeqCst) && process::child_alive(mgr).await && outlives_now(mgr) {
        if let Some(mut pf) = read_pid(mgr) {
            pf.left = Some(now());
            let _ = write_pid(mgr, &pf);
        }
        return;
    }
    let _ = process::stop(mgr).await;
}

/// Sent with "quit-requested".
#[derive(Debug, Clone, Serialize)]
pub struct QuitAsk {
    /// The node keeps running after the app closes. False when it was started before the setting
    /// was on (on Linux it then stops with the app).
    pub outlives: bool,
}

/// Should the window's close be held, so the screen can first say what happens to the node? Only
/// with the setting on and the app's node running. A second close within a minute goes through, so
/// the window can always be closed. Some(outlives) when held. Called on the window's event loop,
/// so it never waits for a lock.
pub fn hold_close(mgr: &NodeManager) -> Option<bool> {
    if mgr.obliterated.load(Ordering::SeqCst) || !mgr.settings.try_lock().ok()?.keep_running {
        return None;
    }
    let outlives = if adopted_running(mgr) {
        true
    } else {
        let mut child = mgr.child.try_lock().ok()?;
        if !child.as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_none()) {
            return None;
        }
        mgr.detached.load(Ordering::SeqCst) || !cfg!(target_os = "linux")
    };
    let mut asked = mgr.close_asked.lock().unwrap();
    if asked.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
        *asked = None;
        return None;
    }
    *asked = Some(Instant::now());
    Some(outlives)
}

/// The window's close button (lib.rs): hold the close and tell the screen, when `hold_close` says so.
pub fn on_close(window: &tauri::Window, api: &tauri::CloseRequestApi) {
    use tauri::{Emitter, Manager};
    let Some(mgr) = window.try_state::<std::sync::Arc<NodeManager>>() else {
        return;
    };
    if let Some(outlives) = hold_close(&mgr) {
        api.prevent_close();
        let _ = window.emit("quit-requested", QuitAsk { outlives });
    }
}

/// Stop and start the app's node, so it starts the way the settings now say (in its own session
/// with "Keep running" on).
pub async fn restart(mgr: &NodeManager) -> Result<(), String> {
    mgr.still_here()?;
    let _busy = mgr.busy("Restarting FreeBank…")?;
    process::stop(mgr).await?;
    process::start(mgr).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::testnode::{self, FakeNode, KillOnDrop, Opts};
    use super::*;

    fn session(pid: u32) -> i32 {
        unsafe { libc::getsid(pid as libc::pid_t) }
    }

    /// Start the app's node and wait until it answers; stopped by pid when the guard drops.
    async fn start_node(mgr: &NodeManager) -> (u32, KillOnDrop) {
        process::start(mgr).await.unwrap();
        let s = mgr.settings.lock().await.clone();
        testnode::wait_for_port(s.rpc_port);
        let pids = process::managed_pids(mgr).await;
        assert_eq!(pids.len(), 1, "{:?}", pids);
        (pids[0], KillOnDrop { pid: pids[0], datadir: PathBuf::from(&s.datadir) })
    }

    #[tokio::test]
    async fn kept_running_after_the_app_closes_and_managed_again_at_the_next_launch() {
        let (d, mgr) = testnode::manager("keep", true).await;
        let s = mgr.settings.lock().await.clone();
        let datadir = PathBuf::from(&s.datadir);
        let (pid, _guard) = start_node(&mgr).await;
        // Its own session: it outlives the app, and the app's terminal can't stop it.
        assert_eq!(session(pid), pid as i32);
        let pf = read_pid(&mgr).unwrap();
        assert_eq!((pf.pid, pf.detached, pf.left, pf.tag.as_str()), (pid, true, None, "v0.2.16"));
        assert_eq!((pf.datadir.as_str(), pf.rpc_port), (s.datadir.as_str(), s.rpc_port));
        let st = process::status(&mgr).await.unwrap();
        assert!(st.managed && !st.adopted && st.keep_running && st.keeps_running, "{:?}", st);

        // Closing the window: the first close is held so the screen can say the node keeps
        // running; a second one goes through.
        assert_eq!(hold_close(&mgr), Some(true));
        assert_eq!(hold_close(&mgr), None);
        // The app exits: the node stays, and the pid file says when the app left it.
        at_exit(&mgr).await;
        assert_eq!(lock::datadir_holder(&datadir), Some(pid));
        let left = read_pid(&mgr).unwrap().left.expect("left");
        drop(mgr);

        // The next launch recognises it and manages it again.
        let mgr = NodeManager::new(d.join("app"));
        let st = process::status(&mgr).await.unwrap();
        assert!(st.managed && st.adopted && st.keeps_running, "{:?}", st);
        assert_eq!(st.background_since, Some(left));
        assert_eq!(st.state, detect::RpcState::Up);
        assert!(process::child_alive(&mgr).await);
        assert!(!process::someone_elses_node(&mgr).await);
        // Starting finds it running; nothing else starts.
        process::start(&mgr).await.unwrap();
        assert_eq!(process::managed_pids(&mgr).await, vec![pid]);
        // Rename, Update and Stop all go through stop(): it stops, and lets go of the folder.
        process::stop(&mgr).await.unwrap();
        assert_eq!(lock::datadir_holder(&datadir), None);
        assert!(read_pid(&mgr).is_none());
        let st = process::status(&mgr).await.unwrap();
        assert!(!st.managed && !st.adopted && st.state == detect::RpcState::Down, "{:?}", st);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn without_the_setting_the_node_stops_with_the_app() {
        let (d, mgr) = testnode::manager("nokeep", false).await;
        let datadir = PathBuf::from(&mgr.settings.lock().await.datadir);
        let (pid, _guard) = start_node(&mgr).await;
        // As before: in the app's session, and (on Linux) stopped by the kernel if the app dies.
        assert_ne!(session(pid), pid as i32);
        assert!(!read_pid(&mgr).unwrap().detached);
        let st = process::status(&mgr).await.unwrap();
        assert!(st.managed && !st.keep_running && !st.keeps_running, "{:?}", st);
        assert_eq!(hold_close(&mgr), None);
        at_exit(&mgr).await;
        assert_eq!(lock::datadir_holder(&datadir), None);
        assert!(read_pid(&mgr).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The macOS crash item: the app dies without its exit handler, the node runs on, and the next
    /// launch manages it instead of calling it "started by another program".
    #[tokio::test]
    async fn a_node_left_behind_by_a_crash_is_managed_again() {
        let (d, mgr) = testnode::manager("crash", false).await;
        let (pid, _guard) = start_node(&mgr).await;
        let started = read_pid(&mgr).unwrap().started;
        drop(mgr);
        let mgr = NodeManager::new(d.join("app"));
        let st = process::status(&mgr).await.unwrap();
        assert!(st.managed && st.adopted, "{:?}", st);
        // It didn't close cleanly, so "in the background since" is when it started.
        assert_eq!(st.background_since, Some(started));
        assert_eq!(process::managed_pids(&mgr).await, vec![pid]);
        process::stop(&mgr).await.unwrap();
        assert!(!process::child_alive(&mgr).await);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn only_the_apps_own_node_is_adopted() {
        let (d, mgr) = testnode::manager("notours", false).await;
        let s = mgr.settings.lock().await.clone();
        let datadir = PathBuf::from(&s.datadir);
        let node = FakeNode::spawn(&datadir, s.rpc_port, Opts::default());
        let pf = PidFile {
            pid: node.pid(),
            datadir: s.datadir.clone(),
            rpc_port: s.rpc_port,
            tag: "v0.2.16".into(),
            started: now(),
            detached: true,
            left: Some(now()),
        };
        assert_eq!(check(&mgr.http, &s, &pf).await, Verdict::Adopt);

        // A node that doesn't take the cookie in its folder isn't the app's.
        let cookie = std::fs::read(datadir.join(".cookie")).unwrap();
        std::fs::write(datadir.join(".cookie"), "__cookie__:not-this-one").unwrap();
        assert!(matches!(check(&mgr.http, &s, &pf).await, Verdict::NotOurs(_)));
        write_pid(&mgr, &pf).unwrap();
        assert!(matches!(adopt_now(&mgr).await, Verdict::NotOurs(_)));
        let st = process::status(&mgr).await.unwrap();
        assert!(!st.managed && !st.adopted, "{:?}", st);
        assert!(process::someone_elses_node(&mgr).await);
        // It stays on file: the checks run again (it might be the app's once the cookie is right).
        assert!(read_pid(&mgr).is_some());
        std::fs::write(datadir.join(".cookie"), &cookie).unwrap();

        // Another folder or port than the settings name now.
        let mut other = s.clone();
        other.datadir = d.join("elsewhere").to_string_lossy().into_owned();
        assert!(matches!(check(&mgr.http, &other, &pf).await, Verdict::NotOurs(_)));
        let mut other = s.clone();
        other.rpc_port = testnode::free_port();
        assert!(matches!(check(&mgr.http, &other, &pf).await, Verdict::NotOurs(_)));
        // A folder without FreeBank's mark.
        std::fs::remove_file(datadir.join(super::super::DATADIR_MARK)).unwrap();
        assert!(matches!(check(&mgr.http, &s, &pf).await, Verdict::NotOurs(_)));
        std::fs::write(datadir.join(super::super::DATADIR_MARK), b"").unwrap();

        // A process that doesn't hold the folder (its pid used again, say): the pid file is stale.
        let stale = PidFile { pid: std::process::id(), ..pf.clone() };
        assert_eq!(check(&mgr.http, &s, &stale).await, Verdict::Gone);
        write_pid(&mgr, &stale).unwrap();
        assert_eq!(adopt_now(&mgr).await, Verdict::Gone);
        assert!(read_pid(&mgr).is_none());
        // A process that has gone.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        assert_eq!(check(&mgr.http, &s, &PidFile { pid: gone_pid, ..pf.clone() }).await, Verdict::Gone);

        // All of it right again: adopted, and the settings form stays locked while it runs.
        write_pid(&mgr, &pf).unwrap();
        assert_eq!(adopt_now(&mgr).await, Verdict::Adopt);
        assert!(process::status(&mgr).await.unwrap().managed);
        drop(node);
        assert!(!process::child_alive(&mgr).await);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The same with the real freebankd, in a data folder and on ports of its own, with nothing
    /// listening beyond 127.0.0.1 (listen=0) and no peers (connect=0). Run by hand, with a
    /// freebankd whose release signature was checked:
    ///   FB_REAL_FREEBANKD=<freebankd> FB_REAL_GRPCURL=<grpcurl> [FB_REAL_ENFORCER=host:port
    ///   FB_REAL_REST=host:port] cargo test real_freebankd -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn real_freebankd_kept_running_and_managed_again() {
        let (Some(bin), Some(grpcurl)) = (std::env::var_os("FB_REAL_FREEBANKD"), std::env::var_os("FB_REAL_GRPCURL")) else {
            eprintln!("FB_REAL_FREEBANKD and FB_REAL_GRPCURL are not set: skipped");
            return;
        };
        let (d, mgr) = testnode::manager("real", true).await;
        std::fs::copy(&bin, mgr.freebankd("v0.2.16")).unwrap();
        let mut s = mgr.settings.lock().await.clone();
        s.grpcurl = Some(PathBuf::from(&grpcurl).to_string_lossy().into_owned());
        if let Some(e) = std::env::var_os("FB_REAL_ENFORCER") {
            s.enforcer = e.to_string_lossy().into_owned();
        }
        if let Some(r) = std::env::var_os("FB_REAL_REST") {
            s.rest = r.to_string_lossy().into_owned();
        }
        mgr.save_settings(s.clone()).await.unwrap();
        let datadir = PathBuf::from(&s.datadir);
        std::fs::write(datadir.join("freebank.conf"), "listen=0\nconnect=0\ncoinbasetag=freebank-test\n").unwrap();

        process::start(&mgr).await.unwrap();
        let pid = process::managed_pids(&mgr).await[0];
        let _guard = KillOnDrop { pid, datadir: datadir.clone() };
        // It holds its folder and answers RPC with the cookie there (warming up counts).
        let answers = |mgr: &NodeManager, s: &Settings| {
            let c = detect::local_client(&mgr.http, s).with_timeout(Duration::from_secs(5));
            async move { matches!(c.call_typed("getblockchaininfo", vec![]).await, Ok(_) | Err(RpcError::Rpc { .. })) }
        };
        let until = Instant::now() + Duration::from_secs(90);
        while !(lock::datadir_holder(&datadir) == Some(pid) && answers(&mgr, &s).await) {
            assert!(Instant::now() < until, "freebankd never answered; see {}", mgr.app_dir.join("logs/freebankd.out").display());
            assert!(process::child_alive(&mgr).await, "freebankd stopped: {:?}", process::status(&mgr).await.map(|st| st.exited));
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        eprintln!("freebankd {} holds {} and answers on 127.0.0.1:{}", pid, datadir.display(), s.rpc_port);
        assert_eq!(session(pid), pid as i32, "not in its own session");

        at_exit(&mgr).await;
        assert_eq!(lock::datadir_holder(&datadir), Some(pid), "it stopped with the app");
        drop(mgr);
        eprintln!("the app has closed; freebankd {} still runs", pid);

        let mgr = NodeManager::new(d.join("app"));
        let st = process::status(&mgr).await.unwrap();
        assert!(st.managed && st.adopted, "{:?}", st);
        eprintln!("next launch: managed again, state {:?}, in the background since {:?}", st.state, st.background_since);
        let t = Instant::now();
        process::stop(&mgr).await.unwrap();
        eprintln!("stopped in {:?}", t.elapsed());
        assert_eq!(lock::datadir_holder(&datadir), None);
        assert!(read_pid(&mgr).is_none());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Turning the setting on while the node runs: on Linux that node would still stop with the app,
    /// and the screen says so; a restart starts it in its own session.
    #[tokio::test]
    async fn a_restart_applies_the_setting() {
        let (d, mgr) = testnode::manager("restart", false).await;
        let (pid1, _g1) = start_node(&mgr).await;
        let mut s = mgr.settings.lock().await.clone();
        s.keep_running = true;
        mgr.save_settings(s).await.unwrap();
        let st = process::status(&mgr).await.unwrap();
        assert!(st.keep_running);
        if cfg!(target_os = "linux") {
            assert!(!st.keeps_running);
            assert_eq!(hold_close(&mgr), Some(false));
        }
        restart(&mgr).await.unwrap();
        let (pid2, _g2) = start_node(&mgr).await;
        assert_ne!(pid1, pid2);
        assert_eq!(session(pid2), pid2 as i32);
        assert!(process::status(&mgr).await.unwrap().keeps_running);
        process::stop(&mgr).await.unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }
}
