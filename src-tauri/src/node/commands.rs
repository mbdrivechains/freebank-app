//! Tauri commands for the first-run flow and the Node tab.

use super::{conf_tag, default_datadir, detect, install, obliterate, platform, process, NodeManager, Settings};
use crate::rpc::FreeBankClient;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::sync::Mutex;

type Mgr = Arc<NodeManager>;

/// While the app updates itself (app_update.rs), no node install or update starts: the app restarts in a moment. Each
/// side sets its own flag and then looks at the other's, so the two never both go ahead.
const APP_UPDATING: &str = "The FreeBank app is updating itself and restarts in a moment. Try again after that.";
fn app_updating(mgr: &NodeManager) -> bool {
    mgr.app_updating.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Debug, Serialize)]
pub struct SetupInfo {
    pub settings: Settings,
    /// Set when this machine has no FreeBank build.
    pub platform_error: Option<String>,
    pub suggested_tag: String,
    /// The name already in freebank.conf, if any.
    pub current_tag: Option<String>,
    /// freebankd is installed by this app, checked and present on disk, and the node's data folder
    /// is there. Without the folder (removed by "Obliterate" or by hand) setup runs again and makes it.
    pub installed: bool,
    /// The installed release, when an earlier build of the app put it there without checking its
    /// signature: it isn't started, and setup installs again.
    pub unverified: Option<String>,
    pub default_datadir: String,
    pub app_version: String,
    /// This build has demo mode (relay.rs `GATEWAY`): setup offers it.
    pub demo_available: bool,
    /// The installed node runs in demo mode (v0.2.25 or newer), or none is installed yet.
    pub demo_ready: bool,
}

#[tauri::command]
pub async fn setup_info(mgr: State<'_, Mgr>) -> Result<SetupInfo, String> {
    let settings = mgr.settings.lock().await.clone();
    let installed = mgr.can_start(&settings);
    let demo_ready = settings.installed_tag.as_deref().map_or(true, super::relay::node_can_demo);
    Ok(SetupInfo {
        current_tag: conf_tag(Path::new(&settings.datadir)),
        unverified: mgr.unverified(&settings),
        settings,
        platform_error: platform().err(),
        suggested_tag: install::suggest_tag(),
        installed,
        default_datadir: default_datadir().to_string_lossy().into_owned(),
        app_version: super::APP_VERSION.to_string(),
        demo_available: super::relay::available(),
        demo_ready,
    })
}

/// The fields "Advanced" can change.
#[derive(Debug, Deserialize)]
pub struct SettingsInput {
    pub rest: String,
    pub enforcer: String,
    pub datadir: String,
    pub rpc_port: u16,
    pub p2p_port: u16,
}

#[tauri::command]
pub async fn setup_save(mgr: State<'_, Mgr>, input: SettingsInput) -> Result<Settings, String> {
    let datadir = input.datadir.trim();
    if datadir.is_empty() || !Path::new(datadir).is_absolute() {
        return Err("The data folder must be a full path.".into());
    }
    if input.rpc_port == input.p2p_port || input.rpc_port == 0 || input.p2p_port == 0 {
        return Err("Choose two different ports.".into());
    }
    let mut s = mgr.settings.lock().await.clone();
    s.rest = detect::normalize_endpoint(&input.rest);
    s.enforcer = detect::normalize_endpoint(&input.enforcer);
    // On a network whose node keeps its files in a folder of its own (easynet), that folder inside the chosen one.
    let fitted = crate::network::fit_datadir(crate::network::current(), Path::new(datadir));
    // Never inside another network's data folder, nor around it: removing one would take the other's wallet (v0.5.0).
    let top = crate::network::top_of(&mgr.app_dir);
    if let Some((name, _)) = crate::network::other_datadirs(&top, crate::network::current())
        .into_iter()
        .find(|(_, other)| crate::network::nested(&fitted, other))
    {
        return Err(format!("That folder holds your {name} node's data folder, or is inside it. Choose a folder of its own."));
    }
    s.datadir = fitted.to_string_lossy().into_owned();
    s.rpc_port = input.rpc_port;
    s.p2p_port = input.p2p_port;
    mgr.save_settings(s.clone()).await?;
    Ok(s)
}

