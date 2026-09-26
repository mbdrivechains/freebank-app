//! Running freebankd as this app's child: start, stop (RPC `stop`, then SIGTERM), and the
//! progress/status reads the screens poll. A node the app didn't start is only read, never stopped.

use super::{conf_tag, detect, install, NodeManager, EXPLORER, PIN_HASH, PIN_HEIGHT};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Start freebankd with the settings on file. Errors are written for the screen.
pub async fn start(mgr: &NodeManager) -> Result<(), String> {
    mgr.still_here()?;
    let mut child = mgr.child.lock().await;
    if let Some(c) = child.as_mut() {
        if c.try_wait().ok().flatten().is_none() {
            return Ok(());
        }
    }
    // A new start: why an earlier node stopped no longer applies. Cleared before the checks below,
    // so a start that fails here isn't reported as that old exit. (Only `reap` sets it, under the
    // child lock held here.)
    *mgr.last_exit.lock().unwrap() = None;
    let s = mgr.settings.lock().await.clone();
    let tag = s.installed_tag.clone().ok_or("FreeBank isn't installed yet.")?;
    let bin = mgr.freebankd(&tag);
    if !bin.is_file() {
        return Err(format!("{} is missing; please install again.", bin.display()));
    }
    let grpcurl = install::find_grpcurl(&mgr.app_dir, s.grpcurl.as_deref())
        .map(|(p, _)| p)
        .ok_or("grpcurl is missing; please install again.")?;
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

    let mut cmd = tokio::process::Command::new(&bin);
    cmd.arg(format!("-datadir={}", s.datadir))
        .arg("-server=1")
        .arg("-mainchaintransport=enforcer")
        .arg(format!("-enforceraddr={}", s.enforcer))
        .arg(format!("-mainchainrest={}", s.rest))
        .arg("-mainchainchain=main")
        .arg(format!("-mainchainblockpin={}:{}", PIN_HEIGHT, PIN_HASH))
        .arg(format!("-grpcurlbin={}", grpcurl.display()))
        // Always explicit, so an rpcport= line in freebank.conf can't hide the node from us.
        .arg(format!("-rpcport={}", s.rpc_port))
        .arg(format!("-port={}", s.p2p_port))
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    // If the app dies without a clean exit, the kernel asks the node to shut down too.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let c = cmd
        .spawn()
        .map_err(|e| format!("Couldn't start {}: {}", bin.display(), e))?;
    *child = Some(c);
    Ok(())
}

/// Stop the node this app started: RPC `stop`, else SIGTERM (the RPC refuses during warm-up),
/// then wait for it to exit. Never SIGKILL: a hard kill risks the block index.
pub async fn stop(mgr: &NodeManager) -> Result<(), String> {
    let mut guard = mgr.child.lock().await;
    let Some(child) = guard.as_mut() else {
        return Ok(());
    };
    if child.try_wait().ok().flatten().is_some() {
        *guard = None;
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
            Ok(())
        }
        Err(_) => Err("FreeBank is still shutting down; give it a minute.".into()),
    }
}

/// The last line of debug.log, without its timestamp, for the "warming up" screen.
fn last_log_line(datadir: &Path) -> Option<String> {
    let mut f = std::fs::File::open(datadir.join("debug.log")).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    // "2026-09-26 10:00:00 init message: Loading…" -> "Loading…"
    let b = line.as_bytes();
    let stamped = b.len() > 20 && b[4] == b'-' && b[10] == b' ' && b[13] == b':' && b[19] == b' ';
    let msg = if stamped { &line[20..] } else { line };
    let msg = msg.strip_prefix("init message: ").unwrap_or(msg);
    Some(msg.chars().take(160).collect())
}

/// If our child has exited, forget it and say why (once; later polls read `last_exit`).
async fn reap(mgr: &NodeManager, datadir: &Path) -> (bool, Option<String>) {
    let mut guard = mgr.child.lock().await;
    let Some(child) = guard.as_mut() else {
        return (false, mgr.last_exit.lock().unwrap().clone());
    };
    match child.try_wait() {
        Ok(None) => (true, None),
        Ok(Some(status)) => {
            let why = last_log_line(datadir).unwrap_or_default();
            let msg = format!("FreeBank stopped ({}). {}", status, why).trim().to_string();
            *guard = None;
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
}

pub async fn progress(mgr: &NodeManager) -> NodeProgress {
    let s = mgr.settings.lock().await.clone();
    let datadir = PathBuf::from(&s.datadir);
    let (running, exited) = reap(mgr, &datadir).await;
    let rpc = detect::probe(&mgr.http, &s).await;
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
    /// The app started this node (and will stop it on quit).
    pub managed: bool,
    /// The app has a freebankd it can start.
    pub installed: bool,
    pub exited: Option<String>,
    pub log_line: Option<String>,
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
    pub versions: Versions,
}

/// "FreeBank app 0.1.0 · node v0.2.15 (843ccae)"
#[derive(Debug, Serialize, Clone, Default)]
pub struct Versions {
    pub app: String,
    pub node: Option<String>,
    pub commit: Option<String>,
}

/// Is the node this app started still running?
pub async fn child_alive(mgr: &NodeManager) -> bool {
    let mut guard = mgr.child.lock().await;
    guard
        .as_mut()
        .map(|c| c.try_wait().ok().flatten().is_none())
        .unwrap_or(false)
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
        exited: None,
        log_line: None,
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
    let (managed, exited) = reap(mgr, &datadir).await;
    st.managed = managed;
    st.exited = exited;
    let probe = detect::probe(&mgr.http, &s).await;
    // Our node is alive but hasn't opened its RPC port yet: it is starting, not stopped.
    st.state = match probe.state {
        detect::RpcState::Down if managed => detect::RpcState::Warming,
        other => other,
    };
    st.message = probe.message;
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

/// Is a node the app didn't start answering on our port?
pub(crate) async fn someone_elses_node(mgr: &NodeManager) -> bool {
    if child_alive(mgr).await {
        return false;
    }
    let s = mgr.settings.lock().await.clone();
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
}
