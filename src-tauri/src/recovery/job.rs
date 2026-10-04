//! The long wallet jobs. They run in the background while the screen polls their progress
//! (`wallet_setup_progress`), and each may restart the node:
//! - **Setup:** the passphrase first (`encryptwallet`; freebankd stops, and the app starts it again),
//!   then the HD seed from new or typed words, the encrypted copy of the words, and for typed words a
//!   scan of the chain for their coins. `fresh` first moves the current wallet aside, so the words go
//!   into a new wallet.
//! - **Restore a backup file:** the file in the wallet's place (the current wallet moved aside, or put
//!   back if the node can't use the file), then a scan.

use super::*;
use crate::node::detect;
use crate::seed::{Chain, Entropy};
use crate::wallet::{RelockGuard, RelockState};
use serde_json::json;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use zeroize::Zeroizing;

pub(crate) const WRONG_PASSPHRASE: &str = "That isn't the wallet's passphrase.";
const ALREADY_HAD_THESE_WORDS: &str = "This wallet had these recovery words' seed once before, and a wallet can't go back to an \
     earlier seed. Restore them into a new wallet instead: Settings, Wallet, Restore from recovery words.";

/// What the screen shows while a job runs.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub running: bool,
    pub done: bool,
    /// "new", "restore-words" or "restore-file".
    pub kind: String,
    /// This job's steps in order ("aside", "start", "encrypt", "restart", "seed", "save", "scan"), and
    /// the one under way.
    pub stages: Vec<String>,
    pub stage: String,
    pub note: Option<String>,
    pub error: Option<String>,
    /// Another program runs the node, which stopped to finish encrypting: it must be started there.
    pub waiting_for_node: bool,
    /// Where the wallet that was in place before is kept now.
    pub moved_aside: Option<String>,
    /// New words are ready: the screen takes them once with `wallet_setup_words`.
    pub words_ready: bool,
    /// The scan for coins: the block reached, and the last block.
    pub scan_at: Option<u64>,
    pub scan_to: Option<u64>,
}

/// A job's progress, shared with the screen.
#[derive(Clone, Default)]
pub struct Report(Arc<Mutex<Progress>>);

impl Report {
    pub fn get(&self) -> Progress {
        self.0.lock().unwrap().clone()
    }
    fn with(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.0.lock().unwrap());
    }
    fn stages(&self, s: &[&str]) {
        self.with(|p| p.stages = s.iter().map(|x| x.to_string()).collect());
    }
    fn stage(&self, s: &str) {
        self.with(|p| {
            p.stage = s.into();
            p.note = None;
        });
    }
    fn note(&self, n: impl Into<String>) {
        let n = n.into();
        self.with(|p| p.note = Some(n));
    }
    fn waiting(&self, w: bool) {
        self.with(|p| p.waiting_for_node = w);
    }
    fn aside(&self, path: Option<&Path>) {
        self.with(|p| p.moved_aside = path.map(|a| a.to_string_lossy().into_owned()));
    }
    fn scan(&self, at: u64, to: u64) {
        self.with(|p| {
            p.scan_at = Some(at);
            p.scan_to = Some(to);
        });
    }
}

static JOB: LazyLock<Report> = LazyLock::new(Report::default);
/// A new wallet's words, until the screen takes them (once), or for ten minutes.
static NEW_WORDS: LazyLock<Mutex<Option<(Instant, Zeroizing<Vec<String>>)>>> = LazyLock::new(Default::default);
const WORDS_KEPT: Duration = Duration::from_secs(600);

pub fn progress() -> Progress {
    JOB.get()
}

/// The new words, once. Afterwards they are only shown again from Settings, behind the passphrase.
pub fn take_words() -> Option<Zeroizing<Vec<String>>> {
    let (at, words) = NEW_WORDS.lock().unwrap().take()?;
    JOB.with(|p| p.words_ready = false);
    (at.elapsed() < WORDS_KEPT).then_some(words)
}