#[tauri::command]
pub async fn stack_check(mgr: State<'_, Mgr>) -> Result<detect::StackCheck, String> {
    let s = mgr.settings.lock().await.clone();
    if s.demo {
        return Ok(super::relay::check(&mgr).await);
    }
    Ok(detect::check_stack(&mgr.http, &s.rest, &s.enforcer).await)
}

/// Demo mode on or off (setup's "Try the demo", Settings' "Leave demo mode"). The node starts with it next time.
#[tauri::command]
pub async fn node_set_demo(mgr: State<'_, Mgr>, on: bool) -> Result<Settings, String> {
    mgr.still_here()?;
    if on {
        super::relay::ensure(&mgr).await?;
    }
    let mut s = mgr.settings.lock().await.clone();
    s.demo = on;
    // The relay lives in the app, so a demo node must not outlive it: left running, the node would find nothing on
    // the relay's port, or whatever else took it.
    if on {
        s.keep_running = false;
        s.keep_phone = false;
        crate::phone::login_item::set(&mgr.app_dir, false)?;
    }
    mgr.save_settings(s.clone()).await?;
    Ok(s)
}

#[tauri::command]
pub async fn node_probe(mgr: State<'_, Mgr>) -> Result<detect::Probe, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(detect::probe(&mgr.http, &s).await)
}

#[tauri::command]
pub async fn datadir_check(mgr: State<'_, Mgr>) -> Result<detect::DatadirCheck, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(detect::setup_check(Path::new(&s.datadir), &s.created()))
}

#[tauri::command]
pub fn validate_tag(tag: String) -> Result<(), String> {
    install::validate_tag(&tag)
}

#[tauri::command]
pub async fn install_start(mgr: State<'_, Mgr>, tag: String, move_aside: bool) -> Result<(), String> {
    install::validate_tag(&tag)?;
    // An install under way (a second click, or the screen reloaded) keeps its task, so "Cancel"
    // can still stop it.
    if mgr.install.lock().unwrap().running {
        return Ok(());
    }
    // A cancelled install may not have noticed yet; let it finish stopping first.
    let old = mgr.install_task.lock().unwrap().take();
    if let Some(task) = old {
        task.abort();
        let _ = task.await;
    }
    {
        let mut p = mgr.install.lock().unwrap();
        if p.running {
            return Ok(());
        }
        *p = install::InstallProgress {
            running: true,
            stage: "release".into(),
            ..Default::default()
        };
    }
    if app_updating(&mgr) {
        *mgr.install.lock().unwrap() = install::InstallProgress::default();
        return Err(APP_UPDATING.into());
    }
    // Held while the task starts, so a "Cancel" right now finds it to stop.
    let mut slot = mgr.install_task.lock().unwrap();
    *slot = Some(tauri::async_runtime::spawn(install::run(mgr.inner().clone(), tag, move_aside)));
    Ok(())
}

/// "Cancel" on the install screen. Allowed while the install is still fetching (release, signature,
/// download, verify): from unpacking on it writes files and settings, and it finishes in moments anyway.
#[tauri::command]
pub fn install_cancel(mgr: State<'_, Mgr>) -> Result<(), String> {
    {
        let mut p = mgr.install.lock().unwrap();
        if !p.running {
            return Ok(());
        }
        if !install::cancellable(&p.stage) {
            return Err("It's too late to cancel: FreeBank is being set up and will be ready in a moment.".into());
        }
        // The task checks this before it writes anything, and stops there.
        p.cancelled = true;
        p.running = false;
    }
    if let Some(task) = mgr.install_task.lock().unwrap().as_ref() {
        task.abort();
    }
    let _ = std::fs::remove_dir_all(mgr.app_dir.join("tmp"));
    Ok(())
}

#[tauri::command]
pub fn install_progress(mgr: State<'_, Mgr>) -> install::InstallProgress {
    mgr.install.lock().unwrap().clone()
}

#[tauri::command]
pub async fn node_start(mgr: State<'_, Mgr>) -> Result<(), String> {
    process::start(&mgr).await
}

#[tauri::command]
pub async fn node_stop(mgr: State<'_, Mgr>) -> Result<(), String> {
    process::stop_managed(&mgr).await
}

