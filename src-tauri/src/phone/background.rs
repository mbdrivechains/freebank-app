//! "Keep your phone connected when FreeBank is closed" (v0.2.1).
//!
//! With the setting on, closing the window starts the app again without one, in its own session:
//! `freebank --phone-background --app-dir <dir>`. That background part runs the phone link alone,
//! against the node the app left running ("Keep running" comes with the setting). If phone sends are
//! on, the passphrase goes to it on its stdin, never to disk. In the background part:
//! - balance, history, receive and sends within the daily limit work as with the app open;
//! - nothing can pair (that needs the open app's pairing code);
//! - a send the open app would hold (over the limit, or with the wallet locked) is refused at once:
//!   nobody is there to confirm it (`Phone::set_background`).
//!
//! **Daemon mode (v0.2.4):** "Start when I log in" adds a login item
//! (`login_item.rs`) that starts this part with `--light` when the user logs in. Light, it reads no passphrase and
//! starts no node; the node starts when a paired phone asks for something (after its session and Face ID checked
//! out), and until the node answers the phone hears `ERR_STARTING`, as does a request that finds the node gone later
//! (it is woken again). If the node can't start, the phone hears so for two minutes before the next try. A node it
//! spawned stops again after `IDLE_STOP_SECS` with no phone asking. Without the app's passphrase, sends within the
//! limit work only for a wallet without one. On Linux, from its autostart entry, the node gets a systemd scope of its
//! own (`node::process::node_command`), so logging out gives it time to shut down.
//!
//! It writes `<app data>/phone.pid`. When the app opens, it takes the link back: it stops the
//! background part (SIGTERM, and only a process whose command line is this one) and waits for it, so
//! two desktops never take turns at the relay room. "Stop everything and close" stops the node and
//! starts no background part. Never SIGKILL.

use super::{commands::NodeRpc, link, unix_now, Events, Phone, RpcFail};
use crate::rpc::FreeBankClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// In the app's data folder.
pub const PID_FILE: &str = "phone.pid";
/// The argument that starts the background part.
pub const ARG: &str = "--phone-background";
/// Started at login (daemon mode): the node starts when a paired phone asks.
pub const LIGHT: &str = "--light";
/// A node the light part started stops after this long with no phone asking.
const IDLE_STOP_SECS: u64 = 30 * 60;
/// How long the light part waits for a node it started to answer.
const START_WAIT_SECS: u64 = 10 * 60;
/// How long the app waits for the background part to stop when it takes the link back.
const TAKE_BACK_WAIT: Duration = Duration::from_secs(10);
/// This app already started the background part (the close notice's "Keep the phone connected").
pub static HANDED_OVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PidFile {
    pub pid: u32,
    /// unix seconds
    pub started: u64,
}

/// The app folder, and whether it is light (started at login), when this process was started as the background part.
pub fn requested(args: &[String]) -> Option<(PathBuf, bool)> {
    match args {
        [_, a, d, dir] if a == ARG && d == "--app-dir" && !dir.is_empty() => Some((PathBuf::from(dir), false)),
        [_, a, d, dir, l] if a == ARG && d == "--app-dir" && !dir.is_empty() && l == LIGHT => Some((PathBuf::from(dir), true)),
        _ => None,
    }
}

/// Start the background part and hand it the phone-send passphrase, if there is one. Its pid.
/// From an AppImage, through the AppImage file itself (`$APPIMAGE`): the app's own program lives in
/// the AppImage's mount, which goes when the app does (code review note).
pub fn spawn(app_dir: &Path, pass: Option<Zeroizing<String>>) -> Result<u32, String> {
    let exe = match std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()) {
        Some(appimage) => appimage,
        None => std::env::current_exe().map_err(|e| format!("Couldn't find FreeBank's program: {e}"))?,
    };
    spawn_with(&exe, app_dir, pass)
}