pub enum Job {
    Setup { passphrase: Zeroizing<String>, restore: Option<Entropy>, fresh: bool },
    RestoreFile { upload: PathBuf },
}

/// Start a job in the background. One at a time. `guard` is the app's one RelockGuard.
pub fn start(mgr: Arc<NodeManager>, guard: RelockState, job: Job) -> Result<(), String> {
    let kind = match &job {
        Job::Setup { restore: None, .. } => "new",
        Job::Setup { .. } => "restore-words",
        Job::RestoreFile { .. } => "restore-file",
    };
    {
        let mut p = JOB.0.lock().unwrap();
        if p.running {
            return Err("Please wait: FreeBank is still working on your wallet.".into());
        }
        *p = Progress { running: true, kind: kind.into(), stage: "check".into(), ..Default::default() };
    }
    *NEW_WORDS.lock().unwrap() = None;
    let report = JOB.clone();
    tauri::async_runtime::spawn(async move {
        let r = run(&mgr, &guard, job, &report).await;
        finish(&report, r);
    });
    Ok(())
}

fn finish(report: &Report, r: Result<Option<Zeroizing<Vec<String>>>, String>) {
    let words_ready = matches!(r, Ok(Some(_)));
    if let Ok(Some(words)) = &r {
        *NEW_WORDS.lock().unwrap() = Some((Instant::now(), words.clone()));
    }
    report.with(|p| {
        p.running = false;
        p.waiting_for_node = false;
        p.words_ready = words_ready;
        match r {
            Ok(_) => {
                p.done = true;
                p.stage = "done".into();
                p.note = None;
            }
            Err(e) => p.error = Some(e),
        }
    });
}

/// Run a job to its end. New words come back for the screen to show.
pub(crate) async fn run(
    mgr: &NodeManager,
    guard: &RelockGuard,
    job: Job,
    report: &Report,
) -> Result<Option<Zeroizing<Vec<String>>>, String> {
    mgr.still_here()?;
    match job {
        Job::Setup { passphrase, restore, fresh } => setup(mgr, guard, &passphrase, restore, fresh, report).await,
        Job::RestoreFile { upload } => {
            let r = restore_file(mgr, &upload, report).await;
            // The screen's copy of the backup; the user still has the file they chose.
            let _ = std::fs::remove_file(&upload);
            r.map(|_| None)
        }
    }
}

// ---- Setup ----

async fn setup(
    mgr: &NodeManager,
    guard: &RelockGuard,
    pass: &str,
    restore: Option<Entropy>,
    fresh: bool,
    report: &Report,
) -> Result<Option<Zeroizing<Vec<String>>>, String> {
    let _busy = mgr.busy(if restore.is_some() {
        "Restoring your wallet from its recovery words…"
    } else {
        "Setting up your wallet…"
    })?;
    let s = mgr.settings.lock().await.clone();
    if fresh {
        report.stages(&["aside", "start", "encrypt", "restart", "seed", "save", "scan"]);
        report.stage("aside");
        let moved = replace_wallet(mgr, &s, None).await?;
        report.aside(moved.as_deref());
        report.stage("start");
        start_and_wait(mgr, &s, report).await?;
    }
    let mut c = local_client(mgr, &s, LONG)?;
    let info = c.call_ui("getwalletinfo", vec![]).await?;
    let chain = Chain::from_name(c.call_ui("getblockchaininfo", vec![]).await?["chain"].as_str().unwrap_or(""))?;
    let encrypted = info.get("unlocked_until").is_some();
    let mut stages: Vec<&str> = if fresh { vec!["aside", "start"] } else { vec![] };
    if !encrypted {
        stages.extend(["encrypt", "restart"]);
    }
    stages.extend(["seed", "save"]);
    if restore.is_some() {
        stages.push("scan");
    }
    report.stages(&stages);

    if !encrypted {
        report.stage("encrypt");
        let ours = process::child_alive(mgr).await;
        encrypt(&mut c, pass).await?;
        report.stage("restart");
        restart_after_encrypt(mgr, &s, ours, report).await?;
        c = local_client(mgr, &s, LONG)?;
        if c.call_ui("getwalletinfo", vec![]).await?.get("unlocked_until").is_none() {
            return Err("Your node started again, but its wallet has no passphrase. Please try again.".into());
        }
    }
    report.stage("seed");
    unlock(&mut c, guard, pass, UNLOCK_SECS).await?;
    let result = seed_and_scan(mgr, guard, &mut c, pass, restore, chain, report).await;
    // Locked again whatever happened (a wallet without a passphrase answers -15, which is fine).
    let _ = crate::wallet::lock(&mut c).await;
    result
}

