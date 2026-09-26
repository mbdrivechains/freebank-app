//! First run and node management: find the eCash beta stack (node REST + enforcer), install the
//! C++ freebankd release into the app's own data folder, run it, and report its progress, peers
//! and height. This app never bids (no refreshbmm).

pub mod commands;
pub mod detect;
pub mod install;
pub mod process;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The eCash beta fork block. freebankd refuses to start on any other chain.
pub const PIN_HEIGHT: u64 = 967_680;
pub const PIN_HASH: &str = "00000000000000030101ba5cfea54b22becc79f95dc6040beb76e01dd9d04042";
pub const EXPLORER: &str = "https://explorer.ecxfreebank.com";
pub const RELEASES_URL: &str = "https://api.github.com/repos/mbdrivechains/freebank/releases?per_page=50";
pub const RELEASE_DOWNLOAD: &str = "https://github.com/mbdrivechains/freebank/releases/download";
pub const GRPCURL_VERSION: &str = "1.9.4";
/// The FreeBank seed's own coinbase tag; users pick their own.
pub const SEED_TAG: &str = "ecxfreebank.com";
pub const DEFAULT_RPC_PORT: u16 = 8454;
pub const DEFAULT_P2P_PORT: u16 = 8455;
/// Marks a datadir this app set up, so it is never moved aside.
pub const DATADIR_MARK: &str = ".freebank-node";
/// What "Delete chain data" removes from the datadir. Houses, bills and pools live under blocks/.
/// wallet.dat, freebank.conf and the eCash block-hash cache (mainblockhash.dat) stay.
pub const CHAIN_DATA: &[&str] = &["blocks", "chainstate", "indexes", "bmm.dat", "mempool.dat"];
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the release asset and grpcurl come from on this machine: (freebank triplet, grpcurl os_arch).
pub fn platform() -> Result<(&'static str, &'static str), String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("x86_64-linux-gnu", "linux_x86_64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(("arm64-apple-darwin", "osx_arm64"))
    } else {
        Err("FreeBank runs on Linux x86_64 and Apple Silicon Macs for now.".into())
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// freebankd's own default datadir (main network: no subfolder).
pub fn default_datadir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/FreeBank")
    } else {
        home().join(".freebank")
    }
}

/// Settings the user can change under "Advanced", plus what the app has installed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// eCash node REST, host:port
    pub rest: String,
    /// bip300301 enforcer gRPC, host:port
    pub enforcer: String,
    pub datadir: String,
    pub rpc_port: u16,
    pub p2p_port: u16,
    /// Release tag this app installed and runs, e.g. v0.2.15
    pub installed_tag: Option<String>,
    pub grpcurl: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            rest: "127.0.0.1:18302".into(),
            enforcer: "127.0.0.1:50051".into(),
            datadir: default_datadir().to_string_lossy().into_owned(),
            rpc_port: DEFAULT_RPC_PORT,
            p2p_port: DEFAULT_P2P_PORT,
            installed_tag: None,
            grpcurl: None,
        }
    }
}

/// Everything the first-run flow and the Node tab share. Lives for the whole app.
pub struct NodeManager {
    pub app_dir: PathBuf,
    /// For GitHub, the explorer and eCash REST (not for the node's RPC).
    pub http: reqwest::Client,
    pub settings: tokio::sync::Mutex<Settings>,
    /// The freebankd this app started, if any. A node someone else started is never stopped here.
    pub child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    pub last_exit: std::sync::Mutex<Option<String>>,
    pub install: Arc<std::sync::Mutex<install::InstallProgress>>,
    pub update: Arc<std::sync::Mutex<install::InstallProgress>>,
    /// Set while the node is being stopped, restarted or swapped, so the Node tab can say so
    /// instead of waiting on the node.
    pub activity: std::sync::Mutex<Option<String>>,
    explorer_tip: std::sync::Mutex<Option<(Instant, u64)>>,
    /// The newest C++ release on GitHub, cached so the Node tab doesn't ask on every poll.
    latest: std::sync::Mutex<Option<(Instant, Result<String, String>)>>,
    /// freebankd -version per release tag: ("v0.2.15", "843ccae").
    versions: std::sync::Mutex<HashMap<String, (String, Option<String>)>>,
}