fn spawn_with(exe: &Path, app_dir: &Path, pass: Option<Zeroizing<String>>) -> Result<u32, String> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    // The app's folder as working folder: not the app's own, which in an AppImage is the mount of the app that
    // is closing.
    cmd.current_dir(app_dir);
    cmd.arg(ARG).arg("--app-dir").arg(app_dir).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        // Its own session: it outlives the app, and no terminal's hangup reaches it.
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start FreeBank's background part: {e}"))?;
    let line = Zeroizing::new(format!("{}\n", pass.as_ref().map_or("", |p| p.as_str())));
    let handed = child.stdin.take().map(|mut s| s.write_all(line.as_bytes()));
    if !matches!(handed, Some(Ok(()))) {
        return Err("Couldn't hand the phone link to FreeBank's background part.".into());
    }
    Ok(child.id())
}

fn read_pid(app_dir: &Path) -> Option<PidFile> {
    serde_json::from_slice(&std::fs::read(app_dir.join(PID_FILE)).ok()?).ok()
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        pid != 0 && pid <= i32::MAX as u32 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Is `pid` this app's background part for `app_dir`? Its command line says so.
fn is_ours(pid: u32, app_dir: &Path) -> bool {
    let dir = app_dir.to_string_lossy();
    #[cfg(target_os = "linux")]
    {
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
        let args: Vec<String> = raw.split(|&b| b == 0).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        let args: Vec<String> = args.into_iter().filter(|a| !a.is_empty()).collect();
        requested(&args).is_some_and(|(d, _)| d.to_string_lossy() == dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(out) = std::process::Command::new("/bin/ps").args(["-ww", "-o", "command=", "-p", &pid.to_string()]).output()
        else {
            return false;
        };
        // The whole end of its command line, so a folder named "<dir>X" doesn't count (review I7).
        let line = String::from_utf8_lossy(&out.stdout);
        let line = line.trim_end();
        line.ends_with(&format!(" {ARG} --app-dir {dir}")) || line.ends_with(&format!(" {ARG} --app-dir {dir} {LIGHT}"))
    }
}

/// At the app's start: stop the background part if it runs, and wait for it, before the app's own
/// phone link starts. When it had started (unix seconds), for the screen; None if there was none.
/// Err if it wouldn't stop (it is left running; never SIGKILL).
pub fn take_back(app_dir: &Path) -> Result<Option<u64>, String> {
    let Some(pf) = read_pid(app_dir) else { return Ok(None) };
    if !alive(pf.pid) || !is_ours(pf.pid, app_dir) {
        let _ = std::fs::remove_file(app_dir.join(PID_FILE));
        return Ok(None);
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pf.pid as libc::pid_t, libc::SIGTERM);
    }
    let until = Instant::now() + TAKE_BACK_WAIT;
    while alive(pf.pid) && is_ours(pf.pid, app_dir) {
        if Instant::now() > until {
            return Err(format!(
                "FreeBank's background part (process {}) didn't stop, so your phone may switch between it and this \
                 window. Stop that process, then open FreeBank again.",
                pf.pid
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(app_dir.join(PID_FILE));
    Ok(Some(pf.started))
}

/// Events go nowhere: there is no screen.
struct NoScreen;

impl Events for NoScreen {
    fn emit(&self, _name: &str, _payload: Value) {}
}

/// The background part's whole life (main.rs). Its exit code.
pub fn main(app_dir: PathBuf, light: bool) -> i32 {
    // It may hold the wallet passphrase: no core dumps, and (Linux) no reading its memory from other
    // processes of the same user (security review L3).
    #[cfg(unix)]
    unsafe {
        let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        libc::setrlimit(libc::RLIMIT_CORE, &none);
        #[cfg(target_os = "linux")]
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }
    // The pid file first, before anything that can wait, so an app opened at once finds it and takes
    // the link back (security review L1).
    let me = PidFile { pid: std::process::id(), started: crate::node::background::now() };
    if std::fs::write(app_dir.join(PID_FILE), serde_json::to_vec(&me).unwrap_or_default()).is_err() {
        return 1;
    }
    // The passphrase, if the app handed one over: everything up to end of input, less the last new
    // line, read into room enough that it never moves (a move would leave a copy behind). Started at
    // login (light), nothing is handed over: stdin isn't read.
    let mut raw = Zeroizing::new(Vec::with_capacity(4097));
    if !light {
        let _ = std::io::stdin().take(4096).read_to_end(&mut raw);
    }
    let pass = std::str::from_utf8(&raw)
        .ok()
        .map(|s| s.strip_suffix('\n').unwrap_or(s))
        .filter(|s| !s.is_empty())
        .map(|s| Zeroizing::new(s.to_string()));
    drop(raw);
    let code = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(rt) => rt.block_on(run(app_dir.clone(), pass, light)),
        Err(_) => 1,
    };
    if read_pid(&app_dir).is_some_and(|p| p.pid == me.pid) {
        let _ = std::fs::remove_file(app_dir.join(PID_FILE));
    }
    code
}

async fn run(app_dir: PathBuf, pass: Option<Zeroizing<String>>, light: bool) -> i32 {
    crate::activity::init(&app_dir);
    let mgr = Arc::new(crate::node::NodeManager::new(app_dir.clone()));
    let s = mgr.settings.lock().await.clone();
    // Demo mode (v0.4.4): the node the app left running asks for eCash facts through the relay, which runs here now.
    if s.demo {
        if let Err(e) = crate::node::relay::ensure(&mgr).await {
            crate::activity::note(&format!("demo relay: {}", e));
        }
    }
    // Other wallets may be open in the node (several wallets, hosted ones): name the main one, as the app does.
    if !s.extra_wallets.is_empty() || s.main_wallet.is_some() {
        crate::rpc::set_main_wallet(Some(s.main_wallet.clone().unwrap_or_else(|| crate::wallets::MAIN.into())));
    }
    let mut client = FreeBankClient::default();
    client.configure_local(&format!("http://127.0.0.1:{}", s.rpc_port), s.datadir.clone().into());
    let mut probe = FreeBankClient::default();
    probe.configure_local(&format!("http://127.0.0.1:{}", s.rpc_port), s.datadir.clone().into());
    let node: Arc<dyn super::Rpc> = Arc::new(NodeRpc(Arc::new(Mutex::new(client))));
    let waker = light.then(|| {
        Arc::new(Waker {
            st: std::sync::Mutex::new(WakeState::default()),
            phone: std::sync::OnceLock::new(),
            mgr: mgr.clone(),
            probe: Arc::new(Mutex::new(probe)),
            ops: Mutex::new(()),
        })
    });
    let rpc: Arc<dyn super::Rpc> = match &waker {
        Some(w) => Arc::new(WakingRpc { inner: node, waker: w.clone() }),
        None => node,
    };
    let Ok((phone, out)) = Phone::new(&app_dir, rpc, Arc::new(NoScreen), Arc::new(unix_now)) else {
        return 1;
    };
    phone.set_background(true);
    phone.set_maker(Arc::new(crate::wallets::HostedMaker { mgr: mgr.clone() }));
    if let Some(w) = waker {
        crate::activity::note("daemon: started at login; the node starts when a phone asks");
        let _ = w.phone.set(Arc::downgrade(&phone));
        {
            let w = w.clone();
            tokio::spawn(async move {
                let up = answers(&w.probe).await;
                w.st.lock().unwrap().answering = up;
            });
        }
        let ws = w.clone();
        phone.set_waker(Arc::new(move || ws.asked()));
        tokio::spawn(w.idle_stop());
    }
    if let Some(p) = pass {
        // Checked by unlocking once, as when it is turned on in the app; without it, sends wait for the app.
        let _ = phone.phone_send_on(p).await;
    }
    let link = tokio::spawn(link::run(phone.clone(), out));
    tokio::spawn(phone.clone().expire_forever());
    tokio::spawn(phone.clone().resume_hosted());
    // Automatic updates (v0.4.2, opt in): the background part takes rounds too, so a copy whose window stays closed
    // still gets them. A lock file keeps it and the app from both going ahead.
    {
        let m = mgr.clone();
        tokio::spawn(async move {
            tokio::time::sleep(crate::app_update::AUTO_FIRST).await;
            loop {
                let g = m.clone();
                let progress = std::sync::Mutex::new(crate::app_update::Progress::default());
                crate::app_update::auto_round(&m.app_dir, &m.http, &progress, &move || g.still_here()).await;
                tokio::time::sleep(crate::app_update::AUTO_EVERY).await;
            }
        });
    }
    // Until stopped, or until the app takes the relay room back (the link ends then, L1).
    tokio::select! {
        _ = stopped() => {}
        _ = link => {}
    }
    phone.forget_passphrase();
    0
}

/// Daemon mode: start the node when a paired phone asks, report it starting until it answers (or why it couldn't
/// start), and stop a node this part spawned after `IDLE_STOP_SECS` with no phone asking. All of its state is under
/// one lock, so a wake finishing as a request arrives can't leave "starting" on (security review, v0.2.4).
struct Waker {
    st: std::sync::Mutex<WakeState>,
    phone: std::sync::OnceLock<std::sync::Weak<Phone>>,
    mgr: Arc<crate::node::NodeManager>,
    probe: Arc<Mutex<FreeBankClient>>,
    /// A wake or an idle stop is under way: one at a time.
    ops: Mutex<()>,
}

#[derive(Default)]
struct WakeState {
    /// The node answered since a request last reached none.
    answering: bool,
    waking: bool,
    /// The last start failed: when, and what the phone hears until it is tried again.
    failed: Option<(Instant, String)>,
    /// This part spawned the node that runs (an adopted or already running one is never idle-stopped).
    spawned: bool,
    last_ask: Option<Instant>,
}

/// After a failed start, the next try waits this long.
const RETRY_AFTER: Duration = Duration::from_secs(120);
const WAKE_FAILED_TEXT: &str = "Your desktop couldn't start FreeBank's node. Open FreeBank there to see why.";

impl Waker {
    fn phone(&self) -> Option<Arc<Phone>> {
        self.phone.get().and_then(|w| w.upgrade())
    }

    /// A paired phone asked (from `serve()`, before its request runs): start the node unless it answers, a wake is
    /// under way, or the last start failed a moment ago.
    fn asked(self: &Arc<Self>) {
        let mut s = self.st.lock().unwrap();
        s.last_ask = Some(Instant::now());
        if s.answering || s.waking || s.failed.as_ref().is_some_and(|(at, _)| at.elapsed() < RETRY_AFTER) {
            return;
        }
        s.failed = None;
        s.waking = true;
        if let Some(p) = self.phone() {
            p.set_wake_failure(None);
            p.set_starting(true);
        }
        drop(s);
        let me = self.clone();
        tokio::spawn(async move { me.wake().await });
    }

    /// A request reached no node: it no longer answers. What that request answers instead.
    fn unreachable(self: &Arc<Self>) -> RpcFail {
        self.st.lock().unwrap().answering = false;
        self.asked();
        let s = self.st.lock().unwrap();
        match &s.failed {
            Some((_, why)) if !s.waking => RpcFail::rpc(super::WAKE_FAILED, why.clone()),
            _ => RpcFail::rpc(super::WAKE_STARTING, String::new()),
        }
    }

    async fn wake(self: Arc<Self>) {
        let _one = self.ops.lock().await;
        let outcome: Result<(), String> = async {
            if answers(&self.probe).await {
                return Ok(());
            }
            crate::activity::note("daemon: a phone asked; starting the node");
            let had = self.mgr.child.lock().await.is_some();
            crate::node::process::start(&self.mgr).await?;
            if !had && self.mgr.child.lock().await.is_some() {
                self.st.lock().unwrap().spawned = true;
            }
            let until = Instant::now() + Duration::from_secs(START_WAIT_SECS);
            loop {
                if answers(&self.probe).await {
                    return Ok(());
                }
                // It started and stopped again (it may need -reindex, say).
                crate::node::process::alive(&self.mgr).await?;
                if Instant::now() >= until {
                    return Err("it didn't answer within 10 minutes".into());
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        .await;
        let mut s = self.st.lock().unwrap();
        s.waking = false;
        let phone = self.phone();
        match outcome {
            Ok(()) => {
                s.answering = true;
                s.failed = None;
            }
            Err(e) => {
                crate::activity::note(&format!("daemon: the node didn't start: {e}"));
                s.answering = false;
                s.failed = Some((Instant::now(), WAKE_FAILED_TEXT.into()));
            }
        }
        if let Some(p) = phone {
            p.set_wake_failure(s.failed.as_ref().map(|(_, why)| why.clone()));
            p.set_starting(false);
        }
    }

    /// Every minute: a node this part spawned stops after `IDLE_STOP_SECS` with no phone asking.
    async fn idle_stop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let Ok(_one) = self.ops.try_lock() else { continue };
            let due = {
                let s = self.st.lock().unwrap();
                s.spawned && !s.waking && s.last_ask.is_none_or(|t| t.elapsed() >= Duration::from_secs(IDLE_STOP_SECS))
            };
            if !due {
                continue;
            }
            {
                let mut s = self.st.lock().unwrap();
                s.answering = false;
                s.spawned = false;
            }
            let _ = crate::node::process::stop(&self.mgr).await;
            crate::activity::note("daemon: no phone for 30 minutes; the node stopped");
        }
    }
}

/// The node, for the light part: a call that reaches none wakes it, and answers "starting" (or why it couldn't start).
struct WakingRpc {
    inner: Arc<dyn super::Rpc>,
    waker: Arc<Waker>,
}

impl super::Rpc for WakingRpc {
    fn call<'a>(&'a self, method: &'a str, params: Vec<Value>) -> futures_util::future::BoxFuture<'a, Result<Value, RpcFail>> {
        Box::pin(async move {
            match self.inner.call(method, params).await {
                // No node had the call (none listening, or warming up): wake it, and say it's starting.
                Err(e) if e.unreachable() && !e.maybe => Err(self.waker.unreachable()),
                // The node may have had it and gone (the review of v0.2.7): wake it, but keep the failure as it is,
                // so a payment that may have gone out says so and keeps its count.
                Err(e) if e.unreachable() => {
                    let _ = self.waker.unreachable();
                    Err(e)
                }
                r => r,
            }
        })
    }

    fn call_in<'a>(
        &'a self,
        wallet: &'a str,
        method: &'a str,
        params: Vec<Value>,
    ) -> futures_util::future::BoxFuture<'a, Result<Value, RpcFail>> {
        Box::pin(async move {
            match self.inner.call_in(wallet, method, params).await {
                Err(e) if e.unreachable() && !e.maybe => Err(self.waker.unreachable()),
                Err(e) if e.unreachable() => {
                    let _ = self.waker.unreachable();
                    Err(e)
                }
                r => r,
            }
        })
    }
}

/// The node answers RPC (not just running: warm-up says no).
async fn answers(probe: &Arc<Mutex<FreeBankClient>>) -> bool {
    tokio::time::timeout(Duration::from_secs(5), async { probe.lock().await.call_fresh_typed("getblockcount", vec![]).await.is_ok() })
        .await
        .unwrap_or(false)
}

/// SIGTERM (the app taking the link back, or the system shutting down) or Ctrl-C.
async fn stopped() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::Rpc;

    /// A node that fails every call so.
    struct Fails(RpcFail);

    impl super::super::Rpc for Fails {
        fn call<'a>(&'a self, _: &'a str, _: Vec<Value>) -> super::super::BoxFuture<'a, Result<Value, RpcFail>> {
            let e = self.0.clone();
            Box::pin(async move { Err(e) })
        }
    }

    /// The review of v0.2.7: in daemon mode a call that reached no node says the node is starting (or why it
    /// couldn't), but one the node may have had keeps its failure, so a payment that may have gone out says so and
    /// keeps its count.
    #[tokio::test]
    async fn waking_hides_only_calls_that_reached_no_node() {
        let dir = std::env::temp_dir().join(format!("fb-waking-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let waker = Arc::new(Waker {
            // A start failed just now: no new wake is tried, and calls that reach no node hear why.
            st: std::sync::Mutex::new(WakeState { failed: Some((Instant::now(), "It couldn't start.".into())), ..Default::default() }),
            phone: std::sync::OnceLock::new(),
            mgr: Arc::new(crate::node::NodeManager::new(dir.clone())),
            probe: Arc::new(Mutex::new(FreeBankClient::default())),
            ops: Mutex::new(()),
        });
        let refused = WakingRpc { inner: Arc::new(Fails(RpcFail::refused("connection refused"))), waker: waker.clone() };
        let e = refused.call("sendtoaddress", vec![]).await.unwrap_err();
        assert_eq!((e.code, e.maybe), (Some(super::super::WAKE_FAILED), false));
        let warming = WakingRpc { inner: Arc::new(Fails(RpcFail::rpc(super::super::RPC_IN_WARMUP, "Loading"))), waker: waker.clone() };
        assert_eq!(warming.call("getbalance", vec![]).await.unwrap_err().code, Some(super::super::WAKE_FAILED));
        let dropped = WakingRpc { inner: Arc::new(Fails(RpcFail::other("connection closed before message completed"))), waker };
        let e = dropped.call("sendtoaddress", vec![]).await.unwrap_err();
        assert_eq!((e.code, e.maybe), (None, true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_its_own_command_line_starts_it() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(requested(&a(&["freebank", ARG, "--app-dir", "/x/y z"])), Some((PathBuf::from("/x/y z"), false)));
        assert_eq!(requested(&a(&["freebank", ARG, "--app-dir", "/x/y z", LIGHT])), Some((PathBuf::from("/x/y z"), true)));
        assert_eq!(requested(&a(&["freebank"])), None);
        assert_eq!(requested(&a(&["freebank", ARG])), None);
        assert_eq!(requested(&a(&["freebank", ARG, "--app-dir", ""])), None);
        assert_eq!(requested(&a(&["freebank", ARG, "--app-dir", "/x", "more"])), None);
    }

    /// The real program: started as the background part, it writes its pid file; taking the link back
    /// stops it (SIGTERM) and it removes the file. Run by hand after a build:
    /// `FREEBANK_BIN=<target>/debug/freebank cargo test --lib real_background_part -- --ignored`
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn real_background_part_starts_and_is_taken_back() {
        let bin = PathBuf::from(std::env::var("FREEBANK_BIN").expect("FREEBANK_BIN"));
        let d = std::env::temp_dir().join(format!("fbphonebg-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // Its own settings: a scratch node folder and a port nothing listens on, never the real node's.
        let s = crate::node::Settings { datadir: d.join("node").to_string_lossy().into_owned(), rpc_port: 9, ..Default::default() };
        std::fs::write(d.join("settings.json"), serde_json::to_vec(&s).unwrap()).unwrap();
        let pid = spawn_with(&bin, &d, Some(Zeroizing::new("not a real passphrase".into()))).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        while read_pid(&d).is_none() {
            assert!(Instant::now() < until, "no phone.pid");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(read_pid(&d).unwrap().pid, pid);
        assert!(is_ours(pid, &d));
        assert!(!is_ours(pid, &d.join("other")));
        let started = take_back(&d).unwrap().expect("it was running");
        assert!(started > 0);
        assert!(!d.join(PID_FILE).exists());
        // Reaped by init (its own session), so gone rather than a zombie of this test: wait a moment.
        std::thread::sleep(Duration::from_millis(300));
        let gone = std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |st| st.contains(") Z "));
        assert!(gone, "still running");
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Not ours, or gone: the pid file goes and nothing is signalled.
    #[cfg(unix)]
    #[test]
    fn a_stale_or_foreign_pid_file_is_dropped() {
        let d = std::env::temp_dir().join(format!("fbphonebg-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // This test process: alive, but not a background part.
        let me = PidFile { pid: std::process::id(), started: 1 };
        std::fs::write(d.join(PID_FILE), serde_json::to_vec(&me).unwrap()).unwrap();
        assert_eq!(take_back(&d), Ok(None));
        assert!(!d.join(PID_FILE).exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Daemon mode: a node that can't start (here: none installed) is reported as such, not as "starting" for good, and
    /// isn't tried again for a while (security review, v0.2.4).
    #[tokio::test]
    async fn a_node_that_cant_start_says_so_and_waits_before_trying_again() {
        let dir = std::env::temp_dir().join(format!("fb-wake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut probe = FreeBankClient::default();
        probe.configure("http://127.0.0.1:1", "u", "p"); // nothing answers there
        let w = Arc::new(Waker {
            st: std::sync::Mutex::new(WakeState::default()),
            phone: std::sync::OnceLock::new(),
            mgr: Arc::new(crate::node::NodeManager::new(dir.clone())),
            probe: Arc::new(Mutex::new(probe)),
            ops: Mutex::new(()),
        });
        assert_eq!(w.unreachable().code, Some(super::super::WAKE_STARTING), "a wake is under way");
        for _ in 0..200 {
            if !w.st.lock().unwrap().waking {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let r = w.unreachable();
        assert_eq!((r.code, r.plain()), (Some(super::super::WAKE_FAILED), WAKE_FAILED_TEXT.to_string()));
        assert!(!w.st.lock().unwrap().waking, "not tried again at once");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