async fn seed_and_scan(
    mgr: &NodeManager,
    guard: &RelockGuard,
    c: &mut FreeBankClient,
    pass: &str,
    restore: Option<Entropy>,
    chain: Chain,
    report: &Report,
) -> Result<Option<Zeroizing<Vec<String>>>, String> {
    let restoring = restore.is_some();
    let entropy = restore.unwrap_or_else(seed::new_entropy);
    let id = set_seed(c, &entropy, chain).await?;
    report.stage("save");
    mgr.still_here()?;
    save_words(&mgr.app_dir, &entropy, &id, pass, restoring).await?;
    if restoring {
        report.stage("scan");
        rescan(c, Some((pass, guard)), report).await?;
    }
    Ok((!restoring).then(|| seed::words(&entropy)))
}

/// encryptwallet. freebankd stops by itself afterwards.
pub(crate) async fn encrypt(c: &mut FreeBankClient, pass: &str) -> Result<(), String> {
    if pass.is_empty() {
        return Err("Please choose a passphrase.".into());
    }
    match c.call_fresh_typed("encryptwallet", vec![json!(pass)]).await {
        Ok(_) => Ok(()),
        // The node stops right after encrypting. If it went before its answer arrived, the check
        // after the restart shows whether the wallet got its passphrase.
        Err(RpcError::Refused(_)) | Err(RpcError::Unreachable(_)) | Err(RpcError::Busy) => Ok(()),
        Err(e) => Err(e.for_ui()),
    }
}

/// How long the flows unlock for: the most the app asks for. The scan unlocks again before it runs out.
pub(crate) const UNLOCK_SECS: u64 = crate::wallet::MAX_UNLOCK_SECONDS as u64;

/// Every walletpassphrase in the wallet flows goes through here, and so through the app's one
/// RelockGuard (crate::wallet::unlock, which clamps to 1..=300 s): freebankd's Core 0.16 wallet
/// deadlocks if an unlock lands on an earlier unlock's relock. Callers ask for 5 s or more. A wrong
/// passphrase says so in plain words.
pub(crate) async fn unlock(c: &mut FreeBankClient, guard: &RelockGuard, pass: &str, seconds: u64) -> Result<(), String> {
    match crate::wallet::unlock(c, guard, pass.to_string(), seconds as i64).await {
        Ok(_) => Ok(()),
        Err(e) if e.starts_with(&format!("RPC error {}:", RPC_WALLET_PASSPHRASE_INCORRECT)) => Err(WRONG_PASSPHRASE.into()),
        Err(e) => Err(e),
    }
}

