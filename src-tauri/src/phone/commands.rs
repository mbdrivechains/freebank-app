//! What the Settings screen's Phone section calls, and the Tauri glue: the node RPC behind the
//! narrow door, events to the screen, and starting the relay link.

use super::{
    store::to_ecx, unix_now, Ask, BoxFuture, Confirmed, Events, HeldView, LinkStatus, Phone, Rpc, RpcFail,
    WalletView,
};
use crate::rpc::{FreeBankClient, RpcError};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// The phone relay, or why it couldn't start (e.g. the desktop key couldn't be written).
pub struct PhoneState(pub Result<Arc<Phone>, String>);

impl PhoneState {
    fn get(&self) -> Result<&Arc<Phone>, String> {
        self.0.as_ref().map_err(|e| format!("The phone relay isn't available: {e}"))
    }

    /// At quit: the phone-send passphrase is wiped from memory.
    pub fn forget_passphrase(&self) {
        if let Ok(p) = &self.0 {
            p.forget_passphrase();
        }
    }
}

/// The wallet's RPC client, keeping Core's error codes.
pub(crate) struct NodeRpc(pub(crate) Arc<Mutex<FreeBankClient>>);

impl Rpc for NodeRpc {
    fn call<'a>(&'a self, method: &'a str, params: Vec<Value>) -> BoxFuture<'a, Result<Value, RpcFail>> {
        Box::pin(async move {
            self.0.lock().await.call_fresh_typed(method, params).await.map_err(|e| match e {
                RpcError::Rpc { code, message } => RpcFail::rpc(code, message),
                e => RpcFail::other(e.to_string()),
            })
        })
    }
}

struct AppEvents(AppHandle);

impl Events for AppEvents {
    fn emit(&self, name: &str, payload: Value) {
        let _ = self.0.emit(name, payload);
    }
}

/// Load the phone state and start the relay link (it stays idle until a phone is paired) and the
/// clock that expires held sends.
pub fn start(
    app: &AppHandle,
    app_dir: &Path,
    client: Arc<Mutex<FreeBankClient>>,
    relock: Arc<crate::wallet::RelockGuard>,
) -> PhoneState {
    let _ = std::fs::create_dir_all(app_dir);
    match Phone::new(app_dir, Arc::new(NodeRpc(client)), Arc::new(AppEvents(app.clone())), Arc::new(unix_now)) {
        Ok((phone, out)) => {
            // Unlocks from the phone link and from the screens take turns (the freebankd deadlock).
            phone.share_relock_guard(relock);
            tauri::async_runtime::spawn(super::link::run(phone.clone(), out));
            tauri::async_runtime::spawn(phone.clone().expire_forever());
            PhoneState(Ok(phone))
        }
        Err(e) => PhoneState(Err(e)),
    }
}

#[derive(Serialize)]
pub struct PairStart {
    pub url: String,
    /// unix seconds
    pub expires: u64,
}

#[tauri::command]
pub fn phone_pair_start(phone: State<'_, PhoneState>) -> Result<PairStart, String> {
    let (url, expires) = phone.get()?.pair_start()?;
    Ok(PairStart { url, expires })
}

/// Allow or deny one waiting pair request. Allowing it refuses the others.
#[tauri::command]
pub fn phone_pair_answer(phone: State<'_, PhoneState>, id: String, allow: bool) -> Result<(), String> {
    phone.get()?.pair_answer(&id, allow)
}

#[derive(Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub added: u64,
    pub last_seen: Option<u64>,
    /// ECX a day it may send without asking
    pub limit: f64,
    /// ECX of that sent today
    pub spent_today: f64,
    pub online: bool,
}

#[tauri::command]
pub fn phone_devices(phone: State<'_, PhoneState>) -> Result<Vec<DeviceView>, String> {
    let p = phone.get()?;
    let today = super::store::day_of(unix_now());
    Ok(p.devices()
        .into_iter()
        .map(|d| DeviceView {
            online: p.online(&d.id),
            limit: to_ecx(d.limit_sats),
            spent_today: to_ecx(d.spent_on(today)),
            id: d.id,
            name: d.name,
            added: d.added,
            last_seen: d.last_seen,
        })
        .collect())
}

#[tauri::command]
pub fn phone_revoke(phone: State<'_, PhoneState>, id: String) -> Result<(), String> {
    phone.get()?.revoke(&id)
}

#[tauri::command]
pub fn phone_set_limit(phone: State<'_, PhoneState>, id: String, limit: f64) -> Result<(), String> {
    phone.get()?.set_limit(&id, limit)
}

/// Confirm (send) or decline a held send. When the wallet is locked, `passphrase` unlocks it for
/// this one send; without one the answer says `need_passphrase` and the send keeps waiting.
#[tauri::command]
pub async fn phone_confirm_send(
    phone: State<'_, PhoneState>,
    id: String,
    allow: bool,
    passphrase: Option<String>,
) -> Result<Confirmed, String> {
    let passphrase = passphrase.map(Zeroizing::new);
    let p = phone.get()?.clone();
    p.confirm_send(&id, allow, passphrase).await
}

/// Is the wallet encrypted, locked, and are phone sends on?
#[tauri::command]
pub async fn phone_wallet(phone: State<'_, PhoneState>) -> Result<WalletView, String> {
    let p = phone.get()?.clone();
    Ok(p.wallet_view().await)
}

/// Turn on "Let my phone send while FreeBank is open": the passphrase is checked and kept in
/// memory only.
#[tauri::command]
pub async fn phone_send_on(phone: State<'_, PhoneState>, passphrase: String) -> Result<(), String> {
    let passphrase = Zeroizing::new(passphrase);
    let p = phone.get()?.clone();
    p.phone_send_on(passphrase).await
}

/// Turn phone sends off: the passphrase is wiped.
#[tauri::command]
pub fn phone_send_off(phone: State<'_, PhoneState>) -> Result<(), String> {
    phone.get()?.forget_passphrase();
    Ok(())
}

#[derive(Serialize)]
pub struct RelayStatus {
    pub url: String,
    pub room: String,
    #[serde(flatten)]
    pub link: LinkStatus,
    /// Phones waiting for "Allow this phone?", each with its comparison code
    pub pair_pending: Vec<Ask>,
    /// Sends waiting for the desktop
    pub held: Vec<HeldView>,
}

#[tauri::command]
pub fn phone_relay_status(phone: State<'_, PhoneState>) -> Result<RelayStatus, String> {
    let p = phone.get()?;
    Ok(RelayStatus {
        url: p.relay_url(),
        room: p.room.clone(),
        link: p.status(),
        pair_pending: p.pair_pending(),
        held: p.held(),
    })
}

#[tauri::command]
pub fn phone_set_relay(phone: State<'_, PhoneState>, url: String) -> Result<(), String> {
    phone.get()?.set_relay(&url)
}

/// The latest phone sends from the log, newest first.
#[tauri::command]
pub fn phone_recent_sends(phone: State<'_, PhoneState>) -> Result<Vec<Value>, String> {
    Ok(phone.get()?.store.recent_sends(20))
}
