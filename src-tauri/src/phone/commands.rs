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
    pub fn get(&self) -> Result<&Arc<Phone>, String> {
        self.0.as_ref().map_err(|e| format!("The phone relay isn't available: {e}"))
    }

    /// For what "Approve sends on my phone" guards: the phone link, or None when it isn't set up. One that failed to
    /// start while its files say the setting is on refuses, rather than letting payments through (security re-review
    /// L4).
    pub fn guard(&self, app_dir: &Path) -> Result<Option<&Arc<Phone>>, String> {
        match &self.0 {
            Ok(p) => Ok(Some(p)),
            Err(e) if super::store::Store::new(app_dir).load_config().approve_over.is_some() => Err(format!(
                "\"Approve sends on my phone\" is on, but FreeBank's phone link didn't start ({e}), so it can't ask your \
                 phone. Restart FreeBank."
            )),
            Err(_) => Ok(None),
        }
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

/// What the app found at its start: the background part it took the phone link back from (when
/// that had started, unix seconds), or why it couldn't (background.rs, `take_back`).
pub struct PhoneBackground(pub Result<Option<u64>, String>);

#[derive(Serialize)]
pub struct KeepInfo {
    /// "Keep your phone connected when FreeBank is closed".
    pub keep: bool,
    /// Asked already (once, after the first phone pairs).
    pub asked: bool,
    /// The app took the link back at its start from a background part running since then.
    pub took_back: Option<u64>,
    pub take_back_error: Option<String>,
    /// "Start when I log in" (daemon mode, `login_item.rs`), and whether this system has it.
    pub at_login: bool,
    pub at_login_here: bool,
}

#[tauri::command]
pub async fn phone_keep_info(
    mgr: State<'_, Arc<crate::node::NodeManager>>,
    bg: State<'_, PhoneBackground>,
) -> Result<KeepInfo, String> {
    let s = mgr.settings.lock().await.clone();
    let (took_back, take_back_error) = match &bg.0 {
        Ok(t) => (*t, None),
        Err(e) => (None, Some(e.clone())),
    };
    Ok(KeepInfo {
        keep: s.keep_phone,
        asked: s.keep_phone_asked,
        took_back,
        take_back_error,
        at_login: super::login_item::is_on(&mgr.app_dir),
        at_login_here: cfg!(any(target_os = "macos", target_os = "linux")),
    })
}

/// The switch, and the question after the first pairing. On also keeps the node running.
#[tauri::command]
pub async fn phone_keep_set(mgr: State<'_, Arc<crate::node::NodeManager>>, on: bool) -> Result<(), String> {
    mgr.still_here()?;
    let mut s = mgr.settings.lock().await.clone();
    s.keep_phone = on;
    s.keep_phone_asked = true;
    if on {
        s.keep_running = true;
    } else {
        // Starting at login is part of keeping the phone connected.
        super::login_item::set(&mgr.app_dir, false)?;
    }
    mgr.save_settings(s).await
}

/// "Start when I log in" (daemon mode): the login item, and with it "Keep my phone connected" and "Keep running", so
/// the node a phone woke stays when the app opens and takes the link back.
#[tauri::command]
pub async fn phone_login_set(mgr: State<'_, Arc<crate::node::NodeManager>>, on: bool) -> Result<(), String> {
    mgr.still_here()?;
    super::login_item::set(&mgr.app_dir, on)?;
    crate::activity::note(if on { "daemon: start at login on" } else { "daemon: start at login off" });
    if on {
        let mut s = mgr.settings.lock().await.clone();
        s.keep_phone = true;
        s.keep_phone_asked = true;
        s.keep_running = true;
        mgr.save_settings(s).await?;
    }
    Ok(())
}

/// "Keep the phone connected" in the close notice: start the background part with the phone-send
/// passphrase (if on), then close. The node keeps running ("Keep running" is on with this setting).
#[tauri::command]
pub async fn phone_keep_connected_quit(
    app: AppHandle,
    mgr: State<'_, Arc<crate::node::NodeManager>>,
    phone: State<'_, PhoneState>,
) -> Result<(), String> {
    let p = phone.get()?;
    super::background::spawn(&mgr.app_dir, p.passphrase_for_handover())?;
    super::background::HANDED_OVER.store(true, std::sync::atomic::Ordering::SeqCst);
    app.exit(0);
    Ok(())
}

/// At quit without the close notice (⌘Q, the Dock's or the menu's Quit on a Mac, code review 1):
/// with "Keep my phone connected" on, a phone paired and the node left running, start the background
/// part as the notice's "Keep the phone connected" would. Not after that was done already, after
/// "Stop everything" (the node is stopped), or after Obliterate.
pub fn keep_at_exit(mgr: &crate::node::NodeManager, phone: &PhoneState) {
    use std::sync::atomic::Ordering;
    use crate::node::{background as node_bg, process};
    if super::background::HANDED_OVER.load(Ordering::SeqCst) || mgr.obliterated.load(Ordering::SeqCst) {
        return;
    }
    let Ok(p) = &phone.0 else { return };
    let keep = mgr.settings.try_lock().is_ok_and(|s| s.keep_phone && s.keep_running);
    if !keep || !p.has_phones() {
        return;
    }
    let node_stays = node_bg::adopted_running(mgr)
        || tauri::async_runtime::block_on(async { process::child_alive(mgr).await && node_bg::outlives_now(mgr) });
    if node_stays && super::background::spawn(&mgr.app_dir, p.passphrase_for_handover()).is_ok() {
        super::background::HANDED_OVER.store(true, Ordering::SeqCst);
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
pub async fn phone_pair_answer(phone: State<'_, PhoneState>, id: String, allow: bool) -> Result<(), String> {
    let p = phone.get()?.clone();
    // While "Approve sends on my phone" is on, a new phone could approve sends: pairing one takes a paired phone's Face
    // ID too (v0.2.5).
    if allow && p.approve_over().is_some() {
        let ask = p.pair_pending().into_iter().find(|a| a.id == id).ok_or("That phone is no longer waiting.")?;
        let text = format!("Pair a new phone, \"{}\", with this desktop (it shows {})", ask.name, ask.code);
        p.request_approval(super::Approve::Change { text }).await?;
    }
    p.pair_answer(&id, allow)
}

#[derive(Serialize)]
pub struct ApproveInfo {
    /// ECX: once the desktop's payments in a day would come to more than this, a phone's Face ID first; None when off.
    pub over: Option<f64>,
    /// ECX the desktop can still pay today without asking.
    pub left: Option<f64>,
    /// The paired phones with Face ID, which can approve.
    pub approvers: usize,
    /// Approvals the desktop is waiting for now: `{id, text, expires}`.
    pub waiting: Vec<Value>,
    /// A change the recovery words made, waiting its day: `{over (ECX, null: off), due (unix seconds)}`.
    pub scheduled: Option<Value>,
}

/// "Approve sends on my phone" (v0.2.5): its amount, what is left today, and who can approve.
#[tauri::command]
pub fn phone_approve_info(phone: State<'_, PhoneState>) -> Result<ApproveInfo, String> {
    let p = phone.get()?;
    Ok(ApproveInfo {
        over: p.approve_over().map(to_ecx),
        left: p.desk_left().map(to_ecx),
        approvers: p.approvers(),
        waiting: p.approvals_waiting(),
        scheduled: p.scheduled().map(|s| serde_json::json!({"over": s.over.map(to_ecx), "due": s.due})),
    })
}

/// Set it: `over` in ECX, None to turn it off. Off or a higher amount takes a phone's Face ID at once, or the wallet's
/// recovery `words` a day from now (the answer: when, unix seconds). The words must make the key the node's wallet
/// uses, and match FreeBank's copy of them. Turning it on needs that copy: it is the way back from a lost phone.
#[tauri::command]
pub async fn phone_approve_set(
    phone: State<'_, PhoneState>,
    mgr: State<'_, Arc<crate::node::NodeManager>>,
    over: Option<f64>,
    words: Option<String>,
) -> Result<Option<u64>, String> {
    mgr.still_here()?;
    let p = phone.get()?.clone();
    let over = match over {
        Some(v) if !(v > 0.0) || !v.is_finite() => return Err("Enter an amount in ECX above zero.".into()),
        Some(v) => Some(super::store::json_to_sats(&serde_json::json!(v))?),
        None => None,
    };
    if over.is_some() && p.approve_over().is_none() {
        let node = p.hd_seed_id().await?;
        if node.is_none() || crate::seed::sealed_key_id_hex(&mgr.app_dir) != node {
            return Err("This needs your wallet's recovery words kept in FreeBank (Settings > Wallet): they are how you \
                        turn it off if you lose your phone."
                .into());
        }
    }
    let words_ok = match words {
        Some(w) => {
            let w = Zeroizing::new(w);
            let theirs = crate::seed::words_key_id_hex(&w)?;
            if theirs.is_none() || theirs != p.hd_seed_id().await? || !crate::seed::words_are_this_wallets(&mgr.app_dir, &w)? {
                return Err("Those aren't this wallet's recovery words.".into());
            }
            true
        }
        None => false,
    };
    p.set_approve_over(over, words_ok).await
}

/// "Cancel" on the change the recovery words made, while it waits its day.
#[tauri::command]
pub fn phone_approve_cancel_scheduled(phone: State<'_, PhoneState>) -> Result<(), String> {
    phone.get()?.cancel_scheduled();
    Ok(())
}

/// "Cancel" on the desktop's screen while it waits for a phone's approval.
#[tauri::command]
pub fn phone_approval_cancel(phone: State<'_, PhoneState>, id: String) -> Result<(), String> {
    phone.get()?.cancel_approval(&id);
    Ok(())
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
    /// Face ID: the phone added a passkey; `face_id_sends`, each send asks for it too.
    pub face_id: bool,
    pub face_id_sends: bool,
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
            face_id: d.passkey.is_some(),
            face_id_sends: d.passkey.as_ref().is_some_and(|k| k.sends),
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

/// "Remove Face ID": the phone's passkey goes (a phone that lost it can then open without it).
#[tauri::command]
pub async fn phone_remove_passkey(phone: State<'_, PhoneState>, id: String) -> Result<(), String> {
    phone.get()?.clone().remove_passkey_asked(&id).await
}

#[tauri::command]
pub async fn phone_set_limit(phone: State<'_, PhoneState>, id: String, limit: f64) -> Result<(), String> {
    phone.get()?.clone().set_limit(&id, limit).await
}

/// While "Approve sends on my phone" is on, a phone approves `text` first: something that would undo the setting or
/// change the wallet under it (Obliterate, a new wallet or seed, a restore; security re-review H2, M1).
pub async fn approve_change(phone: &PhoneState, app_dir: &Path, text: &str) -> Result<(), String> {
    if let Some(p) = phone.guard(app_dir)? {
        if p.approve_over().is_some() {
            p.request_approval(super::Approve::Change { text: text.into() }).await?;
        }
    }
    Ok(())
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