/// Give the (unlocked) wallet the HD seed that `entropy`'s words make, unless it has it already, and
/// check the node derives the address the words predict. Returns the HD seed's key id.
pub(crate) async fn set_seed(c: &mut FreeBankClient, entropy: &[u8; 32], chain: Chain) -> Result<[u8; 20], String> {
    let hd = seed::freebank_hd_seed(entropy)?;
    let id = seed::key_id(&hd)?;
    let id_hex = seed::key_id_hex(&id);
    let current = c.call_ui("getwalletinfo", vec![]).await?["hdmasterkeyid"].as_str().map(String::from);
    if current.as_deref() != Some(id_hex.as_str()) {
        let wif = seed::wif(&hd, chain);
        match c.call_fresh_typed("sethdseed", vec![json!(true), json!(wif.as_str())]).await {
            Ok(_) => {}
            Err(e) if is_code(&e, RPC_INVALID_ADDRESS_OR_KEY) && e.to_string().contains("Already have this key") => {
                return Err(ALREADY_HAD_THESE_WORDS.into())
            }
            Err(e) => return Err(e.for_ui()),
        }
    }
    let now_id = c.call_ui("getwalletinfo", vec![]).await?["hdmasterkeyid"].as_str().map(String::from);
    if now_id.as_deref() != Some(id_hex.as_str()) {
        return Err("Your node didn't take the new seed. Nothing has changed; please try again.".into());
    }
    let first = seed::address(&hd, false, 0)?;
    let a = c.call_ui("getaddressinfo", vec![json!(first)]).await?;
    if a["ismine"] != json!(true) || a["hdkeypath"] != json!("m/0'/0'/0'") || a["hdmasterkeyid"] != json!(id_hex) {
        return Err("Your node makes different addresses from these words than FreeBank expects. Please don't \
                    use this wallet yet, and report this."
            .into());
    }
    Ok(id)
}

/// Save the words' entropy, sealed with the passphrase, as the app's seed file, and record the new
/// seed. A seed file for other words is kept beside it (seed.enc.old-<time>), never overwritten.
pub(crate) async fn save_words(app_dir: &Path, entropy: &Entropy, id: &[u8; 20], pass: &str, confirmed: bool) -> Result<(), String> {
    let (e, i, p) = (entropy.clone(), *id, Zeroizing::new(pass.to_string()));
    let sealed = tokio::task::spawn_blocking(move || seed::seal(&e, &i, &p, seed::KDF))
        .await
        .map_err(|e| e.to_string())??;
    let path = seed::seed_path(app_dir);
    if let Some(old) = seed::read_file(&path)? {
        if seed::file_key_id(&old).ok() != Some(*id) {
            keep_aside(&path)?;
        }
    }
    seed::write_private(&path, &sealed)?;
    let mut r = Record::load(app_dir);
    r.seed_id = Some(seed::key_id_hex(id));
    r.seed_set_at = Some(now());
    r.words_confirmed_at = confirmed.then(now);
    r.save(app_dir)
}

/// Rename `path` to `<path>.old-<time>` beside it.
pub(crate) fn keep_aside(path: &Path) -> Result<PathBuf, String> {
    let base = format!("{}.old-{}", path.to_string_lossy(), stamp());
    let mut dest = PathBuf::from(&base);
    let mut n = 2;
    while dest.exists() {
        dest = PathBuf::from(format!("{}-{}", base, n));
        n += 1;
    }
    std::fs::rename(path, &dest).map_err(|e| format!("Couldn't move {} aside: {}", path.display(), e))?;
    Ok(dest)
}

// ---- The node: stop, start, wait ----

/// The node's last debug.log line worth showing (`process::last_log_line`).
fn log_tail(datadir: &str) -> Option<String> {
    crate::node::process::last_log_line(Path::new(datadir))
}

