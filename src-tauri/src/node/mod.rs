//! First run and node management: find the eCash beta stack (node REST + enforcer), install the
//! C++ freebankd release into the app's own data folder, run it, and report its progress, peers
//! and height. This app never bids (no refreshbmm).

pub mod background;
pub mod commands;
pub mod detect;
pub mod install;
pub mod lock;
pub mod obliterate;
pub mod process;
pub mod release_key;
#[cfg(all(test, unix))]
pub mod testnode;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The eCash beta fork block. freebankd refuses to start on any other chain.
pub const PIN_HEIGHT: u64 = 967_680;
pub const PIN_HASH: &str = "00000000000000030101ba5cfea54b22becc79f95dc6040beb76e01dd9d04042";
pub const EXPLORER: &str = "https://explorer.ecxfreebank.com";
pub const RELEASES_URL: &str = "https://api.github.com/repos/mbdrivechains/freebank/releases?per_page=50";
pub const RELEASE_DOWNLOAD: &str = "https://github.com/mbdrivechains/freebank/releases/download";
/// The FreeBank seed's own coinbase tag; users pick their own.
pub const SEED_TAG: &str = "ecxfreebank.com";
pub const DEFAULT_RPC_PORT: u16 = 8454;
pub const DEFAULT_P2P_PORT: u16 = 8455;
/// Marks a datadir this app set up, so it is never moved aside.
pub const DATADIR_MARK: &str = ".freebank-node";

/// The end of a moved-aside folder's name after "<name>.old-": a unix time, with "-2", "-3"… when
/// two moves fell in one second (detect.rs `away_path`, recovery `keep_aside`).
pub fn aside_stamp(rest: &str) -> bool {
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    match rest.split_once('-') {
        Some((t, n)) => digits(t) && digits(n),
        None => digits(rest),
    }
}
/// What "Delete chain data" removes from the datadir. Houses, bills and pools live under blocks/.
/// The wallet (wallet.dat or wallets/), freebank.conf and the eCash block-hash cache stay.
pub const CHAIN_DATA: &[&str] = &["blocks", "chainstate", "indexes", "bmm.dat", "mempool.dat"];
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the release asset and grpcurl come from on this machine: (freebank triplet, grpcurl os_arch).
pub fn platform() -> Result<(&'static str, &'static str), String> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(("x86_64-linux-gnu", "linux_x86_64"))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(("arm64-apple-darwin", "osx_arm64"))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        // Intel Macs: freebankd v0.2.16 has no build for them yet; the app is ready for the release that adds
        // one, under this name (Bitcoin Core's depends triplet).
        Ok(("x86_64-apple-darwin", "osx_x86_64"))
    } else {
        Err("FreeBank runs on Linux x86_64 and Macs for now.".into())
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
    /// The data folder the app created when it last installed (it was missing, empty or only
    /// leftovers). Kept as v0.1.1 wrote it; `datadirs_created` has every one.
    pub datadir_created: Option<String>,
    /// Every data folder the app created. "Obliterate" ticks a node folder for you only when it is
    /// one of these (and still carries the mark).
    pub datadirs_created: Vec<String>,
    /// The folders FreeBank moved aside (<folder>.old-<time>): setup's older data folders, and the
    /// folders a restore put the replaced wallet in (recovery/job.rs). Only these are called that.
    pub moved_aside: Vec<String>,
    /// "Keep FreeBank's node running after I close the app" (Settings). Off: the app stops its node
    /// when it closes, as before.
    pub keep_running: bool,
    /// "Keep your phone connected when FreeBank is closed" (Settings > Phone; phone/background.rs).
    /// On means `keep_running` too.
    pub keep_phone: bool,
    /// The question was asked (once, after the first phone pairs) or the switch was used.
    pub keep_phone_asked: bool,
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
            datadir_created: None,
            datadirs_created: Vec::new(),
            moved_aside: Vec::new(),
            keep_running: false,
            keep_phone: false,
            keep_phone_asked: false,
        }
    }
}

impl Settings {
    /// Every data folder the app created, the one v0.1.1 recorded included, each once.
    pub fn created(&self) -> Vec<String> {
        let mut all = self.datadirs_created.clone();
        if let Some(d) = &self.datadir_created {
            if !all.contains(d) {
                all.insert(0, d.clone());
            }
        }
        all
    }