/// "Test connection" under Advanced, on the addresses in the form (saved or not).
#[tauri::command]
pub async fn test_connection(
    mgr: State<'_, Mgr>,
    rest: String,
    enforcer: String,
) -> Result<Vec<detect::ConnCheck>, String> {
    Ok(detect::test_connection(
        &mgr.http,
        &detect::normalize_endpoint(&rest),
        &detect::normalize_endpoint(&enforcer),
    )
    .await)
}

#[tauri::command]
pub async fn node_set_tag(mgr: State<'_, Mgr>, tag: String) -> Result<String, String> {
    process::set_tag(&mgr, &tag).await.map(String::from)
}

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub installed: Option<String>,
    pub latest: Option<String>,
    /// A newer C++ release than the installed one exists.
    pub available: bool,
    pub error: Option<String>,
}

/// Is there a newer release than the one this app installed? Cached for 30 minutes unless `force`.
#[tauri::command]
pub async fn update_check(mgr: State<'_, Mgr>, force: bool) -> Result<UpdateInfo, String> {
    let installed = mgr.settings.lock().await.installed_tag.clone();
    if installed.is_none() {
        return Ok(UpdateInfo {
            installed,
            latest: None,
            available: false,
            error: None,
        });
    }
    let latest = mgr.latest_release(force).await;
    let available = match (&latest, &installed) {
        (Ok(l), Some(i)) => {
            let net = crate::network::current();
            crate::network::order(net, l) > crate::network::order(net, i)
        }
        _ => false,
    };
    Ok(UpdateInfo {
        installed,
        available,
        error: latest.as_ref().err().cloned(),
        latest: latest.ok(),
    })
}

#[tauri::command]
pub fn update_start(mgr: State<'_, Mgr>) -> Result<(), String> {
    {
        let mut p = mgr.update.lock().unwrap();
        if p.running {
            return Ok(());
        }
        *p = install::InstallProgress {
            running: true,
            stage: "release".into(),
            what: Some("update".into()),
            ..Default::default()
        };
    }
    if app_updating(&mgr) {
        *mgr.update.lock().unwrap() = install::InstallProgress::default();
        return Err(APP_UPDATING.into());
    }
    tauri::async_runtime::spawn(install::run_update(mgr.inner().clone()));
    Ok(())
}

#[tauri::command]
pub fn update_progress(mgr: State<'_, Mgr>) -> install::InstallProgress {
    mgr.update.lock().unwrap().clone()
}

#[tauri::command]
pub async fn remove_programs(mgr: State<'_, Mgr>) -> Result<process::Removed, String> {
    process::remove_programs(&mgr).await
}

#[tauri::command]
pub async fn delete_chain_data(mgr: State<'_, Mgr>) -> Result<(), String> {
    process::delete_chain_data(&mgr).await
}

/// Where "Obliterate" looks: Tauri's paths for this app, and the settings on file.
async fn obliterate_places(app: &AppHandle, mgr: &NodeManager) -> obliterate::Places {
    let s = mgr.settings.lock().await.clone();
    let ours = process::managed_pids(mgr).await;
    let keep = mgr.backups.lock().unwrap().iter().map(|b| PathBuf::from(&b.saved)).collect();
    let path = app.path();
    let local_data = path.app_local_data_dir().ok();
    obliterate::Places {
        home: path.home_dir().unwrap_or_else(|_| super::home()),
        app_dir: mgr.app_dir.clone(),
        // WebKitGTK keeps the screen's data (mediakeys/, storage/) in the app's own folder.
        screen_uses_app_dir: cfg!(target_os = "linux") && local_data.as_deref() == Some(mgr.app_dir.as_path()),
        // A developer's scratch folder may hold other work: only FreeBank's own files in it go. So does beta's folder when
        // another network's lives inside it (networks/, v0.5.0): beta's own files go, the other networks stay.
        app_dir_shared: std::env::var_os("FREEBANK_APP_DIR").is_some() || other_networks_inside(&mgr.app_dir),
        datadir: PathBuf::from(&s.datadir),
        others: crate::network::other_datadirs(&crate::network::top_of(&mgr.app_dir), crate::network::current())
            .into_iter()
            .map(|(name, d)| (name.to_string(), d))
            .collect(),
        created: s.created().iter().map(PathBuf::from).collect(),
        moved_aside: s.moved_aside.iter().map(PathBuf::from).collect(),
        // The screen's caches are the whole app's: only beta's Obliterate, the app's own, takes them.
        caches: if crate::network::current().id == crate::network::BETA.id { screen_caches(app) } else { Vec::new() },
        keep,
        ours,
        stack: stack_folders(mgr, &s),
        elsewhere: elsewhere(&s),
    }
}