/// Wait until the node answers with its wallet. `ours`: the app started it, so if it exits, say why.
async fn wait_up(mgr: &NodeManager, s: &Settings, ours: bool, limit: Duration, report: &Report) -> Result<(), String> {
    let until = Instant::now() + limit;
    loop {
        if ours && !process::child_alive(mgr).await {
            return Err(format!(
                "FreeBank stopped while starting. {}",
                log_tail(&s.datadir).unwrap_or_default()
            )
            .trim()
            .to_string());
        }
        let p = detect::probe(&mgr.http, s).await;
        match p.state {
            detect::RpcState::Up => {
                if let Ok(mut c) = local_client(mgr, s, Duration::from_secs(20)) {
                    if c.call_fresh("getwalletinfo", vec![]).await.is_ok() {
                        return Ok(());
                    }
                }
            }
            detect::RpcState::Locked => return Err(p.message),
            detect::RpcState::Warming if !p.message.is_empty() => report.note(p.message),
            _ => report.note(log_tail(&s.datadir).unwrap_or_else(|| "Starting FreeBank…".into())),
        }
        if Instant::now() > until {
            return Err("FreeBank is taking a long time to start. The Node tab shows what it is doing.".into());
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
}

async fn start_and_wait(mgr: &NodeManager, s: &Settings, report: &Report) -> Result<(), String> {
    process::start(mgr).await?;
    wait_up(mgr, s, true, Duration::from_secs(15 * 60), report).await
}

/// After encryptwallet: freebankd stops by itself. The app starts its own node again; a node another
/// program runs has to be started there, and this waits for it.
///
/// COORDINATOR: this is the one place that restarts the node after encryptwallet, using
/// process::child_alive and process::start as they are on a26e147. If the node agent's re-adoption
/// ("keep the node running after quit") means the app's own node may not be its child, decide `ours`
/// (and how to start it) here.
pub(crate) async fn restart_after_encrypt(mgr: &NodeManager, s: &Settings, ours: bool, report: &Report) -> Result<(), String> {
    if ours {
        report.note("FreeBank stops to finish encrypting the wallet, then starts again.");
        let until = Instant::now() + Duration::from_secs(300);
        while process::child_alive(mgr).await {
            if Instant::now() > until {
                return Err("FreeBank didn't stop after encrypting the wallet. Stop and start it on the Node tab, \
                            then carry on here."
                    .into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        return start_and_wait(mgr, s, report).await;
    }
    report.waiting(true);
    report.note(
        "Your node was started by another program. It stops now to finish encrypting the wallet: start it \
         again there, and FreeBank carries on.",
    );
    // First it goes down (or it is already back, with a passphrase: started since, by its uptime; just after
    // encryptwallet the old process still answers for a moment while it shuts down, so an answer alone isn't enough)...
    let entered = Instant::now();
    let until = entered + Duration::from_secs(180);
    loop {
        let p = detect::probe(&mgr.http, s).await;
        if p.state == detect::RpcState::Down {
            break;
        }
        if p.state == detect::RpcState::Up {
            if let Ok(mut c) = local_client(mgr, s, Duration::from_secs(20)) {
                let restarted = c
                    .call_fresh("uptime", vec![])
                    .await
                    .ok()
                    .and_then(|u| u.as_u64())
                    .is_some_and(|up| up <= entered.elapsed().as_secs() + 1);
                if restarted {
                    if let Ok(i) = c.call_fresh("getwalletinfo", vec![]).await {
                        if i.get("unlocked_until").is_some() {
                            report.waiting(false);
                            return Ok(());
                        }
                    }
                }
            }
        }
        if Instant::now() > until {
            break;
        }
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    // ...then someone starts it again.
    let r = wait_up(mgr, s, false, Duration::from_secs(60 * 60), report).await;
    report.waiting(false);
    r
}

/// Stop the app's node and move the wallet out of its place, into a new folder beside the data
/// folder: `<datadir>.old-<unix time>/wallet.dat.old-<local time>`, the way setup moves a data folder
/// aside, and recorded the same way (Settings.moved_aside), so Obliterate, Back up and the Security
/// panel treat it as FreeBank's own. With `with`, a copy of that backup file takes the wallet's place
/// (0600). Returns where the old wallet went (None if there was none).
pub(crate) async fn replace_wallet(mgr: &NodeManager, s: &Settings, with: Option<&Path>) -> Result<Option<PathBuf>, String> {
    if process::someone_elses_node(mgr).await {
        return Err("A FreeBank node started by another program is running. Stop it there first.".into());
    }
    if !mgr.can_start(s) {
        return Err("FreeBank can only restore into the node it runs itself, and it isn't installed here.".into());
    }
    process::stop(mgr).await?;
    let datadir = PathBuf::from(&s.datadir);
    let wallet = default_wallet(&datadir);
    let moved = if wallet.exists() { Some(move_wallet_aside(&datadir, &wallet)?) } else { None };
    if let Some(src) = with {
        if let Err(e) = copy_private(src, &wallet) {
            // Put the wallet back where it was.
            if let Some(m) = &moved {
                let _ = std::fs::rename(m, &wallet);
                if let Some(d) = m.parent() {
                    let _ = std::fs::remove_dir(d);
                }
            }
            return Err(e);
        }
    }
    if let Some(dir) = moved.as_deref().and_then(Path::parent) {
        // Without the record the folder still stays: Obliterate lists it as a look-alike it never
        // removes. So a failure to write settings.json doesn't stop the restore.
        let _ = record_moved_aside(mgr, dir).await;
    }
    Ok(moved)
}

/// Record a folder FreeBank moved something into, as setup records its moves (Settings.moved_aside).
pub(crate) async fn record_moved_aside(mgr: &NodeManager, dir: &Path) -> Result<(), String> {
    let d = dir.to_string_lossy().into_owned();
    let mut s = mgr.settings.lock().await.clone();
    if s.moved_aside.contains(&d) {
        return Ok(());
    }
    s.moved_aside.push(d);
    mgr.save_settings(s).await
}

/// Forget a recorded folder that is gone again (a failed restore put the wallet back).
pub(crate) async fn forget_moved_aside(mgr: &NodeManager, dir: &Path) -> Result<(), String> {
    let d = dir.to_string_lossy().into_owned();
    let mut s = mgr.settings.lock().await.clone();
    if !s.moved_aside.contains(&d) {
        return Ok(());
    }
    s.moved_aside.retain(|m| *m != d);
    mgr.save_settings(s).await
}

pub(crate) fn move_wallet_aside(datadir: &Path, wallet: &Path) -> Result<PathBuf, String> {
    let parent = datadir.parent().ok_or("The data folder has no parent folder.")?;
    let name = datadir.file_name().ok_or("The data folder has no name.")?.to_string_lossy().into_owned();
    let mut secs = now();
    let dir = loop {
        let d = parent.join(format!("{}.old-{}", name, secs));
        match std::fs::create_dir(&d) {
            Ok(()) => break d,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => secs += 1,
            Err(e) => return Err(format!("Couldn't make {}: {}", d.display(), e)),
        }
    };
    seed::private_dir(&dir)?;
    let dest = dir.join(format!("wallet.dat.old-{}", stamp()));
    if std::fs::rename(wallet, &dest).is_err() {
        // Another disk: copy, then remove the original once the copy is whole.
        copy_private(wallet, &dest)?;
        std::fs::remove_file(wallet).map_err(|e| format!("Couldn't move {}: {}", wallet.display(), e))?;
    }
    Ok(dest)
}

/// Copy `from` to a new file `to`, readable by this user only, flushed to disk.
pub(crate) fn copy_private(from: &Path, to: &Path) -> Result<(), String> {
    let mut src = std::fs::File::open(from).map_err(|e| format!("Couldn't read {}: {}", from.display(), e))?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut dst = opts.open(to).map_err(|e| format!("Couldn't write {}: {}", to.display(), e))?;
    std::io::copy(&mut src, &mut dst)
        .and_then(|_| dst.sync_all())
        .map_err(|e| format!("Couldn't write {}: {}", to.display(), e))
}

// ---- Restore a backup file ----

async fn restore_file(mgr: &NodeManager, upload: &Path, report: &Report) -> Result<(), String> {
    let _busy = mgr.busy("Restoring your wallet from a backup…")?;
    let s = mgr.settings.lock().await.clone();
    report.stages(&["aside", "start", "scan"]);
    report.stage("aside");
    let wallet = default_wallet(Path::new(&s.datadir));
    let moved = replace_wallet(mgr, &s, Some(upload)).await?;
    report.aside(moved.as_deref());
    report.stage("start");
    if let Err(e) = start_and_wait(mgr, &s, report).await {
        return Err(match put_back(mgr, &s, &wallet, moved.as_deref(), report).await {
            Ok(()) => format!("Your node couldn't use that backup ({}). FreeBank put your wallet back.", e),
            Err(b) => format!(
                "Your node couldn't use that backup ({}), and FreeBank couldn't put your wallet back: {} Your wallet is kept at {}.",
                e,
                b,
                moved.map(|m| m.to_string_lossy().into_owned()).unwrap_or_default()
            ),
        });
    }
    report.stage("scan");
    let mut c = local_client(mgr, &s, LONG)?;
    rescan(&mut c, None, report).await
}

/// After a backup the node couldn't use: the wallet goes back in its place and the node starts.
async fn put_back(mgr: &NodeManager, s: &Settings, wallet: &Path, moved: Option<&Path>, report: &Report) -> Result<(), String> {
    process::stop(mgr).await?;
    // The copy of the backup goes; the user still has the file they chose.
    std::fs::remove_file(wallet).map_err(|e| format!("Couldn't remove {}: {}", wallet.display(), e))?;
    if let Some(m) = moved {
        std::fs::rename(m, wallet).map_err(|e| format!("Couldn't move {} back: {}", m.display(), e))?;
        if let Some(d) = m.parent() {
            let _ = std::fs::remove_dir(d);
            if !d.exists() {
                let _ = forget_moved_aside(mgr, d).await;
            }
        }
    }
    report.aside(None);
    start_and_wait(mgr, s, report).await
}

// ---- The scan ----

/// Scan the whole chain for the wallet's coins, in chunks sized to take a few seconds each, so the
/// screen can show the block reached. With the passphrase (and the app's relock guard) the wallet
/// stays unlocked while it scans, so its key pool keeps topping up as keys turn out to be in use: it
/// unlocks at the start, and again only when less than a minute of that unlock is left.
pub(crate) async fn rescan(c: &mut FreeBankClient, unlocking: Option<(&str, &RelockGuard)>, report: &Report) -> Result<(), String> {
    rescan_renewing(c, unlocking, report, Duration::from_secs(UNLOCK_SECS - 60)).await
}

/// `rescan`, unlocking again once `renew_after` has passed since the last unlock.
pub(crate) async fn rescan_renewing(
    c: &mut FreeBankClient,
    unlocking: Option<(&str, &RelockGuard)>,
    report: &Report,
    renew_after: Duration,
) -> Result<(), String> {
    let tip = c.call_ui("getblockcount", vec![]).await?.as_u64().unwrap_or(0);
    let mut from = 0u64;
    let mut chunk = 500u64;
    let mut unlocked_at: Option<Instant> = None;
    report.scan(0, tip);
    while from <= tip {
        let to = (from + chunk - 1).min(tip);
        if let Some((p, guard)) = unlocking {
            if unlocked_at.is_none_or(|t| t.elapsed() >= renew_after) {
                unlock(c, guard, p, UNLOCK_SECS).await?;
                unlocked_at = Some(Instant::now());
            }
        }
        let t = Instant::now();
        c.call_ui("rescanblockchain", vec![json!(from), json!(to)]).await?;
        let took = t.elapsed();
        if took < Duration::from_secs(2) {
            chunk = (chunk * 2).min(50_000);
        } else if took > Duration::from_secs(10) {
            chunk = (chunk / 2).max(50);
        }
        report.scan(to, tip);
        from = to + 1;
    }
    // Blocks that arrived while it scanned.
    let now_tip = c.call_ui("getblockcount", vec![]).await?.as_u64().unwrap_or(tip);
    if now_tip > tip {
        c.call_ui("rescanblockchain", vec![json!(tip + 1), json!(now_tip)]).await?;
        report.scan(now_tip, now_tip);
    }
    Ok(())
}
