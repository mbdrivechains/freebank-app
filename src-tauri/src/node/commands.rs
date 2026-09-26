//! Tauri commands for the first-run flow and the Node tab.

use super::{conf_tag, default_datadir, detect, install, platform, process, NodeManager, Settings};
use crate::rpc::FreeBankClient;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

type Mgr = Arc<NodeManager>;

#[derive(Debug, Serialize)]
pub struct SetupInfo {
    pub settings: Settings,
    /// Set when this machine has no FreeBank build.
    pub platform_error: Option<String>,
    pub suggested_tag: String,
    /// The name already in freebank.conf, if any.
    pub current_tag: Option<String>,
    /// freebankd is installed by this app and present on disk.
    pub installed: bool,
    pub default_datadir: String,
    pub app_version: String,
}

#[tauri::command]
pub async fn setup_info(mgr: State<'_, Mgr>) -> Result<SetupInfo, String> {
    let settings = mgr.settings.lock().await.clone();
    let installed = settings
        .installed_tag
        .as_deref()
        .map(|t| mgr.freebankd(t).is_file())
        .unwrap_or(false);
    Ok(SetupInfo {
        current_tag: conf_tag(Path::new(&settings.datadir)),
        settings,
        platform_error: platform().err(),
        suggested_tag: install::suggest_tag(),
        installed,
        default_datadir: default_datadir().to_string_lossy().into_owned(),
        app_version: super::APP_VERSION.to_string(),
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
    s.datadir = datadir.to_string();
    s.rpc_port = input.rpc_port;
    s.p2p_port = input.p2p_port;
    mgr.save_settings(s.clone()).await?;
    Ok(s)
}

#[tauri::command]
pub async fn stack_check(mgr: State<'_, Mgr>) -> Result<detect::StackCheck, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(detect::check_stack(&mgr.http, &s.rest, &s.enforcer).await)
}

#[tauri::command]
pub async fn node_probe(mgr: State<'_, Mgr>) -> Result<detect::Probe, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(detect::probe(&mgr.http, &s).await)
}

#[tauri::command]
pub async fn datadir_check(mgr: State<'_, Mgr>) -> Result<detect::DatadirCheck, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(detect::check_datadir(Path::new(&s.datadir)))
}

#[tauri::command]
pub fn validate_tag(tag: String) -> Result<(), String> {
    install::validate_tag(&tag)
}

#[tauri::command]
pub async fn install_start(mgr: State<'_, Mgr>, tag: String, move_aside: bool) -> Result<(), String> {
    install::validate_tag(&tag)?;
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
    tauri::async_runtime::spawn(install::run(mgr.inner().clone(), tag, move_aside));
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
    let saved = mgr.settings.lock().await.grpcurl.clone();
    let grpcurl = install::find_grpcurl(&mgr.app_dir, saved.as_deref()).map(|(p, _)| p);
    Ok(detect::test_connection(
        &mgr.http,
        grpcurl.as_deref(),
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
        (Ok(l), Some(i)) => super::patch(l) > super::patch(i),
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
            ..Default::default()
        };
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
    c.call("getblockchaininfo", vec![]).await.map(|_| true)
}
