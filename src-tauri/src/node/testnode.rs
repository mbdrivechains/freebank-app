//! A stand-in for freebankd in tests: another process (this test binary, run again) that locks the
//! data folder as freebankd does (an fcntl write lock on `.lock`, and on `.walletlock` if asked),
//! writes an RPC cookie and answers JSON-RPC with it. It keeps the cookie it wrote, so a cookie
//! changed on disk later is a wrong one to it, as it would be to freebankd. `stop` ends it.

use super::NodeManager;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A fresh temp folder for one test.
pub fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fbtn-{}-{}-{}", name, std::process::id(), rand::random::<u32>()));
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A local port nothing listens on (right now).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[derive(Debug, Clone, Default)]
pub struct Opts {
    /// Also lock `<this>/.walletlock`.
    pub walletlock: Option<PathBuf>,
    /// Write no cookie.
    pub no_cookie: bool,
    /// Answer every call with "still loading" (-28), as a warming node does.
    pub warming: bool,
}

impl Opts {
    fn env(&self) -> Vec<(&'static str, String)> {
        let mut v = vec![("FB_FAKE_NODE", "1".to_string())];
        if let Some(w) = &self.walletlock {
            v.push(("FB_FAKE_WALLETLOCK", w.to_string_lossy().into_owned()));
        }
        if self.no_cookie {
            v.push(("FB_FAKE_NO_COOKIE", "1".into()));
        }
        if self.warming {
            v.push(("FB_FAKE_WARMING", "1".into()));
        }
        v
    }
}

const MAIN: &str = "node::testnode::fake_node_main";