    /// Record a data folder this app created (after the one v0.1.1 recorded, which stays).
    pub fn add_created(&mut self, datadir: &str) {
        self.datadirs_created = self.created();
        if !self.datadirs_created.iter().any(|d| d == datadir) {
            self.datadirs_created.push(datadir.to_string());
        }
        self.datadir_created = Some(datadir.to_string());
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
    /// The install task, so "Cancel" can stop a download without waiting for it.
    pub install_task: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    pub update: Arc<std::sync::Mutex<install::InstallProgress>>,
    /// Set while the node is being stopped, restarted or swapped, so the Node tab can say so
    /// instead of waiting on the node.
    pub activity: std::sync::Mutex<Option<String>>,
    explorer_tip: std::sync::Mutex<Option<(Instant, u64)>>,
    /// The newest C++ release on GitHub, cached so the Node tab doesn't ask on every poll.
    latest: std::sync::Mutex<Option<(Instant, Result<String, String>)>>,
    /// freebankd -version per release tag: ("v0.2.15", "843ccae").
    versions: std::sync::Mutex<HashMap<String, (String, Option<String>)>>,
    /// Set once "Obliterate" has removed the app's own folder: nothing is written to disk after.
    pub obliterated: AtomicBool,
    /// What "Obliterate" leaves for the app's exit, because the screen still uses it.
    pub at_exit: std::sync::Mutex<obliterate::AtExit>,
    /// Wallet backups made since the app started (Obliterate's "Back up wallet"), each with the
    /// wallet file it copies: a backup covers only its own wallet.
    pub backups: std::sync::Mutex<Vec<obliterate::Backup>>,
    /// The node this app started before it last closed (or crashed), recognised again at this launch
    /// and managed like its own (background.rs). A child of this run lives in `child` instead.
    pub adopted: std::sync::Mutex<Option<background::Adopted>>,
    /// The node in `child` was started in its own session, so it can outlive the app.
    pub detached: AtomicBool,
    /// The one-time `-reindex` start for a data folder an older release wrote (process::reap_or_reindex):
    /// REINDEX_IDLE, REINDEX_RUNNING or REINDEX_GAVE_UP.
    pub reindex: AtomicU8,
    /// Where debug.log and logs/freebankd.out ended when this app last started its node, so the reason it stopped
    /// is read only from what that run wrote (process::LogMark).
    pub log_mark: std::sync::Mutex<Option<process::LogMark>>,
    /// When the window's close was last held to say the node keeps running.
    pub close_asked: std::sync::Mutex<Option<Instant>>,
    /// The app is updating itself (app_update.rs): Obliterate waits for it, so an update never restarts a removed app.
    pub app_updating: AtomicBool,
    /// When the app last looked for its node from an earlier launch.
    pub adopt_tried: std::sync::Mutex<Option<Instant>>,
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
            install_task: std::sync::Mutex::new(None),
            update: Arc::new(std::sync::Mutex::new(Default::default())),
            activity: std::sync::Mutex::new(None),
            explorer_tip: std::sync::Mutex::new(None),
            latest: std::sync::Mutex::new(None),
            versions: std::sync::Mutex::new(HashMap::new()),
            obliterated: AtomicBool::new(false),
            at_exit: std::sync::Mutex::new(Default::default()),
            backups: std::sync::Mutex::new(Vec::new()),
            adopted: std::sync::Mutex::new(None),
            detached: AtomicBool::new(false),
            reindex: AtomicU8::new(process::REINDEX_IDLE),
            log_mark: std::sync::Mutex::new(None),
            close_asked: std::sync::Mutex::new(None),
            adopt_tried: std::sync::Mutex::new(None),
            app_updating: AtomicBool::new(false),
        }
    }

    /// Refuse anything that would write to disk once "Obliterate" has run.
    pub fn still_here(&self) -> Result<(), String> {
        if self.obliterated.load(Ordering::SeqCst) {
            return Err("FreeBank has been removed from this computer. Close the app to finish.".into());
        }
        Ok(())
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

    /// The version and commit of an installed release, from `freebankd -version` (cached). A release
    /// this code didn't check is never run, not even for this.
    pub fn release_version(&self, tag: &str) -> Option<(String, Option<String>)> {
        if let Some(v) = self.versions.lock().unwrap().get(tag) {
            return Some(v.clone());
        }
        if !self.verified(tag) {
            return None;
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

    /// Write settings.json. Once "Obliterate" has run this does nothing, so the file stays gone.
    pub async fn save_settings(&self, s: Settings) -> Result<(), String> {
        if self.obliterated.load(Ordering::SeqCst) {
            return Ok(());
        }
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

    /// Did this code put releases/<tag> there, after the signature and hash checks (`.verified`)?
    /// Only such a release is run. One an earlier build of the app installed without the checks is
    /// downloaded and checked again first (Node tab, or install again).
    pub fn verified(&self, tag: &str) -> bool {
        self.release_dir(tag).join(install::VERIFIED).is_file()
    }

    /// The installed release is on disk but wasn't checked by this code, so it won't be started.
    pub fn unverified(&self, s: &Settings) -> Option<String> {
        s.installed_tag
            .as_deref()
            .filter(|t| self.freebankd(t).is_file() && !self.verified(t))
            .map(String::from)
    }

    /// Can the app start its node? It needs the freebankd it installed and checked, and the node's
    /// data folder: freebankd won't create that folder (setup does), so without it first run sets
    /// FreeBank up again.
    pub fn can_start(&self, s: &Settings) -> bool {
        s.installed_tag
            .as_deref()
            .is_some_and(|t| self.freebankd(t).is_file() && self.verified(t))
            && Path::new(&s.datadir).is_dir()
    }

    /// The explorer's tip as the app last saw it (the Node tab asks every few seconds), if that was
    /// in the last 10 minutes. Never asks the explorer.
    pub fn explorer_tip_seen(&self) -> Option<u64> {
        let (at, h) = (*self.explorer_tip.lock().unwrap())?;
        (at.elapsed() < Duration::from_secs(600)).then_some(h)
    }

    /// The explorer's tip height, cached for 15 s so polling screens don't hammer it.
    pub async fn explorer_tip(&self) -> Option<u64> {
        // Unit tests never ask the real explorer.
        if cfg!(test) {
            return self.explorer_tip_seen();
        }
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

/// The first `walletdir=` in the data folder's freebank.conf, read as freebankd reads it: the first
/// line wins, `#` starts a comment, and keys after a `[section]` line belong to that section. Only a
/// full path to a folder counts; freebankd refuses to start with anything else.
pub fn conf_walletdir(datadir: &Path) -> Option<PathBuf> {
    let conf = std::fs::read_to_string(datadir.join("freebank.conf")).ok()?;
    for line in conf.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            return None;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "walletdir" {
            let dir = PathBuf::from(value.trim());
            return (dir.is_absolute() && dir.is_dir()).then_some(dir);
        }
    }
    None
}

/// The folders freebankd (0.16) may keep wallets in, and lock with `.walletlock`: walletdir= from
/// freebank.conf, wallets/, and the data folder itself. Those that exist, each once.
pub fn wallet_dirs(datadir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in [conf_walletdir(datadir), Some(datadir.join("wallets")), Some(datadir.to_path_buf())]
        .into_iter()
        .flatten()
    {
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The wallets of the node whose data folder is `datadir`, wherever freebankd (Core 0.16) may have
/// put them: wallet.dat and named wallets (-wallet=<name>, a file each) go in the folder walletdir=
/// in freebank.conf names, else in wallets/ when that folder exists, else in the datadir. freebankd
/// makes wallets/ only when it creates the datadir itself; this app makes the folder first, so its
/// nodes keep wallet.dat at the top. wallets/<name>/wallet.dat, as later Core versions lay it out,
/// counts too. Every place is looked at, default wallets first. A walletdir may lie outside the
/// data folder.
pub fn wallet_files(datadir: &Path) -> Vec<PathBuf> {
    // Each place, and whether its subfolders can hold wallets (not the data folder's: blocks/ and
    // the like hold none).
    let mut places: Vec<(PathBuf, bool)> = Vec::new();
    for dir in [conf_walletdir(datadir), Some(datadir.join("wallets")), Some(datadir.to_path_buf())]
        .into_iter()
        .flatten()
    {
        if !places.iter().any(|(d, _)| *d == dir) {
            let subfolders = dir != datadir;
            places.push((dir, subfolders));
        }
    }
    let mut found: Vec<PathBuf> = places
        .iter()
        .map(|(d, _)| d.join("wallet.dat"))
        .filter(|p| p.is_file())
        .collect();
    for (dir, subfolders) in &places {
        let (dir, subfolders) = (dir.as_path(), *subfolders);
        let mut named: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name() != "wallet.dat")
            .filter_map(|e| {
                let p = e.path();
                if p.is_dir() {
                    // blocks/, chainstate/ and the like hold no wallets; only wallets/<name>/ can.
                    let w = p.join("wallet.dat");
                    (subfolders && w.is_file()).then_some(w)
                } else {
                    is_bdb_file(&p).then_some(p)
                }
            })
            .collect();
        named.sort();
        found.extend(named);
    }
    // A walletdir inside wallets/ is seen from both.
    let mut seen = std::collections::HashSet::new();
    found.retain(|p| seen.insert(p.clone()));
    found
}

/// The wallets that lie inside `folder` (deleting the folder deletes them). A link to a folder
/// holds none: only the link would go.
pub fn wallets_inside(folder: &Path, wallets: &[PathBuf]) -> Vec<PathBuf> {
    let is_link = std::fs::symlink_metadata(folder)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if is_link {
        return Vec::new();
    }
    wallets.iter().filter(|w| w.starts_with(folder)).cloned().collect()
}

/// Is this a Berkeley DB btree file (what a wallet is)? The same test as Core's IsBerkeleyBtree:
/// at least one 4 KiB page, with the btree magic 0x00053162 at byte 12.
fn is_bdb_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    if !f.metadata().map(|m| m.is_file() && m.len() >= 4096).unwrap_or(false) {
        return false;
    }
    let mut head = [0u8; 16];
    if f.read_exact(&mut head).is_err() {
        return false;
    }
    let magic = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    magic == 0x0005_3162 || magic == 0x6231_0500
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
    fn aside_stamps() {
        for ok in ["1727000001", "1727000001-2", "1727000001-10"] {
            assert!(aside_stamp(ok), "{}", ok);
        }
        for bad in ["", "-2", "1727000001-", "1727000001-2-3", "17270x", "1727000001-b"] {
            assert!(!aside_stamp(bad), "{}", bad);
        }
    }

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
    fn wallets_found() {
        let d = std::env::temp_dir().join(format!("fbwallet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // A Berkeley DB page with the btree magic at byte 12, as a wallet starts.
        let mut bdb = vec![0u8; 4096];
        bdb[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());

        assert!(wallet_files(&d).is_empty());
        assert!(wallet_files(&d.join("missing")).is_empty());

        // A folder this app set up: wallet.dat at the top, named wallets beside it.
        std::fs::write(d.join("wallet.dat"), b"w").unwrap();
        std::fs::write(d.join("savings"), &bdb).unwrap();
        std::fs::write(d.join("peers.dat"), vec![0u8; 5000]).unwrap();
        std::fs::create_dir_all(d.join("blocks")).unwrap();
        std::fs::write(d.join("blocks/wallet.dat"), b"w").unwrap();
        assert_eq!(wallet_files(&d), vec![d.join("wallet.dat"), d.join("savings")]);

        // A folder freebankd made itself has wallets/, and its wallets are there.
        let w = d.join("wallets");
        std::fs::create_dir_all(w.join("database")).unwrap();
        std::fs::write(w.join("db.log"), b"").unwrap();
        std::fs::write(w.join(".walletlock"), b"").unwrap();
        std::fs::write(w.join("database/log.0000000001"), &bdb[..100]).unwrap();
        assert_eq!(wallet_files(&d), vec![d.join("wallet.dat"), d.join("savings")]);
        std::fs::write(w.join("wallet.dat"), b"w").unwrap();
        std::fs::write(w.join("spending"), &bdb).unwrap();
        std::fs::write(w.join("short"), &bdb[..100]).unwrap();
        std::fs::create_dir_all(w.join("house")).unwrap();
        std::fs::write(w.join("house/wallet.dat"), b"w").unwrap();
        std::fs::create_dir_all(w.join("empty")).unwrap();
        assert_eq!(
            wallet_files(&d),
            vec![
                w.join("wallet.dat"),
                d.join("wallet.dat"),
                w.join("house/wallet.dat"),
                w.join("spending"),
                d.join("savings"),
            ]
        );
        // Only wallets/wallet.dat: the usual fresh folder.
        std::fs::remove_file(d.join("wallet.dat")).unwrap();
        std::fs::remove_file(d.join("savings")).unwrap();
        std::fs::remove_file(w.join("spending")).unwrap();
        std::fs::remove_dir_all(w.join("house")).unwrap();
        assert_eq!(wallet_files(&d), vec![w.join("wallet.dat")]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Finding 11: wallets in a walletdir= folder count, wherever it is.
    #[test]
    fn walletdir_is_read_as_freebankd_reads_it() {
        let d = std::env::temp_dir().join(format!("fbwalletdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let datadir = d.join("node");
        let mine = datadir.join("mywallets");
        let outside = d.join("elsewhere");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let conf = |text: &str| std::fs::write(datadir.join("freebank.conf"), text).unwrap();
        let mut bdb = vec![0u8; 4096];
        bdb[12..16].copy_from_slice(&0x0005_3162u32.to_le_bytes());

        assert_eq!(conf_walletdir(&datadir), None);
        conf(&format!("coinbasetag=x\n walletdir = {} # mine\nwalletdir={}\n", mine.display(), outside.display()));
        assert_eq!(conf_walletdir(&datadir), Some(mine.clone()), "the first line wins");
        conf(&format!("# walletdir={}\n[test]\nwalletdir={}\n", mine.display(), outside.display()));
        assert_eq!(conf_walletdir(&datadir), None, "comments and other sections don't count");
        conf("walletdir=relative/path\n");
        assert_eq!(conf_walletdir(&datadir), None, "freebankd refuses a relative walletdir");
        conf(&format!("walletdir={}\n", d.join("missing").display()));
        assert_eq!(conf_walletdir(&datadir), None);

        // Inside the data folder under another name: v0.1.1 missed these, so Obliterate deleted them
        // with no warning or backup.
        conf(&format!("walletdir={}\n", mine.display()));
        std::fs::write(mine.join("wallet.dat"), b"w").unwrap();
        std::fs::write(mine.join("savings"), &bdb).unwrap();
        std::fs::write(datadir.join("wallet.dat"), b"old").unwrap();
        assert_eq!(wallet_files(&datadir), vec![mine.join("wallet.dat"), datadir.join("wallet.dat"), mine.join("savings")]);
        assert_eq!(wallet_dirs(&datadir), vec![mine.clone(), datadir.clone()]);
        assert_eq!(wallets_inside(&datadir, &wallet_files(&datadir)).len(), 3);

        // Outside it: still the node's wallets (backups copy them), but deleting the data folder
        // doesn't take them.
        conf(&format!("walletdir={}\n", outside.display()));
        std::fs::write(outside.join("wallet.dat"), b"w").unwrap();
        let all = wallet_files(&datadir);
        assert_eq!(all, vec![outside.join("wallet.dat"), datadir.join("wallet.dat")]);
        assert_eq!(wallets_inside(&datadir, &all), vec![datadir.join("wallet.dat")]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn only_checked_releases_can_start() {
        let d = std::env::temp_dir().join(format!("fbverify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("node")).unwrap();
        let mgr = NodeManager::new(d.join("app"));
        let s = Settings {
            datadir: d.join("node").to_string_lossy().into_owned(),
            installed_tag: Some("v0.2.16".into()),
            ..Default::default()
        };
        assert!(!mgr.can_start(&s) && mgr.unverified(&s).is_none());
        std::fs::create_dir_all(mgr.freebankd("v0.2.16").parent().unwrap()).unwrap();
        std::fs::write(mgr.freebankd("v0.2.16"), b"#!/bin/sh\n").unwrap();
        assert!(!mgr.can_start(&s));
        assert_eq!(mgr.unverified(&s).as_deref(), Some("v0.2.16"));
        std::fs::write(mgr.release_dir("v0.2.16").join(install::VERIFIED), b"x\n").unwrap();
        assert!(mgr.can_start(&s) && mgr.verified("v0.2.16") && mgr.unverified(&s).is_none());
        std::fs::remove_dir_all(&d).unwrap();
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