/// Holds `NodeManager::activity` for one long operation; clears it when dropped.
pub struct Busy<'a>(&'a NodeManager);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        *self.0.activity.lock().unwrap() = None;
    }
}

impl NodeManager {
    pub fn new(app_dir: PathBuf) -> Self {
        let settings = std::fs::read(app_dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("FreeBank-app/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(8))
            .build()
            .expect("http client");
        Self {
            app_dir,
            http,
            settings: tokio::sync::Mutex::new(settings),
            child: tokio::sync::Mutex::new(None),
            last_exit: std::sync::Mutex::new(None),
            install: Arc::new(std::sync::Mutex::new(Default::default())),
            update: Arc::new(std::sync::Mutex::new(Default::default())),
            activity: std::sync::Mutex::new(None),
            explorer_tip: std::sync::Mutex::new(None),
            latest: std::sync::Mutex::new(None),
            versions: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Claim the node for one operation ("Stopping FreeBank…"). Fails if another is under way.
    pub fn busy(&self, what: &str) -> Result<Busy<'_>, String> {
        let mut a = self.activity.lock().unwrap();
        if let Some(other) = a.as_ref() {
            return Err(format!("Please wait: {}", other));
        }
        *a = Some(what.to_string());
        Ok(Busy(self))
    }

    /// The newest C++ release tag, from cache for 30 minutes unless `force`.
    pub async fn latest_release(&self, force: bool) -> Result<String, String> {
        if !force {
            if let Some((at, r)) = self.latest.lock().unwrap().as_ref() {
                if at.elapsed() < Duration::from_secs(30 * 60) {
                    return r.clone();
                }
            }
        }
        let r = install::find_release(&self.http).await;
        *self.latest.lock().unwrap() = Some((Instant::now(), r.clone()));
        r
    }

    /// The version and commit of an installed release, from `freebankd -version` (cached).
    pub fn release_version(&self, tag: &str) -> Option<(String, Option<String>)> {
        if let Some(v) = self.versions.lock().unwrap().get(tag) {
            return Some(v.clone());
        }
        let out = std::process::Command::new(self.freebankd(tag))
            .arg("-version")
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let v = text.lines().find_map(parse_version_line)?;
        self.versions.lock().unwrap().insert(tag.to_string(), v.clone());
        Some(v)
    }

    pub async fn save_settings(&self, s: Settings) -> Result<(), String> {
        std::fs::create_dir_all(&self.app_dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?;
        std::fs::write(self.app_dir.join("settings.json"), json).map_err(|e| e.to_string())?;
        *self.settings.lock().await = s;
        Ok(())
    }

    pub fn release_dir(&self, tag: &str) -> PathBuf {
        self.app_dir.join("releases").join(tag)
    }

    pub fn freebankd(&self, tag: &str) -> PathBuf {
        self.release_dir(tag).join("freebank/bin/freebankd")
    }

    /// The explorer's tip height, cached for 15 s so polling screens don't hammer it.
    pub async fn explorer_tip(&self) -> Option<u64> {
        if let Some((at, h)) = *self.explorer_tip.lock().unwrap() {
            if at.elapsed() < Duration::from_secs(15) {
                return Some(h);
            }
        }
        let text = self
            .http
            .get(format!("{}/api/blocks/tip/height", EXPLORER))
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        let h: u64 = text.trim().parse().ok()?;
        *self.explorer_tip.lock().unwrap() = Some((Instant::now(), h));
        Some(h)
    }
}

/// The `coinbasetag=` line of a freebank.conf, if any.
pub fn conf_tag(datadir: &Path) -> Option<String> {
    let conf = std::fs::read_to_string(datadir.join("freebank.conf")).ok()?;
    conf.lines()
        .find_map(|l| l.strip_prefix("coinbasetag="))
        .map(|t| t.to_string())
}

/// "FreeBank Daemon version v0.2.15.0-843ccae" -> ("v0.2.15", Some("843ccae")).
/// Also reads the "FreeBank version v0.2.15.0-843ccae (release build)" line in debug.log.
pub fn parse_version_line(line: &str) -> Option<(String, Option<String>)> {
    let word = line.split_whitespace().find(|w| {
        w.starts_with('v') && w[1..].starts_with(|c: char| c.is_ascii_digit())
    })?;
    let (ver, commit) = match word.split_once('-') {
        Some((v, c)) if !c.is_empty() => (v, Some(c.to_string())),
        _ => (word, None),
    };
    // v0.2.15.0 -> v0.2.15; v0.2.7.1 stays.
    let ver = if ver.matches('.').count() == 3 {
        ver.strip_suffix(".0").unwrap_or(ver)
    } else {
        ver
    };
    Some((ver.to_string(), commit))
}

/// The N of a C++ release tag v0.2.N.
pub fn patch(tag: &str) -> Option<u32> {
    tag.strip_prefix("v0.2.")?.parse().ok()
}

/// Remove `target` (a file, folder or link) only if it lies strictly inside `root`.
/// Returns false when there was nothing to remove.
pub fn remove_inside(root: &Path, target: &Path) -> Result<bool, String> {
    let meta = match std::fs::symlink_metadata(target) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    let root = root
        .canonicalize()
        .map_err(|e| format!("{}: {}", root.display(), e))?;
    let name = target.file_name().ok_or("Refusing to remove a path with no name.")?;
    let parent = target
        .parent()
        .ok_or("Refusing to remove a path with no parent.")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let full = parent.join(name);
    if !full.starts_with(&root) || full == root {
        return Err(format!(
            "Refusing to remove {}: it is outside {}.",
            full.display(),
            root.display()
        ));
    }
    // A link is removed as a link, never followed.
    if meta.is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(&full)
    } else {
        std::fs::remove_file(&full)
    }
    .map_err(|e| format!("Couldn't remove {}: {}", full.display(), e))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_lines() {
        assert_eq!(
            parse_version_line("FreeBank Daemon version v0.2.16.0-2afa30c"),
            Some(("v0.2.16".into(), Some("2afa30c".into())))
        );
        assert_eq!(
            parse_version_line("2026-09-26 04:00:26 FreeBank version v0.2.15.0-843ccae (release build)"),
            Some(("v0.2.15".into(), Some("843ccae".into())))
        );
        assert_eq!(parse_version_line("version v0.2.7.1"), Some(("v0.2.7.1".into(), None)));
        assert_eq!(parse_version_line("Copyright (C) 2009-2023"), None);
    }

    #[test]
    fn patches() {
        assert_eq!(patch("v0.2.15"), Some(15));
        assert_eq!(patch("v0.3.5"), None);
        assert!(patch("v0.2.16") > patch("v0.2.9"));
    }

    #[test]
    fn remove_guard() {
        let base = std::env::temp_dir().join(format!("fbrm-{}", std::process::id()));
        let root = base.join("app");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("releases/v1")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("tools")).unwrap();

        // Outside the root, the root itself, and ".." tricks are refused.
        assert!(remove_inside(&root, &outside.join("keep")).is_err());
        assert!(remove_inside(&root, &root).is_err());
        assert!(remove_inside(&root, &root.join("releases/../../outside/keep")).is_err());
        // A link inside the root goes, but what it points to stays.
        #[cfg(unix)]
        {
            assert!(remove_inside(&root, &root.join("tools")).unwrap());
            assert!(outside.join("keep").exists());
        }
        assert!(remove_inside(&root, &root.join("releases")).unwrap());
        assert!(!root.join("releases").exists());
        assert!(!remove_inside(&root, &root.join("releases")).unwrap());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