/// Wait until something answers on `port` (the fake node is up), for at most 20 s.
pub fn wait_for_port(port: u16) {
    let until = Instant::now() + Duration::from_secs(20);
    while !super::detect::port_busy(port) {
        assert!(Instant::now() < until, "the fake node never opened port {}", port);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A fake node run directly by the test (not through the app). Killed when dropped.
pub struct FakeNode {
    child: std::process::Child,
}

impl FakeNode {
    pub fn spawn(datadir: &Path, rpc_port: u16, opts: Opts) -> Self {
        std::fs::create_dir_all(datadir).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", MAIN, "--ignored", "--nocapture", "--test-threads", "1"])
            .envs(opts.env())
            .env("FB_FAKE_ARGS", format!("-datadir={} -rpcport={}", datadir.display(), rpc_port))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        wait_for_port(rpc_port);
        Self { child }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for FakeNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A fake node started through the app (so not this test's child): stopped when dropped, but only
/// while it still holds its data folder's lock, so a pid used again by now is never signalled.
pub struct KillOnDrop {
    pub pid: u32,
    pub datadir: PathBuf,
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if super::lock::datadir_holder(&self.datadir) == Some(self.pid) {
            unsafe {
                libc::kill(self.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Put the fake node where the app runs freebankd from (releases/<tag>/freebank/bin/freebankd), as
/// a release this code checked (`.verified`), and a grpcurl that runs. Returns grpcurl's path.
pub fn install_fake_release(mgr: &NodeManager, tag: &str, opts: &Opts) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = mgr.freebankd(tag);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let env: String = opts.env().iter().map(|(k, v)| format!("{}='{}' ", k, v)).collect();
    let script = format!(
        "#!/bin/sh\n{}FB_FAKE_ARGS=\"$*\" exec '{}' --exact {} --ignored --nocapture --test-threads 1\n",
        env,
        std::env::current_exe().unwrap().display(),
        MAIN
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(mgr.release_dir(tag).join(".verified"), "fake\n").unwrap();
    let grpcurl = mgr.app_dir.join("fake-grpcurl");
    std::fs::write(&grpcurl, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&grpcurl, std::fs::Permissions::from_mode(0o755)).unwrap();
    grpcurl
}

/// A NodeManager on fresh temp folders: the app's folder, a data folder the app set up (with its
/// mark), the fake node installed as release v0.2.16 (checked), two free ports, and `keep_running`.
/// Returns the base folder and the manager.
pub async fn manager(name: &str, keep_running: bool) -> (PathBuf, NodeManager) {
    let d = temp(name);
    let datadir = d.join("node");
    std::fs::create_dir_all(&datadir).unwrap();
    std::fs::write(datadir.join(super::DATADIR_MARK), b"").unwrap();
    let mgr = NodeManager::new(d.join("app"));
    let grpcurl = install_fake_release(&mgr, "v0.2.16", &Opts::default());
    let rpc_port = free_port();
    let p2p_port = loop {
        let p = free_port();
        if p != rpc_port {
            break p;
        }
    };
    let s = super::Settings {
        datadir: datadir.to_string_lossy().into_owned(),
        installed_tag: Some("v0.2.16".into()),
        grpcurl: Some(grpcurl.to_string_lossy().into_owned()),
        rpc_port,
        p2p_port,
        keep_running,
        ..Default::default()
    };
    mgr.save_settings(s).await.unwrap();
    (d, mgr)
}

/// The fake node itself, when this binary is run as one (FB_FAKE_NODE set); a no-op otherwise.
#[test]
#[ignore]
fn fake_node_main() {
    if std::env::var_os("FB_FAKE_NODE").is_none() {
        return;
    }
    let args = std::env::var("FB_FAKE_ARGS").unwrap_or_default();
    if args.split_whitespace().any(|a| a == "-version") {
        println!("FreeBank Daemon version v0.2.16.0-fake");
        return;
    }
    let get = |k: &str| args.split_whitespace().find_map(|a| a.strip_prefix(k)).map(String::from);
    let datadir = PathBuf::from(get("-datadir=").expect("-datadir"));
    let port: u16 = get("-rpcport=").expect("-rpcport").parse().unwrap();
    std::fs::create_dir_all(&datadir).unwrap();
    // Every start's arguments, for the -reindex tests.
    let mut started = std::fs::OpenOptions::new().create(true).append(true).open(datadir.join("fake-args.log")).unwrap();
    writeln!(started, "{}", args).unwrap();
    // As freebankd v0.2.18 on a folder v0.2.17 wrote (`fake-old-format`): refused until a start with -reindex
    // (`fake-always-refuse`: refused even then). Its log as the release build's (2026-10-03).
    let reindex = args.split_whitespace().any(|a| a == "-reindex");
    if datadir.join("fake-always-refuse").exists() || (datadir.join("fake-old-format").exists() && !reindex) {
        let mut log = std::fs::OpenOptions::new().create(true).append(true).open(datadir.join("debug.log")).unwrap();
        for l in [
            "FreeBank version v0.2.18.0-fake (release build)",
            ": This datadir's on-disk records (block index, blocks/rev*.dat, chainstate) are record format 2, but this \
             build reads and writes format 3. Restart with -reindex to regenerate them (-reindex-chainstate is NOT sufficient)..",
            "Aborted block database rebuild. Exiting.",
            "Shutdown: In progress...",
            "Shutdown: done",
        ] {
            writeln!(log, "2026-10-03 01:40:31 {}", l).unwrap();
        }
        std::process::exit(1);
    }
    if reindex {
        let _ = std::fs::remove_file(datadir.join("fake-old-format"));
    }
    // As freebankd: no second node on one folder.
    lock_or_exit(&datadir.join(".lock"));
    if let Some(w) = std::env::var_os("FB_FAKE_WALLETLOCK") {
        lock_or_exit(&PathBuf::from(w).join(".walletlock"));
    }
    let cookie = format!("__cookie__:{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
    if std::env::var_os("FB_FAKE_NO_COOKIE").is_none() {
        std::fs::write(datadir.join(".cookie"), &cookie).unwrap();
    }
    let warming = std::env::var_os("FB_FAKE_WARMING").is_some();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let want = {
        use base64::Engine;
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(&cookie))
    };
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        let mut reader = BufReader::new(conn.try_clone().unwrap());
        let (mut line, mut len, mut auth) = (String::new(), 0usize, String::new());
        loop {
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim_end().is_empty() {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("authorization:") {
                auth = line["authorization:".len()..].trim().to_string();
            }
        }
        let mut body = vec![0u8; len];
        if reader.read_exact(&mut body).is_err() {
            continue;
        }
        if auth != want {
            let _ = conn.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let method = req["method"].as_str().unwrap_or("");
        let answer: Result<serde_json::Value, (i64, &str)> = match method {
            _ if warming && method != "stop" => Err((-28, "Loading block index...")),
            "getblockchaininfo" => Ok(serde_json::json!({
                "chain": "main", "blocks": 10, "headers": 10, "initialblockdownload": false
            })),
            "getnetworkinfo" => Ok(serde_json::json!({"subversion": "/FreeBank:0.2.16/"})),
            "getpeerinfo" => Ok(serde_json::json!([])),
            "getconnectioncount" => Ok(serde_json::json!(0)),
            "getwalletinfo" => Ok(serde_json::json!({"balance": 0.0, "unconfirmed_balance": 0.0, "immature_balance": 0.0})),
            "backupwallet" => {
                // As a newer boost's copy_file would: the source's permissions come along.
                let dest = req["params"][0].as_str().unwrap_or("");
                match std::fs::copy(datadir.join("wallet.dat"), dest) {
                    Ok(_) => Ok(serde_json::Value::Null),
                    Err(_) => Err((-4, "Error: Wallet backup failed!")),
                }
            }
            "stop" => Ok(serde_json::json!("FreeBank server stopping")),
            _ => Err((-32601, "Method not found")),
        };
        let (status, reply) = match answer {
            Ok(v) => ("200 OK", serde_json::json!({"result": v, "error": null, "id": req["id"]})),
            Err((code, message)) => (
                "500 Internal Server Error",
                serde_json::json!({"result": null, "error": {"code": code, "message": message}, "id": req["id"]}),
            ),
        };
        let text = reply.to_string();
        let head = format!(
            "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            text.len()
        );
        let _ = conn.write_all(head.as_bytes()).and_then(|_| conn.write_all(text.as_bytes()));
        if method == "stop" {
            let _ = conn.flush();
            drop(conn);
            let _ = std::fs::remove_file(datadir.join(".cookie"));
            std::process::exit(0);
        }
    }
}

/// Take an fcntl write lock on `path` for the rest of this process, as boost's file_lock does, or
/// exit as freebankd does when another node has it.
fn lock_or_exit(path: &Path) {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &fl) } == -1 {
        eprintln!("Cannot obtain a lock on {}", path.display());
        std::process::exit(1);
    }
    std::mem::forget(f);
}