/// The folders of the app's own eCash node and enforcer (v0.5.1): the settings' stack (its chain and data, its blocks on
/// a disk of their own), then any other chain left in stack-data/ (an earlier chain, or one an earlier Obliterate kept).
fn stack_folders(mgr: &NodeManager, s: &super::Settings) -> Vec<obliterate::StackFolder> {
    use obliterate::StackFolder;
    let mut v: Vec<StackFolder> = Vec::new();
    if let Some(st) = &s.stack {
        v.push(StackFolder { path: PathBuf::from(&st.dir), role: "chain" });
        v.extend(st.blocks.as_ref().map(|b| StackFolder { path: PathBuf::from(b), role: "blocks" }));
    }
    // Earlier chains: only folders that are a chain of the app's (its mark, or a chain's folder in a real stack-data/),
    // so a stack-data/ linked to another disk never lists that disk's other folders (review H1).
    let mut left: Vec<PathBuf> = std::fs::read_dir(mgr.app_dir.join("stack-data"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !v.iter().any(|f| &f.path == p))
        .filter(|p| p.join(crate::stack::MARK).exists() || crate::stack::in_own_data(&mgr.app_dir, p))
        .collect();
    left.sort();
    v.extend(left.into_iter().map(|path| StackFolder { path, role: "earlier" }));
    v
}

/// The eCash node's host when it is another computer's (the FreeBank node reads its REST there).
fn elsewhere(s: &super::Settings) -> Option<String> {
    if s.demo {
        return None;
    }
    let host = crate::security::host_part(&s.rest).to_string();
    (!host.is_empty() && !crate::security::is_loopback_host(&host)).then_some(host)
}

/// Beta's app folder holds another network's (networks/<id>/, v0.5.0).
fn other_networks_inside(app_dir: &Path) -> bool {
    crate::network::current().id == crate::network::BETA.id
        && crate::network::ALL.iter().any(|n| n.id != crate::network::BETA.id && crate::network::present(app_dir, n))
}

/// The folders the screen (the system webview) writes to. With FREEBANK_APP_DIR a developer's
/// scratch app shares them with the installed app, so then they are left alone.
fn screen_caches(app: &AppHandle) -> Vec<obliterate::Cache> {
    if std::env::var_os("FREEBANK_APP_DIR").is_some() {
        return Vec::new();
    }
    let path = app.path();
    let named = |id, p: Option<PathBuf>| {
        p.map(|path| obliterate::Cache {
            id,
            path,
            program_named: false,
        })
    };
    let mut caches = vec![
        named("cache", path.app_cache_dir().ok()),
        named("local-data", path.app_local_data_dir().ok()),
    ];
    // WebKitGTK names its cache and HSTS list after the program: ~/.cache/freebank and
    // ~/.local/share/freebank. Such a folder is listed only if it holds nothing but WebKit's files.
    #[cfg(target_os = "linux")]
    if let Some(program) = std::env::current_exe().ok().and_then(|e| e.file_name().map(|n| n.to_owned())) {
        for (id, dir) in [("webkit-cache", path.cache_dir().ok()), ("webkit-data", path.data_dir().ok())] {
            caches.push(dir.map(|d| obliterate::Cache {
                id,
                path: d.join(&program),
                program_named: true,
            }));
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = path.home_dir() {
        let id = &app.config().identifier;
        let lib = home.join("Library");
        caches.extend([
            named("webkit", Some(lib.join("WebKit").join(id))),
            named("caches", Some(lib.join("Caches").join(id))),
            named("http-storage", Some(lib.join("HTTPStorages").join(id))),
            named("cookies", Some(lib.join("HTTPStorages").join(format!("{}.binarycookies", id)))),
            named("saved-state", Some(lib.join("Saved Application State").join(format!("{}.savedState", id)))),
        ]);
    }
    caches.into_iter().flatten().collect()
}

/// What "Obliterate" would remove, with the wallet's balance.
#[tauri::command]
pub async fn obliterate_plan(app: AppHandle, mgr: State<'_, Mgr>) -> Result<obliterate::Plan, String> {
    let places = obliterate_places(&app, &mgr).await;
    obliterate::plan(&mgr, places).await
}

/// "Back up wallet" (Obliterate's list): into Documents, or the home folder if there is no
/// Documents folder.
#[tauri::command]
pub async fn wallet_backup(app: AppHandle, mgr: State<'_, Mgr>) -> Result<Vec<String>, String> {
    let path = app.path();
    let folder = path
        .document_dir()
        .ok()
        .filter(|d| d.is_dir())
        .or_else(|| path.home_dir().ok())
        .ok_or("FreeBank couldn't find your Documents folder or your home folder.")?;
    let saved = obliterate::backup_wallet(&mgr, &folder).await?;
    // Listed in <app data>/backups.json, so Settings > Security checks it in later sessions too.
    crate::security::record_backups(&mgr, &saved);
    Ok(saved)
}

/// "Obliterate": remove the ticked items. Only items of a fresh plan are acted on, never paths; the
/// path each tick carries must match its item's, so nothing goes that the screen didn't show.
#[tauri::command]
pub async fn obliterate(
    app: AppHandle,
    mgr: State<'_, Mgr>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    ticks: Vec<obliterate::Tick>,
) -> Result<obliterate::Outcome, String> {
    // It takes the phone link's settings with it, and the wallet may stay (security re-review H2).
    crate::phone::commands::approve_change(&phone, &mgr.app_dir, "Obliterate: remove FreeBank from this computer, with \
         \"Approve sends on my phone\" and your paired phones").await?;
    let places = obliterate_places(&app, &mgr).await;
    obliterate::run(&mgr, places, ticks).await
}

/// "Remove the app too", on the last screen after Obliterate: the .deb through the password prompt (its apt source
/// goes with it), the Mac app to the Bin through Finder. The screen then closes the app. Other kinds keep the
/// instructions on the screen.
#[tauri::command]
pub async fn remove_app_itself(mgr: State<'_, Mgr>) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REMOVING: AtomicBool = AtomicBool::new(false);
    if !mgr.obliterated.load(Ordering::SeqCst) {
        return Err("Remove FreeBank's data first.".into());
    }
    // Another network's wallet and settings are still on this computer (v0.5.0): the app stays for them.
    let top = crate::network::top_of(&mgr.app_dir);
    let here = crate::network::current();
    if let Some(n) = crate::network::ALL.iter().find(|n| n.id != here.id && crate::network::present(&top, n)) {
        return Err(format!("Your {} wallet and settings are still on this computer, so the app stays.", n.name));
    }
    if REMOVING.swap(true, Ordering::SeqCst) {
        return Err("FreeBank is being removed already.".into());
    }
    let r = remove_now().await;
    REMOVING.store(false, Ordering::SeqCst);
    r
}

async fn remove_now() -> Result<(), String> {
    let how = obliterate::how_to_remove_app();
    if !how.can_remove {
        return Err("Remove this copy by hand, as the screen says.".into());
    }
    match how.kind {
        "deb" => tokio::task::spawn_blocking(|| crate::admin::run_as_root(crate::admin::REMOVE_DEB_SCRIPT))
            .await
            .map_err(|_| "FreeBank couldn't ask for your password.".to_string())?,
        "mac" => {
            let bundle = PathBuf::from(how.path.unwrap_or_default());
            tokio::task::spawn_blocking(move || {
                crate::admin::trash_mac_bundle(&bundle).inspect_err(|_| crate::admin::reveal_mac_bundle(&bundle))
            })
            .await
            .map_err(|_| "FreeBank couldn't ask Finder.".to_string())?
        }
        _ => Err("Remove this copy by hand, as the screen says.".into()),
    }
}

/// "Close FreeBank" on the last screen. Exiting runs lib.rs's exit handler, which stops the node
/// and deletes what "Obliterate" left for then.
#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}

/// Ctrl+Q in the window (Linux windows have no menu bar): ask what to stop, as ⌘Q does on a Mac (background.rs).
#[tauri::command]
pub fn app_quit_asked(app: AppHandle) {
    super::background::quit_asked(&app);
}

#[tauri::command]
pub async fn node_progress(mgr: State<'_, Mgr>) -> Result<process::NodeProgress, String> {
    Ok(process::progress(&mgr).await)
}

#[tauri::command]
pub async fn node_status(mgr: State<'_, Mgr>) -> Result<process::NodeStatus, String> {
    process::status(&mgr).await
}

/// Point the wallet screens at the local node, with the credentials in its datadir.
#[tauri::command]
pub async fn connect_local(
    mgr: State<'_, Mgr>,
    client: State<'_, Arc<Mutex<FreeBankClient>>>,
) -> Result<bool, String> {
    let s = mgr.settings.lock().await.clone();
    let mut c = client.lock().await;
    if !c.configure_local(&format!("http://127.0.0.1:{}", s.rpc_port), s.datadir.clone().into()) {
        return Err(format!("No RPC cookie in {} yet.", s.datadir));
    }
    // A node busy connecting blocks answers late (it took 35-40 s once): the client is set up, and the screens ask
    // again, so "Continue while it syncs" doesn't wait for it (v0.2.5).
    match tokio::time::timeout(std::time::Duration::from_secs(3), c.call("getblockchaininfo", vec![])).await {
        Ok(r) => r.map(|_| true),
        Err(_) => Ok(true),
    }
}

/// "Keep FreeBank's node running after I close the app" (Settings).
#[tauri::command]
pub async fn node_set_keep_running(mgr: State<'_, Mgr>, on: bool) -> Result<Settings, String> {
    mgr.still_here()?;
    let mut s = mgr.settings.lock().await.clone();
    if on && s.demo {
        return Err(super::relay::DEMO_STOPS.into());
    }
    s.keep_running = on;
    // Keeping the phone connected needs the node left running (code review 6), and so does starting at login.
    if !on {
        s.keep_phone = false;
        crate::phone::login_item::set(&mgr.app_dir, false)?;
    }
    mgr.save_settings(s.clone()).await?;
    Ok(s)
}

/// Stop and start the app's node, so it runs as the settings now say (Settings, after "Keep
/// running" was turned on while it ran).
#[tauri::command]
pub async fn node_restart(mgr: State<'_, Mgr>) -> Result<(), String> {
    super::background::restart(&mgr).await
}

/// "Download it again and check it", for an installed release this code never checked. Progress
/// is on update_progress.
#[tauri::command]
pub fn refetch_start(mgr: State<'_, Mgr>) -> Result<(), String> {
    {
        let mut p = mgr.update.lock().unwrap();
        if p.running {
            return Ok(());
        }
        *p = install::InstallProgress {
            running: true,
            stage: "release".into(),
            what: Some("refetch".into()),
            ..Default::default()
        };
    }
    if app_updating(&mgr) {
        *mgr.update.lock().unwrap() = install::InstallProgress::default();
        return Err(APP_UPDATING.into());
    }
    tauri::async_runtime::spawn(install::run_refetch(mgr.inner().clone()));
    Ok(())
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// Beta's Obliterate keeps the other networks' folders inside its own (v0.5.0): the folder counts as shared then.
    #[test]
    fn beta_folder_with_networks_inside_is_shared() {
        let d = std::env::temp_dir().join(format!("fbnetobl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(!other_networks_inside(&d));
        std::fs::create_dir_all(d.join("networks")).unwrap();
        assert!(!other_networks_inside(&d), "an empty networks/ holds nothing to keep");
        std::fs::create_dir_all(d.join("networks/easynet")).unwrap();
        assert!(!other_networks_inside(&d), "nor an empty network folder");
        std::fs::write(d.join("networks/easynet/settings.json"), b"{}").unwrap();
        assert!(other_networks_inside(&d));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Obliterate names an eCash node on another computer (v0.5.1), by name or address; never this computer's own, nor
    /// in demo mode.
    #[test]
    fn another_computers_node_is_named() {
        let with = |rest: &str, demo: bool| elsewhere(&super::super::Settings { rest: rest.into(), demo, ..Default::default() });
        assert_eq!(with("nearby-box:18303", false).as_deref(), Some("nearby-box"));
        assert_eq!(with("100.64.0.2:38532", false).as_deref(), Some("100.64.0.2"));
        assert_eq!(with("[fd7a:115c:a1e0::2]:38532", false).as_deref(), Some("fd7a:115c:a1e0::2"));
        assert_eq!(with("127.0.0.1:38532", false), None);
        assert_eq!(with("localhost:18303", false), None);
        assert_eq!(with("[::1]:18303", false), None);
        assert_eq!(with("nearby-box:18303", true), None);
    }
}
