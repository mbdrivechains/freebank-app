//! "Keep your phone connected when FreeBank is closed" (operator, 2026-09-29: "can we have an option
//! that if a phone is conected then a question is asked : woul dyou like to spin off a damon so you
//! can connect on your phohen when out.."; in v0.2.1 by his choice, 2026-09-30).
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
//! It writes `<app data>/phone.pid`. When the app opens, it takes the link back: it stops the
//! background part (SIGTERM, and only a process whose command line is this one) and waits for it, so
//! two desktops never take turns at the relay room. "Stop everything and close" stops the node and
//! starts no background part. Never SIGKILL.

use super::{commands::NodeRpc, link, unix_now, Events, Phone};
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

/// The app folder, when this process was started as the background part.
pub fn requested(args: &[String]) -> Option<PathBuf> {
    match args {
        [_, a, d, dir] if a == ARG && d == "--app-dir" && !dir.is_empty() => Some(PathBuf::from(dir)),
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
        requested(&args).is_some_and(|d| d.to_string_lossy() == dir)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(out) = std::process::Command::new("/bin/ps").args(["-ww", "-o", "command=", "-p", &pid.to_string()]).output()
        else {
            return false;
        };
        // The whole end of its command line, so a folder named "<dir>X" doesn't count (review I7).
        let line = String::from_utf8_lossy(&out.stdout);
        line.trim_end().ends_with(&format!(" {ARG} --app-dir {dir}"))
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
pub fn main(app_dir: PathBuf) -> i32 {
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
    // line, read into room enough that it never moves (a move would leave a copy behind).
    let mut raw = Zeroizing::new(Vec::with_capacity(4097));
    let _ = std::io::stdin().take(4096).read_to_end(&mut raw);
    let pass = std::str::from_utf8(&raw)
        .ok()
        .map(|s| s.strip_suffix('\n').unwrap_or(s))
        .filter(|s| !s.is_empty())
        .map(|s| Zeroizing::new(s.to_string()));
    drop(raw);
    let code = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
        Ok(rt) => rt.block_on(run(app_dir.clone(), pass)),
        Err(_) => 1,
    };
    if read_pid(&app_dir).is_some_and(|p| p.pid == me.pid) {
        let _ = std::fs::remove_file(app_dir.join(PID_FILE));
    }
    code
}

async fn run(app_dir: PathBuf, pass: Option<Zeroizing<String>>) -> i32 {
    let s = crate::node::NodeManager::new(app_dir.clone()).settings.lock().await.clone();
    let mut client = FreeBankClient::default();
    client.configure_local(&format!("http://127.0.0.1:{}", s.rpc_port), s.datadir.clone().into());
    let Ok((phone, out)) = Phone::new(&app_dir, Arc::new(NodeRpc(Arc::new(Mutex::new(client)))), Arc::new(NoScreen), Arc::new(unix_now))
    else {
        return 1;
    };
    phone.set_background(true);
    if let Some(p) = pass {
        // Checked by unlocking once, as when it is turned on in the app; without it, sends wait for the app.
        let _ = phone.phone_send_on(p).await;
    }
    let link = tokio::spawn(link::run(phone.clone(), out));
    tokio::spawn(phone.clone().expire_forever());
    // Until stopped, or until the app takes the relay room back (the link ends then, L1).
    tokio::select! {
        _ = stopped() => {}
        _ = link => {}
    }
    phone.forget_passphrase();
    0
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

    #[test]
    fn only_its_own_command_line_starts_it() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(requested(&a(&["freebank", ARG, "--app-dir", "/x/y z"])), Some(PathBuf::from("/x/y z")));
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
}
