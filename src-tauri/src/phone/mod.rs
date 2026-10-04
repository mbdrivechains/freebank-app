//! The desktop side of the FreeBank phone relay (relay/PROTOCOL.md in mbdrivechains/freebank-phone):
//! the desktop key D and its room, pairing, the KK session handshake, the narrow door (balance,
//! history, receive, send, status), held sends, phone sends from an encrypted wallet, and the
//! outbound link to the relay.
//!
//! `Phone` is the whole state and knows nothing of Tauri or WebSockets: frames from the relay go
//! into `handle_frame`, frames for the relay come out of the channel `new` returns, the node is
//! reached through `Rpc` and the screen through `Events`. `link` runs the WebSocket; `commands`
//! are what the Settings screen calls.

pub mod background;
pub mod commands;
pub mod crypto;
pub mod link;
pub mod login_item;
pub mod store;
pub mod webauthn;
#[cfg(test)]
mod tests;

use crypto::Session;
use p256::SecretKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::{to_ecx, to_sats, Config, Device, Devices, HeldFile, Store};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Bitcoin Core's JSON-RPC error codes the phone relay acts on.
pub const RPC_WALLET_INSUFFICIENT_FUNDS: i64 = -6;
pub const RPC_WALLET_UNLOCK_NEEDED: i64 = -13;
pub const RPC_WALLET_PASSPHRASE_INCORRECT: i64 = -14;
pub const RPC_IN_WARMUP: i64 = -28;
/// Daemon mode's own answers for a request that reached no node (`background.rs`): it is being started, or it couldn't
/// be (the message says what to do). Never a code the node uses.
pub const WAKE_STARTING: i64 = -32_901;
pub const WAKE_FAILED: i64 = -32_902;

/// A node call that failed.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcFail {
    /// Core's JSON-RPC error code; None when no JSON-RPC answer came back (unreachable, HTTP
    /// error, timeout).
    pub code: Option<i64>,
    pub message: String,
    /// The node may have had the call and acted on it: no answer of its own came back after it was handed over (a
    /// timeout, a dropped connection, an answer that can't be read). A payment that fails so may have gone out (v0.2.7;
    /// the review of v0.2.7: daemon mode's "starting" must not hide it).
    pub maybe: bool,
}

impl RpcFail {
    /// The node's own refusal: it did nothing.
    pub fn rpc(code: i64, message: impl Into<String>) -> Self {
        Self { code: Some(code), message: message.into(), maybe: false }
    }

    /// No answer of the node's own: it may have acted.
    pub fn other(message: impl Into<String>) -> Self {
        Self { code: None, message: message.into(), maybe: true }
    }

    /// The call never reached a node (nothing listened, the login or address was refused).
    pub fn refused(message: impl Into<String>) -> Self {
        Self { code: None, message: message.into(), maybe: false }
    }

    /// No node answered: unreachable, or still warming up.
    pub fn unreachable(&self) -> bool {
        matches!(self.code, None | Some(RPC_IN_WARMUP))
    }

    /// In words for the wallet's owner.
    pub fn plain(&self) -> String {
        match self.code {
            None => "Your desktop can't reach its FreeBank node right now.".into(),
            Some(RPC_IN_WARMUP) => "Your desktop's FreeBank node is still starting up.".into(),
            Some(WAKE_STARTING) => ERR_STARTING.into(),
            Some(WAKE_FAILED) => self.message.clone(),
            Some(RPC_WALLET_INSUFFICIENT_FUNDS) => {
                "Not enough ECX in your desktop wallet for this payment and its fee.".into()
            }
            Some(_) => self.message.clone(),
        }
    }
}

impl From<RpcFail> for String {
    fn from(e: RpcFail) -> String {
        e.plain()
    }
}

/// The node, as the narrow door sees it. The app's implementation wraps the wallet's RPC client.
pub trait Rpc: Send + Sync {
    fn call<'a>(&'a self, method: &'a str, params: Vec<Value>) -> BoxFuture<'a, Result<Value, RpcFail>>;
}

/// The screen: "Allow this phone?", held sends, the send log.
pub trait Events: Send + Sync {
    fn emit(&self, name: &str, payload: Value);
}

pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub const PAIR_TTL_SECS: u64 = 300;
pub const HISTORY_MAX: u64 = 50;
pub const NAME_MAX: usize = 40;
/// Pair requests that can wait for one pairing code at once (the relay lets 8 phones in a room).
pub const MAX_ASKS: usize = 8;
/// A phone channel that hasn't completed `hello` or a pair request this long after its first
/// frame is closed, so a stranger with the room id can't hold the room's phone slots (P5).
pub const IDLE_CHANNEL_SECS: u64 = 30;
/// A held send waits this long for the desktop; then its phone is told it wasn't confirmed.
pub const HELD_TTL_SECS: u64 = 600;
/// Face ID (PROTOCOL.md, "Face ID: passkeys"): a request before the session proved the passkey. The
/// reply carries `"auth":"open"` too, so the page asks for Face ID.
pub const ERR_AUTH_NEEDED: &str = "Unlock with Face ID first.";
pub const ERR_AUTH_FAILED: &str = "Face ID didn't check out. Try again.";
/// How long a passkey challenge lasts, in seconds.
pub const CHALLENGE_SECS: u64 = 120;
/// A proved session asks again after this long without a request, and after `VERIFIED_MAX_SECS` in
/// all (security review M1: a phone left unlocked with FreeBank open).
pub const VERIFIED_IDLE_SECS: u64 = 300;
pub const VERIFIED_MAX_SECS: u64 = 1800;

/// A session's Face ID state: when it proved the passkey and last used it (None: not proved), and
/// its live challenges by purpose ("open", "send", "add", "change"), each good once until its time.
/// `gen` tells one session on a channel from the next (security review I1): a proof counts only for
/// the session whose challenge it used.
#[derive(Default)]
struct Auth {
    gen: u64,
    verified: Option<(u64, u64)>,
    challenges: HashMap<String, ([u8; 32], u64)>,
}

/// What the phone is told, with the app closed, about a send the open app would have held.
pub const ERR_CLOSED_LIMIT: &str =
    "This is over today's limit, and FreeBank is closed on your desktop. Open it there to send this.";
pub const ERR_CLOSED_LOCKED: &str =
    "Your wallet is locked, and FreeBank is closed on your desktop. Open it there to send this.";
/// A phone send from an encrypted wallet unlocks it for this long, and locks it right after.
pub const SEND_UNLOCK_SECS: u64 = 10;
/// How far an unlock keeps from the moment the node relocks after the previous one
/// (`crate::wallet::RelockGuard`).
pub const RELOCK_MARGIN: Duration = crate::wallet::RELOCK_MARGIN;

pub const ERR_DECLINED: &str = "declined on the desktop";
/// The fee a phone's note action pays, in ECX: what the desktop's Notes tab pays.
pub const NOTE_FEE: f64 = 0.001;
/// freebankd before v0.2.19 lists a wallet's notes only while it is unlocked, so there the phone's Notes need phone
/// sends on. v0.2.19 lists them locked (freebankd's note 2026-10-04-from-freebankd-list-locked-done), and this never shows.
pub const ERR_NOTES_LOCKED: &str = "Your desktop wallet is locked. To see your notes here, turn on \"Let my phone send while \
                                    FreeBank is open\" in FreeBank on your desktop (Settings, Phone).";
/// Daemon mode (`background.rs`, light): a paired phone asked, and the node is starting. The reply carries
/// `"starting": true`, so the page says so and asks again (PROTOCOL.md, "Wake on demand").
pub const ERR_STARTING: &str = "Your desktop is starting FreeBank. It answers in a minute or two.";
pub const ERR_RESTARTED: &str = "the desktop app restarted; nothing was sent";
pub const ERR_NOT_ENCRYPTED: &str =
    "This wallet has no passphrase, so phones already send up to their daily limit without one.";
pub const ERR_WRONG_PASSPHRASE: &str = "That isn't the wallet's passphrase.";
pub const ERR_MAY_HAVE_GONE: &str = "Your desktop's node didn't answer after it was handed this payment, so it may have \
                                     gone out. Check History before trying again.";

pub const EV_PAIR: &str = "phone-pair-request";
pub const EV_HELD: &str = "phone-held-send";
pub const EV_SEND: &str = "phone-send";
pub const EV_CHANGED: &str = "phone-changed";
/// An approval the desktop is waiting for (`{id, text, expires}`), or one that ended (`{id, done: true}`).
pub const EV_APPROVAL: &str = "phone-approval";

/// "Approve sends on my phone" (v0.2.5): how long the desktop waits for a phone's answer.
pub const APPROVE_SECS: u64 = 120;
pub const ERR_APPROVE_DECLINED: &str = "Declined on your phone.";
pub const ERR_APPROVE_CANCELLED: &str = "Cancelled on the desktop.";
pub const ERR_APPROVE_GONE: &str = "That approval is no longer waiting.";
pub const ERR_APPROVE_TIMEOUT: &str = "Not approved on your phone within 2 minutes. Open FreeBank on your phone, then try again.";
pub const ERR_NO_APPROVER: &str = "No paired phone has Face ID to approve this. Turn Face ID on in FreeBank on your phone \
                                   (Settings), or turn \"Approve sends on my phone\" off with your recovery words in \
                                   Settings > Phone (that waits a day).";
/// A phone turning its first Face ID on while approvals are on: another phone approves first (security review M2).
pub const ERR_ADD_ASKED: &str = "Your desktop asks your other phone first: approve it there with Face ID within 2 minutes, \
                                 then try again here.";
pub const ERR_ADD_REFUSED: &str = "Your other phone said no, or didn't answer. You can ask again in 10 minutes.";
pub const ERR_ADD_NO_APPROVER: &str = "\"Approve sends on my phone\" is on, and no other phone has Face ID to approve this \
                                       one. Turn it off on your desktop first.";
/// How long a change made with the recovery words waits (a day), and how long the desktop's payments count.
pub const WORDS_WAIT_SECS: u64 = 24 * 3600;
pub const DAY_SECS: u64 = 24 * 3600;
/// How long another phone's yes to a first Face ID lasts.
const GRANT_SECS: u64 = 600;
/// What a note action costs besides the notes: the fee, and a carrier output of 1000 sats for each note output
/// (freebankd's NOTE_DUST_VALUE), two at most (security review M3).
pub const NOTE_FEE_SATS: u64 = 100_000;
pub const NOTE_CARRIER_SATS: u64 = 2_000;
/// What a phone's held send counts for its fee when the desktop confirms it (the node chooses the fee).
pub const SEND_FEE_ALLOWANCE: u64 = 100_000;

/// The final err of a held send nobody answered: "not confirmed on the desktop within 10 minutes".
pub fn expired_text(ttl: u64) -> String {
    if ttl % 60 == 0 {
        let m = ttl / 60;
        format!("not confirmed on the desktop within {m} minute{}", if m == 1 { "" } else { "s" })
    } else {
        format!("not confirmed on the desktop within {ttl} seconds")
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CodeState {
    Live,
    /// A phone was allowed with it.
    Used,
}

struct Code {
    c: [u8; 16],
    expires: u64,
    state: CodeState,
}

/// A pair request waiting for "Allow this phone?". Several can wait on one pairing code (whoever
/// saw the QR code can ask); each shows its own comparison code, and the owner allows the one whose
/// code their phone shows.
#[derive(Clone, Serialize)]
pub struct Ask {
    #[serde(skip)]
    ch: u64,
    #[serde(skip)]
    p_pub: String,
    /// This request: the answer names it.
    pub id: String,
    /// The phone's device id.
    pub device: String,
    pub name: String,
    /// "042 917": allow only if the phone shows the same.
    pub code: String,
}

#[derive(Default)]
struct Pairing {
    code: Option<Code>,
    asks: Vec<Ask>,
}

/// What a phone's payment does: ECX to an address, or (v0.2.5) house notes sent to an address, redeemed for ECX, or
/// demanded. Each is one wallet call under the same guard: Face ID when the phone chose it, the daily limit (note
/// units count as ECX: 1 unit is 1 sat), and the desktop's confirmation above it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    #[default]
    Send,
    NoteSend,
    NoteRedeem,
    NoteDemand,
}

impl Kind {
    /// The phone's method for it.
    fn from_method(m: &str) -> Option<Kind> {
        match m {
            "send" => Some(Kind::Send),
            "note-send" => Some(Kind::NoteSend),
            "note-redeem" => Some(Kind::NoteRedeem),
            "note-demand" => Some(Kind::NoteDemand),
            _ => None,
        }
    }
}

/// A send waiting for the desktop: over the phone's remaining daily limit ("limit"), or the
/// wallet is encrypted and phone sends aren't on ("locked"). Kept in held.json until answered.
#[derive(Clone, Serialize, Deserialize)]
pub struct Held {
    pub confirm: String,
    pub device: String,
    pub name: String,
    /// Where it goes; empty for a redeem or a demand (the notes go back to their house).
    pub address: String,
    pub sats: u64,
    /// A send, or a note action (older held.json files have only sends).
    #[serde(default)]
    pub kind: Kind,
    /// The house, for a note action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub house: Option<u64>,
    /// unix seconds
    pub at: u64,
    /// "limit" or "locked"
    pub why: String,
    /// The phone's request id: the final reply goes out under it.
    pub req_id: Value,
    /// The asking phone's own Face ID signed it: "Approve sends on my phone" needn't ask again when the desktop
    /// confirms it.
    #[serde(default)]
    pub face_id: bool,
    /// The desktop is paying it right now; expiry and revoke leave it to that.
    #[serde(skip)]
    busy: bool,
    /// How "Approve sends on my phone" let the desktop's confirm through (a try again with the passphrase doesn't ask
    /// twice); a count is given back if it isn't paid.
    #[serde(skip)]
    cleared: Option<Cleared>,
    /// The channel of the session that asked: it hears the outcome even after its Face ID proof lapsed,
    /// since the send passed the gate (code review N1). Channels don't outlive a restart, nor do holds.
    #[serde(skip)]
    ch: u64,
}

impl Held {
    fn view(&self, ttl: u64) -> HeldView {
        HeldView {
            confirm: self.confirm.clone(),
            device: self.device.clone(),
            name: self.name.clone(),
            address: self.address.clone(),
            amount: to_ecx(self.sats),
            kind: self.kind,
            house: self.house,
            at: self.at,
            expires: self.at.saturating_add(ttl),
            why: self.why.clone(),
            face_id: self.face_id,
        }
    }
}

/// A held send as the screen shows it.
#[derive(Clone, Serialize)]
pub struct HeldView {
    pub confirm: String,
    pub device: String,
    pub name: String,
    pub address: String,
    pub amount: f64,
    pub kind: Kind,
    pub house: Option<u64>,
    pub at: u64,
    /// unix seconds: when it stops waiting
    pub expires: u64,
    pub why: String,
    /// The phone's own Face ID signed it.
    pub face_id: bool,
}

/// A held send a restart cancelled. Each new session of its phone is told so, until the send's
/// time would have run out.
#[derive(Clone, Serialize, Deserialize)]
pub struct Cancelled {
    pub confirm: String,
    pub device: String,
    pub at: u64,
    pub req_id: Value,
}

/// The link to the relay, as the Settings screen shows it.
#[derive(Clone, Serialize, Default)]
pub struct LinkStatus {
    /// "off" (no phone paired and no pairing open), "connecting", "online", "retrying"
    pub state: String,
    pub detail: String,
}

/// The wallet, as `getwalletinfo` tells it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Wallet {
    /// Not encrypted.
    Plain,
    Locked,
    /// Unlocked until this unix time.
    Unlocked(u64),
}

/// Why a payment didn't go out.
enum Pay {
    /// The wallet is locked and there is no passphrase to unlock it with. Nothing was sent.
    Locked,
    /// The passphrase given for this one send is wrong. Nothing was sent.
    WrongPassphrase,
    /// The node couldn't be asked before the send. Nothing was sent.
    NotTried(String),
    /// The node refused the send.
    Failed(String),
    /// Handed to the node, with no answer of its own (a timeout, a dropped connection): it may have gone out, so it
    /// keeps its count against the limits (v0.2.7).
    MayHaveGone,
}

/// The wallet as the Phone settings show it.
#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct WalletView {
    /// None when the node can't be asked right now.
    pub encrypted: Option<bool>,
    pub locked: bool,
    /// "Let my phone send while FreeBank is open" is on: the passphrase is held in memory.
    pub phone_send: bool,
}

/// What the desktop's "Send" on a held send came to.
#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct Confirmed {
    /// It went out.
    pub txid: Option<String>,
    /// Nothing happened: the wallet is locked. Ask for its passphrase and confirm again.
    pub need_passphrase: bool,
}

/// What a phone is asked to approve (v0.2.5): a payment from the desktop over the day's amount, or something that would
/// weaken "Approve sends on my phone" (turning it off, a higher amount, pairing another phone, a phone's first Face ID,
/// showing the recovery words).
#[derive(Clone, Debug, PartialEq)]
pub enum Approve {
    Send { sats: u64, address: String },
    /// Another payment (the credit tabs'), in words, and what it costs this wallet (None: not known).
    Action { text: String, sats: Option<u64> },
    Change { text: String },
    /// A change the recovery words made, due at unix second `due`: approving makes it now, declining cancels it.
    Scheduled { over: Option<u64>, due: u64 },
}

impl Approve {
    /// In a few words, for the desktop's waiting screen.
    pub fn text(&self) -> String {
        match self {
            Approve::Send { sats, address } => format!("Send {} ECX to {}", to_ecx(*sats), address),
            Approve::Action { text, .. } | Approve::Change { text } => text.clone(),
            Approve::Scheduled { over: None, .. } => {
                "Turn \"Approve sends on my phone\" off (your recovery words were used on the desktop)".into()
            }
            Approve::Scheduled { over: Some(b), .. } => format!(
                "Ask your phones only when the desktop pays more than {} ECX in a day (your recovery words were used on the desktop)",
                to_ecx(*b)
            ),
        }
    }
}

/// How "Approve sends on my phone" let a desktop payment through (`Phone::clear_desktop`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cleared {
    /// It is off.
    Off,
    /// Within the day's amount: counted at unix second `at`. `Phone::uncount` gives it back if the payment doesn't go.
    Counted { at: u64, sats: u64 },
    /// A phone approved it; it doesn't count.
    Approved,
}

struct Approval {
    challenge: [u8; 32],
    /// The phones with Face ID that may answer it.
    devices: Vec<String>,
    expires: u64,
    what: Approve,
    done: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

pub struct Phone {
    pub store: Store,
    d: SecretKey,
    pub d_pub: String,
    pub room: String,
    devices: Mutex<Devices>,
    config: Mutex<Config>,
    pairing: Mutex<Pairing>,
    /// Live sessions by relay channel: the session and the device it belongs to.
    chans: Mutex<HashMap<u64, (Session, String)>>,
    /// Channels with neither a session nor a waiting pair request, since when (IDLE_CHANNEL_SECS).
    idle: Mutex<HashMap<u64, u64>>,
    /// Each session's Face ID state, by channel; a new hello starts it afresh.
    auth: Mutex<HashMap<u64, Auth>>,
    held: Mutex<Vec<Held>>,
    cancelled: Mutex<Vec<Cancelled>>,
    /// Held sends' final replies, by device, until when they are kept (the held time): a phone that
    /// reconnects hears them once it may (code review N1).
    finals: Mutex<Vec<(String, u64, Value)>>,
    held_ttl: AtomicU64,
    /// Run by the background part with the app closed (`background.rs`): nobody can confirm a send,
    /// so one that would be held is refused instead.
    background: std::sync::atomic::AtomicBool,
    /// Daemon mode: what to call when a paired phone asks for anything (it starts the node), and whether the node
    /// is starting and can't answer yet (`background.rs`, light).
    waker: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    starting: std::sync::atomic::AtomicBool,
    /// Daemon mode: the last start failed; what the phone hears until it is tried again.
    wake_failure: Mutex<Option<String>>,
    /// The wallet passphrase while "Let my phone send while FreeBank is open" is on. Memory only:
    /// never written, logged or handed to the screen, and wiped when it is let go.
    pass: Mutex<Option<Zeroizing<String>>>,
    /// One unlock, send and lock at a time, so one send's lock never cuts into another's unlock.
    wallet_gate: tokio::sync::Mutex<()>,
    /// Keeps every `walletpassphrase` clear of the node's relock (`crate::wallet::RelockGuard`).
    /// Its own until the app hands it the one the screens use (`share_relock_guard`).
    relock: Mutex<Arc<crate::wallet::RelockGuard>>,
    status: Mutex<LinkStatus>,
    /// Approvals the desktop is waiting for, by id ("Approve sends on my phone").
    approvals: Mutex<HashMap<String, Approval>>,
    approve_secs: AtomicU64,
    /// Phones another phone let add their first Face ID while approvals are on, until when (security review M2); and
    /// the ones whose approval is being asked now.
    grants: Mutex<HashMap<String, u64>>,
    granting: Mutex<std::collections::HashSet<String>>,
    /// Phones whose ask was declined or not answered, until when they may ask again.
    grant_refused: Mutex<HashMap<String, u64>>,
    out: mpsc::UnboundedSender<Value>,
    /// Wakes the link: the relay URL changed, or it has (or no longer has) something to do.
    pub wake: tokio::sync::Notify,
    rpc: Arc<dyn Rpc>,
    events: Arc<dyn Events>,
    clock: Clock,
}

fn rand16() -> [u8; 16] {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// Characters a device name can't keep: invisible ones that can disguise it (bidirectional
/// formatting, zero-width, tags) and line separators. Control characters go too.
fn hidden(c: char) -> bool {
    matches!(c,
        // bidirectional formatting
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        // zero-width and invisible
        | '\u{00AD}' | '\u{034F}' | '\u{180E}' | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}'
        // tags, and line and paragraph separators
        | '\u{E0000}'..='\u{E007F}' | '\u{2028}' | '\u{2029}')
}

/// A device name as it is shown and stored (P6): control, bidirectional and zero-width characters
/// dropped, at most 40 characters, trimmed; "Phone" if nothing is left.
fn clean_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| !c.is_control() && !hidden(*c)).take(NAME_MAX).collect();
    let n = n.trim().to_string();
    if n.is_empty() {
        "Phone".into()
    } else {
        n
    }
}

/// What a phone's payment pays: the kind, the house for a note action, where it goes, how much (sats, or note units).
#[derive(Clone, Debug)]
pub struct Pending {
    pub kind: Kind,
    pub house: Option<u64>,
    pub address: String,
    pub sats: u64,
}

impl Pending {
    /// What it costs this wallet, as far as is known before it is paid: a note action's fee and carriers too (a send's
    /// fee is the node's to choose).
    fn cost(&self) -> u64 {
        match self.kind {
            Kind::Send => self.sats,
            _ => self.sats.saturating_add(NOTE_FEE_SATS + NOTE_CARRIER_SATS),
        }
    }

    /// What it counts against the desktop's day when the desktop confirms it: a send with an allowance for the fee the
    /// node will choose (0.001 ECX, more than it takes), so the day's total counts fees here too.
    fn desk_cost(&self) -> u64 {
        match self.kind {
            Kind::Send => self.sats.saturating_add(SEND_FEE_ALLOWANCE),
            _ => self.cost(),
        }
    }

    /// How a phone is asked to approve it when the desktop confirms it.
    fn approve(&self) -> Approve {
        let house = self.house.map(|h| format!("house #{h}")).unwrap_or_default();
        let ecx = to_ecx(self.sats);
        match self.kind {
            Kind::Send => Approve::Send { sats: self.sats, address: self.address.clone() },
            Kind::NoteSend => Approve::Action {
                text: format!("Send {ecx} ECX of {house}'s notes to {} (a phone's request)", self.address),
                sats: Some(self.cost()),
            },
            Kind::NoteRedeem => {
                Approve::Action { text: format!("Redeem {ecx} ECX of {house}'s notes (a phone's request)"), sats: Some(self.cost()) }
            }
            Kind::NoteDemand => {
                Approve::Action { text: format!("Demand {ecx} ECX of {house}'s notes (a phone's request)"), sats: Some(self.cost()) }
            }
        }
    }
}

/// A log entry names a note action and its house; a send's entry stays as before.
fn with_kind(entry: &mut Value, kind: Kind, house: Option<u64>) {
    if kind != Kind::Send {
        entry["kind"] = json!(kind);
        entry["house"] = json!(house);
    }
}

/// A redeem or a demand takes one holder's coins summing exactly to the amount (freebankd: "... sum exactly ..."): say
/// what to do, as the desktop's Notes tab does.
fn consolidate_hint(kind: Kind, e: String) -> String {
    if matches!(kind, Kind::NoteRedeem | Kind::NoteDemand) && e.contains("sum exactly") {
        format!(
            "{e} Your notes of this house may sit at more than one of your addresses: gather them first on your \
             desktop (Notes, Send with the address left empty), then try again."
        )
    } else {
        e
    }
}

/// A house's state by its letter (listmynotes' house_status), as the desktop's Notes tab names it.
fn state_name(c: char) -> &'static str {
    match c {
        'o' => "Open",
        's' => "Stressed",
        'd' => "Suspended",
        'i' => "Insolvent",
        'w' => "Wound down",
        _ => "Unknown",
    }
}

/// A house's type (node v0.2.19): "open", "members" (its notes go only to its members) or "redeem" (members only, and
/// notes pass only back to the house or to the holder's own key). Older nodes don't say: open.
fn house_type(h: Option<&Value>) -> &'static str {
    match h.and_then(|h| h["type"].as_str()) {
        Some("members") => "members",
        Some("redeem") => "redeem",
        _ => "open",
    }
}

/// One house of the directory (listhouses) as the phone shows it: its name, state and soundness.
fn house_view(h: &Value) -> Value {
    // listhouses names the state in full ("open", "stressed", "deferred", "insolvent", "wounddown").
    let state = match h["effective_status"].as_str().unwrap_or("") {
        "deferred" => 'd',
        "wounddown" => 'w',
        s => s.chars().next().unwrap_or('?'),
    };
    json!({
        "house": h["id"],
        "name": h["classid"],
        "state": state.to_string(),
        "state_name": state_name(state),
        "ratio_bps": h["attestedratiobps"],
        "outstanding": to_ecx(h["mintedunits"].as_u64().unwrap_or(0)),
        "reserves": h["lastattestreserves"],
        "attested_at": h["lastattestheight"],
        "rate_bps": h["defer_interest_bps"],
        "type": house_type(Some(h)),
    })
}

/// A FreeBank legacy address: 'X', base58, 26 to 35 characters. The node checks the rest.
fn plausible_address(a: &str) -> bool {
    const B58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    a.starts_with('X') && (26..=35).contains(&a.len()) && a.chars().all(|c| B58.contains(c))
}

impl Phone {
    /// Load (or make) the desktop key, the device list and the relay setting from `app_dir/phone`.
    /// Sends still held when the app last stopped are cancelled here, never paid.
    /// Frames for the relay come out of the returned receiver; `link::run` takes it.
    pub fn new(
        app_dir: &Path,
        rpc: Arc<dyn Rpc>,
        events: Arc<dyn Events>,
        clock: Clock,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Value>), String> {
        let store = Store::new(app_dir);
        let d = store.desktop_key()?;
        let d_pub = crypto::pub_b64u(&d.public_key());
        let room = crypto::room(&d.public_key());
        let mut devices = store.load_devices();
        // Names stored by an earlier version are cleaned the same way before they are shown.
        for d in devices.devices.iter_mut() {
            d.name = clean_name(&d.name);
        }
        let config = store.load_config();
        let saved = store.load_held();
        let (out, rx) = mpsc::unbounded_channel();
        let phone = Arc::new(Self {
            store,
            d,
            d_pub,
            room,
            devices: Mutex::new(devices),
            config: Mutex::new(config),
            pairing: Mutex::default(),
            chans: Mutex::default(),
            idle: Mutex::default(),
            auth: Mutex::default(),
            held: Mutex::default(),
            cancelled: Mutex::new(saved.cancelled),
            finals: Mutex::default(),
            held_ttl: AtomicU64::new(HELD_TTL_SECS),
            background: std::sync::atomic::AtomicBool::new(false),
            waker: Mutex::new(None),
            starting: std::sync::atomic::AtomicBool::new(false),
            wake_failure: Mutex::new(None),
            pass: Mutex::new(None),
            wallet_gate: tokio::sync::Mutex::new(()),
            relock: Mutex::new(Arc::default()),
            status: Mutex::new(LinkStatus { state: "off".into(), detail: String::new() }),
            approvals: Mutex::default(),
            approve_secs: AtomicU64::new(APPROVE_SECS),
            grants: Mutex::default(),
            granting: Mutex::default(),
            grant_refused: Mutex::default(),
            out,
            wake: tokio::sync::Notify::new(),
            rpc,
            events,
            clock,
        });
        phone.cancel_after_restart(saved.held);
        phone.post_scheduled();
        Ok((phone, rx))
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn today(&self) -> u64 {
        store::day_of(self.now())
    }

    fn held_ttl(&self) -> u64 {
        self.held_ttl.load(Ordering::SeqCst)
    }

    /// How long held sends wait: 10 minutes, shorter only for tests (at least 1 second).
    #[cfg(test)]
    pub fn set_held_ttl(&self, secs: u64) {
        self.held_ttl.store(secs.clamp(1, HELD_TTL_SECS), Ordering::SeqCst);
    }

    // ----- the link's view -------------------------------------------------------------------

    pub fn relay_url(&self) -> String {
        self.config.lock().unwrap().relay_url.clone()
    }

    /// The first frame to the relay.
    pub fn host_frame(&self) -> Value {
        json!({"t": "host", "room": self.room, "d": self.d_pub})
    }

    /// The answer to the relay's challenge: proof that this desktop holds D.
    pub fn proof_frame(&self, challenge_b64u: &str) -> Result<Value, String> {
        let n = crypto::unb64u(challenge_b64u)?;
        if n.len() != 32 {
            return Err("the relay's challenge isn't 32 bytes".into());
        }
        Ok(json!({"t": "proof", "sig": crypto::b64u(&crypto::proof(&self.d, &n))}))
    }

    /// Whether the link should be up: a phone is paired, or a pairing is open.
    pub fn wanted(&self) -> bool {
        if !self.devices.lock().unwrap().devices.is_empty() {
            return true;
        }
        let p = self.pairing.lock().unwrap();
        !p.asks.is_empty()
            || p.code.as_ref().is_some_and(|c| c.state == CodeState::Live && self.now() < c.expires)
    }

    pub fn set_status(&self, state: &str, detail: &str) {
        let mut s = self.status.lock().unwrap();
        if s.state != state || s.detail != detail {
            *s = LinkStatus { state: state.into(), detail: detail.into() };
            drop(s);
            crate::activity::note(&format!("phone link: {state}: {detail}"));
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    pub fn status(&self) -> LinkStatus {
        self.status.lock().unwrap().clone()
    }

    /// The relay link dropped: every channel with it, and the pair requests that came on them.
    pub fn clear_channels(&self) {
        self.chans.lock().unwrap().clear();
        self.auth.lock().unwrap().clear();
        self.idle.lock().unwrap().clear();
        let mut p = self.pairing.lock().unwrap();
        if !p.asks.is_empty() {
            p.asks.clear();
            drop(p);
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    // ----- frames ----------------------------------------------------------------------------

    fn send_clear(&self, ch: u64, d: Value) {
        let _ = self.out.send(json!({"ch": ch, "d": d}));
    }

    /// Seal `msg` for the session on `ch` and queue it. The lock covers the counter and the
    /// queue, so frames leave in counter order.
    fn send_sealed(&self, ch: u64, msg: &Value) -> bool {
        let mut chans = self.chans.lock().unwrap();
        let Some((s, _)) = chans.get_mut(&ch) else { return false };
        let ct = s.tx.seal(msg.to_string().as_bytes());
        let _ = self.out.send(json!({"ch": ch, "d": {"t": "m", "ct": crypto::b64u(&ct)}}));
        true
    }

    /// Every channel with a live session for `device`.
    fn channels_of(&self, device: &str) -> Vec<u64> {
        self.chans.lock().unwrap().iter().filter(|(_, (_, d))| d == device).map(|(c, _)| *c).collect()
    }

    /// One frame from the relay (anything after the host handshake).
    pub fn handle_frame(self: &Arc<Self>, f: Value) {
        if f["t"] == "closed" {
            if let Some(ch) = f["ch"].as_u64() {
                self.chans.lock().unwrap().remove(&ch);
                self.auth.lock().unwrap().remove(&ch);
                self.idle.lock().unwrap().remove(&ch);
                let mut p = self.pairing.lock().unwrap();
                let before = p.asks.len();
                p.asks.retain(|a| a.ch != ch);
                if p.asks.len() != before {
                    drop(p);
                    self.events.emit(EV_CHANGED, json!({}));
                }
            }
            return;
        }
        let (Some(ch), Some(d)) = (f["ch"].as_u64(), f.get("d")) else { return };
        match d["t"].as_str() {
            Some("pair") => self.on_pair(ch, d),
            Some("hello") => self.on_hello(ch, d),
            Some("m") => self.on_message(ch, d),
            _ => {}
        }
        self.note_idle(ch);
    }

    /// Start (or keep) the idle clock of a channel with neither a session nor a waiting pair
    /// request; stop it for one that has either.
    fn note_idle(&self, ch: u64) {
        let active = self.chans.lock().unwrap().contains_key(&ch)
            || self.pairing.lock().unwrap().asks.iter().any(|a| a.ch == ch);
        let mut idle = self.idle.lock().unwrap();
        if active {
            idle.remove(&ch);
        } else {
            idle.entry(ch).or_insert(self.now());
        }
    }

    /// Ask the relay to drop a phone channel.
    fn close_channel(&self, ch: u64) {
        self.idle.lock().unwrap().remove(&ch);
        let _ = self.out.send(json!({"t": "close", "ch": ch}));
    }

    // ----- pairing ---------------------------------------------------------------------------

    /// Open a pairing: a fresh one-use code for 5 minutes (any earlier code stops working).
    /// Returns the URL the QR code shows and when it expires.
    pub fn pair_start(&self) -> Result<(String, u64), String> {
        let relay = self.relay_url();
        let u = url::Url::parse(&relay).map_err(|_| format!("The relay address {relay} isn't a URL."))?;
        let page_scheme = match u.scheme() {
            "wss" => "https",
            "ws" => "http",
            _ => return Err("The relay address must start with ws:// or wss://.".into()),
        };
        let host = u.host_str().ok_or("The relay address has no host.")?;
        let host = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        let c = rand16();
        let expires = self.now() + PAIR_TTL_SECS;
        let link = json!({"v": 1, "relay": relay, "room": self.room, "d": self.d_pub, "c": crypto::b64u(&c)});
        let url = format!("{page_scheme}://{host}/#pair={}", crypto::b64u(link.to_string().as_bytes()));
        let mut p = self.pairing.lock().unwrap();
        p.code = Some(Code { c, expires, state: CodeState::Live });
        // Requests for an earlier code are refused: that code stops working.
        let old: Vec<Ask> = p.asks.drain(..).collect();
        drop(p);
        self.refuse_asks(&old);
        self.wake.notify_one();
        Ok((url, expires))
    }

    /// Tell these phones no, and let their channels go idle.
    fn refuse_asks(&self, asks: &[Ask]) {
        for a in asks {
            self.send_clear(a.ch, json!({"t": "pair-refused"}));
            self.note_idle(a.ch);
        }
        if !asks.is_empty() {
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    /// Open a pairing with a known code (the test vectors').
    #[cfg(test)]
    fn test_code(&self, c: [u8; 16]) {
        let mut p = self.pairing.lock().unwrap();
        p.code = Some(Code { c, expires: self.now() + PAIR_TTL_SECS, state: CodeState::Live });
        p.asks.clear();
    }

    fn on_pair(&self, ch: u64, d: &Value) {
        let refuse = || self.send_clear(ch, json!({"t": "pair-refused"}));
        let mut p = self.pairing.lock().unwrap();
        let Some(code) = p.code.as_ref() else { return refuse() };
        if code.state != CodeState::Live || self.now() >= code.expires {
            return refuse();
        }
        let c = code.c;
        let opened = (|| -> Result<(p256::PublicKey, p256::PublicKey, String), String> {
            let e = crypto::parse_pub(d["e"].as_str().ok_or("e")?)?;
            let n: [u8; 12] = crypto::unb64u(d["n"].as_str().ok_or("n")?)?
                .try_into()
                .map_err(|_| "nonce must be 12 bytes")?;
            let ct = crypto::unb64u(d["ct"].as_str().ok_or("ct")?)?;
            let k = crypto::pair_key(&self.d, &e, &c);
            let pt: Value = serde_json::from_slice(&crypto::open(&k, &n, &ct)?).map_err(|_| "not JSON")?;
            let phone = crypto::parse_pub(pt["p"].as_str().ok_or("p")?)?;
            Ok((e, phone, clean_name(pt["name"].as_str().unwrap_or(""))))
        })();
        // A frame that doesn't open with the live code is refused, and changes nothing.
        let Ok((e, phone, name)) = opened else { return refuse() };
        // One waiting request per channel (a later one replaces it), and a few at most.
        p.asks.retain(|a| a.ch != ch);
        if p.asks.len() >= MAX_ASKS {
            return refuse();
        }
        let ask = Ask {
            ch,
            p_pub: crypto::pub_b64u(&phone),
            id: hex::encode(&rand16()[..8]),
            device: Device::id_for(&phone),
            name,
            code: crypto::pair_code(&self.d.public_key(), &phone, &e, &c),
        };
        p.asks.push(ask.clone());
        drop(p);
        crate::activity::note("phone pairing: a phone asks to pair");
        self.events.emit(EV_PAIR, serde_json::to_value(&ask).unwrap());
    }

    /// The pair requests waiting for an answer, oldest first.
    pub fn pair_pending(&self) -> Vec<Ask> {
        self.pairing.lock().unwrap().asks.clone()
    }

    /// "Allow this phone?" answered for one request. Allowing it spends the pairing code and
    /// refuses every other request for it; denying refuses only this one, and the code stays
    /// live for the phone the owner is holding.
    pub fn pair_answer(&self, id: &str, allow: bool) -> Result<(), String> {
        let mut p = self.pairing.lock().unwrap();
        let i = p.asks.iter().position(|a| a.id == id).ok_or("That phone is no longer waiting.")?;
        crate::activity::note(if allow { "phone pairing: allowed" } else { "phone pairing: denied" });
        let expired = p.code.as_ref().is_none_or(|c| self.now() >= c.expires);
        if !allow || expired {
            let ask = p.asks.remove(i);
            let rest: Vec<Ask> = if expired { p.asks.drain(..).collect() } else { vec![] };
            drop(p);
            self.refuse_asks(&[vec![ask], rest].concat());
            return if allow { Err("The pairing code has expired. Make a new one.".into()) } else { Ok(()) };
        }
        let ask = p.asks.remove(i);
        let rest: Vec<Ask> = p.asks.drain(..).collect();
        if let Some(c) = p.code.as_mut() {
            c.state = CodeState::Used;
        }
        drop(p);
        self.refuse_asks(&rest);
        let mut devs = self.devices.lock().unwrap();
        devs.devices.retain(|d| d.p_pub != ask.p_pub);
        devs.devices.push(Device {
            id: ask.device.clone(),
            name: ask.name.clone(),
            p_pub: ask.p_pub.clone(),
            added: self.now(),
            last_seen: None,
            limit_sats: store::DEFAULT_LIMIT_SATS,
            spent_day: 0,
            spent_sats: 0,
            passkey: None,
        });
        let saved = self.store.save_devices(&devs);
        drop(devs);
        if let Err(e) = saved {
            self.send_clear(ask.ch, json!({"t": "pair-refused"}));
            self.note_idle(ask.ch);
            return Err(e);
        }
        self.send_clear(ask.ch, json!({"t": "paired"}));
        // The phone comes back on a new channel for its first session; this one should go.
        self.note_idle(ask.ch);
        self.events.emit(EV_CHANGED, json!({}));
        Ok(())
    }

    // ----- devices ---------------------------------------------------------------------------

    pub fn devices(&self) -> Vec<Device> {
        self.devices.lock().unwrap().devices.clone()
    }

    pub fn online(&self, device: &str) -> bool {
        !self.channels_of(device).is_empty()
    }

    /// Forget a phone: its live sessions are cut (it is told `denied`) and its held sends dropped.
    /// Revoking the last phone also turns phone sends off.
    pub fn revoke(&self, id: &str) -> Result<(), String> {
        let mut devs = self.devices.lock().unwrap();
        let before = devs.devices.len();
        devs.devices.retain(|d| d.id != id);
        if devs.devices.len() == before {
            return Err("No such phone.".into());
        }
        let none_left = devs.devices.is_empty();
        let saved = self.store.save_devices(&devs);
        drop(devs);
        for ch in self.channels_of(id) {
            self.chans.lock().unwrap().remove(&ch);
            self.auth.lock().unwrap().remove(&ch);
            self.send_clear(ch, json!({"t": "denied"}));
            // And the relay drops its connection.
            self.close_channel(ch);
        }
        let dropped: Vec<Held> = {
            let mut h = self.held.lock().unwrap();
            let (gone, keep) = h.drain(..).partition(|x| x.device == id && !x.busy);
            *h = keep;
            gone
        };
        self.cancelled.lock().unwrap().retain(|c| c.device != id);
        self.save_held();
        for h in dropped {
            if let Some(c) = h.cleared {
                self.uncount(c);
            }
            self.log_held(&h, "declined", json!("phone revoked"));
        }
        // With no phone left, "Let my phone send while FreeBank is open" serves no one, and Settings
        // no longer shows its switch: the passphrase goes, so a phone paired later starts with phone
        // sends off until they are turned on again.
        if none_left {
            self.forget_passphrase();
            // No phone to wake the node for: the login item goes too (Settings hides its switch then).
            if let Some(app_dir) = self.store.dir.parent() {
                let _ = login_item::set(app_dir, false);
            }
        }
        self.events.emit(EV_CHANGED, json!({}));
        self.wake.notify_one();
        saved
    }

    /// A phone's daily limit. While "Approve sends on my phone" is on, a higher one takes a phone's approval: within
    /// it, that phone pays without the desktop (security re-review H1).
    pub async fn set_limit(&self, id: &str, ecx: f64) -> Result<(), String> {
        let sats = to_sats(ecx)?;
        let (now, name) = {
            let devs = self.devices.lock().unwrap();
            let d = devs.devices.iter().find(|d| d.id == id).ok_or("No such phone.")?;
            (d.limit_sats, d.name.clone())
        };
        if sats > now && self.approve_over().is_some() {
            let text = format!("Let \"{name}\" pay up to {} ECX a day without asking your desktop (now {} ECX)", to_ecx(sats), to_ecx(now));
            self.request_approval(Approve::Change { text }).await?;
        }
        let mut devs = self.devices.lock().unwrap();
        devs.get_mut(id).ok_or("No such phone.")?.limit_sats = sats;
        self.store.save_devices(&devs)
    }

    pub fn set_relay(&self, url: &str) -> Result<(), String> {
        let url = url.trim();
        let u = url::Url::parse(url).map_err(|_| "That isn't a URL.".to_string())?;
        if !matches!(u.scheme(), "ws" | "wss") || u.host_str().is_none() {
            return Err("The relay address must look like wss://host/ws (or ws:// for testing).".into());
        }
        let mut c = self.config.lock().unwrap();
        c.relay_url = url.to_string();
        let r = self.store.save_config(&c);
        drop(c);
        // Sessions and pairings belong to the old relay's room.
        self.wake.notify_one();
        r
    }

    // ----- session handshake -----------------------------------------------------------------

    fn on_hello(&self, ch: u64, d: &Value) {
        let p_str = d["p"].as_str().unwrap_or("");
        let dev = self.devices.lock().unwrap().by_pub(p_str).map(|x| x.id.clone());
        let keys = (|| -> Result<_, String> {
            let p = crypto::parse_pub(p_str)?;
            let ep = crypto::parse_pub(d["e"].as_str().ok_or("e")?)?;
            Ok((p, ep))
        })();
        let (Some(dev), Ok((p, ep))) = (dev, keys) else {
            self.chans.lock().unwrap().remove(&ch);
            self.auth.lock().unwrap().remove(&ch);
            return self.send_clear(ch, json!({"t": "denied"}));
        };
        let ed = crypto::random_secret();
        self.accept_hello(ch, &dev, &p, &ep, &ed);
    }

    /// Split out so the tests can fix eD.
    fn accept_hello(&self, ch: u64, dev: &str, p: &p256::PublicKey, ep: &p256::PublicKey, ed: &SecretKey) {
        let (k_pd, k_dp) = crypto::desktop_session_keys(&self.d, ed, p, ep);
        self.chans.lock().unwrap().insert(ch, (Session::desktop(k_pd, k_dp), dev.to_string()));
        let gen = rand::RngCore::next_u64(&mut rand::rngs::OsRng);
        self.auth.lock().unwrap().insert(ch, Auth { gen, ..Default::default() });
        // Revoked while this hello was on its way (security review L2): no session for it.
        if !self.devices.lock().unwrap().devices.iter().any(|d| d.id == dev) {
            self.chans.lock().unwrap().remove(&ch);
            self.auth.lock().unwrap().remove(&ch);
            return self.send_clear(ch, json!({"t": "denied"}));
        }
        {
            let mut devs = self.devices.lock().unwrap();
            if let Some(x) = devs.get_mut(dev) {
                x.last_seen = Some(self.now());
            }
            let _ = self.store.save_devices(&devs);
        }
        self.send_clear(ch, json!({"t": "hello-ok", "e": crypto::pub_b64u(&ed.public_key())}));
        // A phone with Face ID hears them once the session proves it (auth_open), not before (I4); so do the approvals
        // waiting for it (only phones with Face ID approve).
        if self.passkey_of(dev).is_none() {
            self.tell_restart(ch, dev);
            self.replay_finals(ch, dev);
        }
        self.events.emit(EV_CHANGED, json!({}));
    }

    /// A new session hears the final err of each of its phone's held sends that a restart cancelled.
    fn tell_restart(&self, ch: u64, dev: &str) {
        let (now, ttl) = (self.now(), self.held_ttl());
        let notes: Vec<Cancelled> = self
            .cancelled
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.device == dev && now < c.at.saturating_add(ttl))
            .cloned()
            .collect();
        for c in notes {
            self.send_sealed(ch, &json!({"id": c.req_id, "pending": c.confirm, "err": ERR_RESTARTED}));
        }
    }

    fn on_message(self: &Arc<Self>, ch: u64, d: &Value) {
        let opened = {
            let mut chans = self.chans.lock().unwrap();
            let Some((s, dev)) = chans.get_mut(&ch) else {
                drop(chans);
                return self.send_clear(ch, json!({"t": "closed"}));
            };
            let dev = dev.clone();
            match crypto::unb64u(d["ct"].as_str().unwrap_or("")).and_then(|ct| s.rx.open(&ct)) {
                Ok(pt) => Ok((pt, dev)),
                Err(e) => {
                    chans.remove(&ch);
                    self.auth.lock().unwrap().remove(&ch);
                    Err(e)
                }
            }
        };
        // Out of order or tampered: the session is over; the phone starts again with hello.
        let Ok((pt, dev)) = opened else { return self.send_clear(ch, json!({"t": "closed"})) };
        let Ok(req) = serde_json::from_slice::<Value>(&pt) else { return };
        let me = self.clone();
        tokio::spawn(async move {
            let id = req["id"].clone();
            let reply = match me.serve(ch, &dev, &req).await {
                Ok(v) => json!({"id": id, "ok": v}),
                Err(e) if e == ERR_AUTH_NEEDED => json!({"id": id, "err": e, "auth": "open"}),
                Err(e) if e == ERR_STARTING => json!({"id": id, "err": e, "starting": true}),
                Err(e) => json!({"id": id, "err": e}),
            };
            me.send_sealed(ch, &reply);
        });
    }

    // ----- the narrow door -------------------------------------------------------------------

    async fn serve(self: &Arc<Self>, ch: u64, dev: &str, req: &Value) -> Result<Value, String> {
        let a = &req["a"];
        let m = req["m"].as_str().unwrap_or("");
        // A request that was on its way when the phone was revoked (security review L2).
        if !self.devices.lock().unwrap().devices.iter().any(|d| d.id == dev) {
            return Err("This phone was removed on the desktop.".into());
        }
        // Face ID first, for a phone that added a passkey (PROTOCOL.md, "Face ID: passkeys").
        match m {
            "auth-start" => {
                if a["for"] == "add" {
                    self.first_passkey_allowed(dev)?;
                }
                return self.auth_start(ch, dev, a);
            }
            "auth" => return self.auth_open(ch, dev, a),
            // Its own Face ID (an assertion over the approval's challenge), or a decline: not behind the session's.
            "approve" => return self.approve_answer(ch, dev, a),
            "passkey-add" => {
                self.first_passkey_allowed(dev)?;
                let r = self.passkey_add(ch, dev, a);
                if r.is_ok() {
                    self.grants.lock().unwrap().remove(dev);
                }
                return r;
            }
            _ if !self.verified(ch, dev) => return Err(ERR_AUTH_NEEDED.into()),
            _ => {}
        }
        // A paired phone, past its session and Face ID: in daemon mode this starts the node.
        self.wake();
        if matches!(
            m,
            "balance" | "history" | "receive" | "status" | "send" | "notes" | "houses" | "note-send" | "note-redeem"
                | "note-demand"
        ) {
            if self.starting.load(Ordering::SeqCst) {
                return Err(ERR_STARTING.into());
            }
            if let Some(why) = self.wake_failure.lock().unwrap().clone() {
                return Err(why);
            }
        }
        match m {
            // Changing Face ID takes Face ID again, over its own challenge (M1): a session proved
            // earlier doesn't do.
            "passkey-set" => {
                let sends = a["sends"].as_bool().ok_or("sends is true or false")?;
                self.check_assertion(ch, dev, "change", &a["auth"], None)?;
                self.with_passkey(dev, |k| k.sends = sends)?;
                Ok(json!({}))
            }
            "passkey-remove" => {
                self.check_assertion(ch, dev, "change", &a["auth"], None)?;
                self.with_device(dev, |d| d.passkey = None)?;
                Ok(json!({}))
            }
            "balance" => {
                let confirmed = self.rpc.call("getbalance", vec![]).await?.as_f64().ok_or("bad balance")?;
                let pending = match self.rpc.call("getunconfirmedbalance", vec![]).await {
                    Ok(v) => v.as_f64().unwrap_or(0.0),
                    Err(_) => 0.0,
                };
                Ok(json!({"confirmed": confirmed, "pending": pending}))
            }
            "history" => {
                let count = a["count"].as_u64().unwrap_or(20).clamp(1, HISTORY_MAX);
                let r = self.rpc.call("listtransactions", vec![json!("*"), json!(count)]).await?;
                // The node lists oldest first; the phone shows newest first.
                let list: Vec<Value> = r
                    .as_array()
                    .map(|v| v.as_slice())
                    .unwrap_or(&[])
                    .iter()
                    .rev()
                    .map(|t| {
                        json!({
                            "txid": t["txid"], "category": t["category"], "amount": t["amount"],
                            "confirmations": t["confirmations"], "time": t["time"], "address": t["address"],
                        })
                    })
                    .collect();
                Ok(Value::Array(list))
            }
            "receive" => {
                // A new wallet gives out no address until it has its passphrase and recovery words
                // (the desktop's address screens wait the same way, recovery::addresses_held).
                let info = self.rpc.call("getwalletinfo", vec![]).await?;
                if self.store.dir.parent().is_some_and(|app_dir| crate::recovery::addresses_held(&info, app_dir)) {
                    return Err(crate::recovery::WALLET_NOT_SET_UP.into());
                }
                let r = self.rpc.call("getnewaddress", vec![json!(""), json!("legacy")]).await?;
                Ok(json!({"address": r.as_str().ok_or("bad address")?}))
            }
            "status" => {
                let r = self.rpc.call("getblockchaininfo", vec![]).await?;
                let synced = match r["initialblockdownload"].as_bool() {
                    Some(ibd) => !ibd,
                    None => r["blocks"] == r["headers"],
                };
                let left = self.devices.lock().unwrap().devices.iter().find(|d| d.id == dev).map(|d| d.left_on(self.today()));
                let key = self.passkey_of(dev);
                Ok(json!({
                    "blocks": r["blocks"], "synced": synced, "limit_left": to_ecx(left.unwrap_or(0)),
                    "face_id": key.is_some(), "face_id_sends": key.is_some_and(|k| k.sends),
                    // v0.2.5: this desktop answers notes, houses and the note actions.
                    "credit": true,
                    // Turning Face ID on here waits for another phone's approval first (security review M2).
                    "add_asks": self.first_passkey_asks(dev),
                }))
            }
            "notes" => self.notes().await,
            "houses" => {
                let r = self.rpc.call("listhouses", vec![]).await?;
                Ok(Value::Array(r.as_array().map(|v| v.as_slice()).unwrap_or(&[]).iter().map(house_view).collect()))
            }
            "send" | "note-send" | "note-redeem" | "note-demand" => {
                // Signed by this phone's own Face ID: the desktop's approval needn't ask for it again (security review M1).
                let face_id = self.passkey_of(dev).is_some_and(|k| k.sends);
                if face_id {
                    self.check_assertion(ch, dev, "send", &a["auth"], None)?;
                }
                let kind = Kind::from_method(m).expect("one of the four");
                let r = if kind == Kind::Send {
                    self.send(dev, req, face_id).await
                } else {
                    self.note_action(dev, req, kind, face_id).await
                };
                // A held send remembers the session that asked (N1).
                if let Some(confirm) = r.as_ref().ok().and_then(|v| v["pending"].as_str()) {
                    if let Some(h) = self.held.lock().unwrap().iter_mut().find(|h| h.confirm == confirm) {
                        h.ch = ch;
                    }
                }
                r
            }
            _ => Err("unknown method".into()),
        }
    }

    // ----- Face ID: passkeys -----------------------------------------------------------------

    fn passkey_of(&self, dev: &str) -> Option<store::Passkey> {
        self.devices.lock().unwrap().devices.iter().find(|d| d.id == dev).and_then(|d| d.passkey.clone())
    }

    /// Change a device and save the list.
    fn with_device(&self, dev: &str, f: impl FnOnce(&mut Device)) -> Result<(), String> {
        let mut devs = self.devices.lock().unwrap();
        let d = devs.get_mut(dev).ok_or("This phone was removed on the desktop.")?;
        f(d);
        self.store.save_devices(&devs)?;
        drop(devs);
        self.events.emit(EV_CHANGED, json!({}));
        Ok(())
    }

    fn with_passkey(&self, dev: &str, f: impl FnOnce(&mut store::Passkey)) -> Result<(), String> {
        let mut found = false;
        self.with_device(dev, |d| {
            if let Some(k) = d.passkey.as_mut() {
                found = true;
                f(k);
            }
        })?;
        if found { Ok(()) } else { Err("This phone has no Face ID set up.".into()) }
    }

    /// The session may use the narrow door: its phone has no passkey, or the session proved it lately
    /// (within `VERIFIED_IDLE_SECS` of its last request and `VERIFIED_MAX_SECS` of the proof). Each
    /// request that passes counts as use.
    fn verified(&self, ch: u64, dev: &str) -> bool {
        let Some(has_key) = self.device_has_passkey(dev) else { return false };
        if !has_key {
            return true;
        }
        let now = self.now();
        let mut auth = self.auth.lock().unwrap();
        let Some(s) = auth.get_mut(&ch) else { return false };
        match s.verified {
            Some((at, used)) if now <= used + VERIFIED_IDLE_SECS && now <= at + VERIFIED_MAX_SECS => {
                s.verified = Some((at, now));
                true
            }
            _ => {
                s.verified = None;
                false
            }
        }
    }

    /// Whether the device has a passkey; None if it is gone.
    fn device_has_passkey(&self, dev: &str) -> Option<bool> {
        self.devices.lock().unwrap().devices.iter().find(|d| d.id == dev).map(|d| d.passkey.is_some())
    }

    /// May this session hear its phone's news (held sends' outcomes)? Without a passkey, yes; with
    /// one, only once it proved it and the proof hasn't lapsed (I4). Reading doesn't count as use.
    fn may_hear(&self, ch: u64, dev: &str) -> bool {
        match self.device_has_passkey(dev) {
            None => false,
            Some(false) => true,
            Some(true) => {
                let now = self.now();
                self.auth.lock().unwrap().get(&ch).and_then(|s| s.verified).is_some_and(|(at, used)| {
                    now <= used + VERIFIED_IDLE_SECS && now <= at + VERIFIED_MAX_SECS
                })
            }
        }
    }

    /// The session proved the passkey just now: if it is still the session whose challenge was used.
    fn mark_verified(&self, ch: u64, gen: u64) {
        let now = self.now();
        if let Some(s) = self.auth.lock().unwrap().get_mut(&ch).filter(|s| s.gen == gen) {
            s.verified = Some((now, now));
        }
    }

    /// `auth-start`: a fresh challenge for this session and purpose.
    fn auth_start(&self, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        let purpose = a["for"].as_str().unwrap_or("");
        if !matches!(purpose, "open" | "send" | "add" | "change") {
            return Err("for is open, send, add or change".into());
        }
        let mut c = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut c);
        let mut auth = self.auth.lock().unwrap();
        let s = auth.get_mut(&ch).ok_or("no session")?;
        s.challenges.insert(purpose.to_string(), (c, self.now() + CHALLENGE_SECS));
        drop(auth);
        let mut reply = json!({"challenge": crypto::b64u(&c)});
        if let Some(k) = self.passkey_of(dev) {
            reply["cred"] = json!(k.cred);
        }
        Ok(reply)
    }

    /// Check an assertion over this session's live challenge for `purpose`, which it uses up either
    /// way. With the phone's stored passkey (its counter checked and updated under one lock, I3), or
    /// `new_pk` (a passkey being added). The new counter, and the session generation it belongs to.
    fn check_assertion(&self, ch: u64, dev: &str, purpose: &str, x: &Value, new_pk: Option<&[u8]>) -> Result<(u32, u64), String> {
        let live = {
            let mut auth = self.auth.lock().unwrap();
            auth.get_mut(&ch).map(|s| (s.challenges.remove(purpose), s.gen))
        };
        let Some((Some((challenge, until)), gen)) = live else { return Err(ERR_AUTH_FAILED.into()) };
        if self.now() > until {
            return Err(ERR_AUTH_FAILED.into());
        }
        let (origin, rp_id) = webauthn::origin_for(&self.relay_url()).ok_or(ERR_AUTH_FAILED)?;
        let part = |k: &str| crypto::unb64u(x[k].as_str().unwrap_or("")).map_err(|_| ERR_AUTH_FAILED.to_string());
        let (ad, cdj, sig) = (part("ad")?, part("cdj")?, part("sig")?);
        let check = |pk: &[u8], stored: u32| {
            webauthn::verify(pk, &rp_id, &origin, &challenge, &ad, &cdj, &sig, stored).map_err(|_| ERR_AUTH_FAILED.to_string())
        };
        if let Some(pk) = new_pk {
            return check(pk, 0).map(|c| (c, gen));
        }
        self.check_stored(dev, &challenge, x).map(|c| (c, gen))
    }

    /// An assertion over `challenge` by `dev`'s stored passkey; its signature counter is kept.
    fn check_stored(&self, dev: &str, challenge: &[u8; 32], x: &Value) -> Result<u32, String> {
        let (origin, rp_id) = webauthn::origin_for(&self.relay_url()).ok_or(ERR_AUTH_FAILED)?;
        let part = |k: &str| crypto::unb64u(x[k].as_str().unwrap_or("")).map_err(|_| ERR_AUTH_FAILED.to_string());
        let (ad, cdj, sig) = (part("ad")?, part("cdj")?, part("sig")?);
        let mut devs = self.devices.lock().unwrap();
        let key = devs.get_mut(dev).and_then(|d| d.passkey.as_mut()).ok_or("This phone has no Face ID set up.")?;
        let count = webauthn::verify(&crypto::unb64u(&key.pk)?, &rp_id, &origin, challenge, &ad, &cdj, &sig, key.count)
            .map_err(|_| ERR_AUTH_FAILED.to_string())?;
        if count != key.count {
            key.count = count;
            self.store.save_devices(&devs)?;
        }
        Ok(count)
    }

    /// `auth`: the session proves the phone's passkey.
    fn auth_open(&self, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        let (_, gen) = self.check_assertion(ch, dev, "open", a, None)?;
        self.mark_verified(ch, gen);
        // The restart notices and held sends' outcomes it didn't hear before proving the passkey (I4, N1), and the
        // approvals waiting for it.
        self.tell_restart(ch, dev);
        self.replay_finals(ch, dev);
        self.tell_approvals(ch, dev);
        Ok(json!({}))
    }

    /// `passkey-add`: keep a new passkey once an assertion shows it works. Replacing one takes the old
    /// one too, now: an assertion over a "change" challenge in `a.auth` (M1).
    fn passkey_add(&self, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        if !self.verified(ch, dev) {
            return Err(ERR_AUTH_NEEDED.into());
        }
        if self.passkey_of(dev).is_some() {
            self.check_assertion(ch, dev, "change", &a["auth"], None)?;
        }
        let pk = crypto::unb64u(a["pk"].as_str().unwrap_or("")).map_err(|_| "bad key")?;
        if pk.len() != 65 || p256::ecdsa::VerifyingKey::from_sec1_bytes(&pk).is_err() {
            return Err("That isn't a P-256 passkey.".into());
        }
        let cred = a["cred"].as_str().unwrap_or("");
        if !(1..=1023).contains(&crypto::unb64u(cred).map(|c| c.len()).unwrap_or(0)) {
            return Err("bad credential id".into());
        }
        let sends = a["sends"].as_bool().unwrap_or(false);
        let (count, gen) = self.check_assertion(ch, dev, "add", a, Some(&pk))?;
        let key = store::Passkey { pk: crypto::b64u(&pk), cred: cred.to_string(), sends, count, added: self.now() };
        self.with_device(dev, |d| d.passkey = Some(key))?;
        self.mark_verified(ch, gen);
        Ok(json!({}))
    }

    /// Desktop Settings: "Remove Face ID" for a phone that lost its passkey.
    pub fn remove_passkey(&self, dev: &str) -> Result<(), String> {
        self.with_device(dev, |d| d.passkey = None)
    }

    /// The desktop's "Remove Face ID": while "Approve sends on my phone" is on, a phone approves it first, since that
    /// phone then opens and pays without Face ID (security re-review H1).
    pub async fn remove_passkey_asked(&self, dev: &str) -> Result<(), String> {
        if self.approve_over().is_some() && self.passkey_of(dev).is_some() {
            let text = format!("Remove Face ID from \"{}\": it then opens and pays without it", self.device_name(dev));
            self.request_approval(Approve::Change { text }).await?;
        }
        self.remove_passkey(dev)
    }

    fn device_name(&self, dev: &str) -> String {
        self.devices.lock().unwrap().devices.iter().find(|d| d.id == dev).map(|d| d.name.clone()).unwrap_or_default()
    }

    fn log(&self, dev: &str, name: &str, p: &Pending, result: &str, detail: Value) {
        let mut entry = json!({
            "time": self.now(), "device": dev, "name": name, "address": p.address,
            "amount": to_ecx(p.sats), "result": result, "detail": detail,
        });
        with_kind(&mut entry, p.kind, p.house);
        self.write_log(entry);
    }

    /// A held send's lines carry its id, so Settings lists it once, with its latest state.
    fn log_held(&self, h: &Held, result: &str, detail: Value) {
        let mut entry = json!({
            "time": self.now(), "device": h.device, "name": h.name, "address": h.address,
            "amount": to_ecx(h.sats), "result": result, "detail": detail, "held": h.confirm,
        });
        with_kind(&mut entry, h.kind, h.house);
        self.write_log(entry);
    }

    fn write_log(&self, entry: Value) {
        // Not the sends that went through: their time could pick them out on the explorer.
        match entry["result"].as_str() {
            Some("sent") => {}
            r => crate::activity::note(&format!("phone send: {}", r.unwrap_or("?"))),
        }
        self.store.log_send(&entry);
        self.events.emit(EV_SEND, entry);
    }

    /// Take `sats` from the phone's allowance for `day` if it fits.
    fn reserve(&self, dev: &str, sats: u64, day: u64) -> bool {
        let mut devs = self.devices.lock().unwrap();
        let ok = devs.reserve(dev, sats, day);
        if ok {
            let _ = self.store.save_devices(&devs);
        }
        ok
    }

    /// Give back a reservation whose send didn't go out.
    fn release(&self, dev: &str, sats: u64, day: u64) {
        let mut devs = self.devices.lock().unwrap();
        devs.release(dev, sats, day);
        let _ = self.store.save_devices(&devs);
    }

    async fn send(&self, dev: &str, req: &Value, face_id: bool) -> Result<Value, String> {
        let a = &req["a"];
        let address = a["address"].as_str().unwrap_or("").trim().to_string();
        if !plausible_address(&address) {
            return Err("That isn't a FreeBank address (it starts with X).".into());
        }
        let sats = store::json_to_sats(&a["amount"])?;
        if sats == 0 {
            return Err("amount must be more than zero".into());
        }
        // The node's own check (checksum, network) before anything is reserved or held.
        let v = self.rpc.call("validateaddress", vec![json!(address)]).await?;
        if v["isvalid"] != true {
            return Err("That isn't a valid FreeBank address.".into());
        }
        self.act(dev, req, Pending { kind: Kind::Send, house: None, address, sats }, face_id).await
    }

    /// A phone's note action (v0.2.5): send notes of a house to an address, redeem them for ECX, or demand them. The
    /// amount is in ECX, as the desktop's Notes tab takes it (1 unit is 1 sat), and counts against the daily limit.
    async fn note_action(&self, dev: &str, req: &Value, kind: Kind, face_id: bool) -> Result<Value, String> {
        let a = &req["a"];
        let house = a["house"].as_u64().ok_or("Which house's notes?")?;
        let sats = store::json_to_sats(&a["amount"])?;
        if sats == 0 {
            return Err("amount must be more than zero".into());
        }
        let address = if kind == Kind::NoteSend {
            let address = a["address"].as_str().unwrap_or("").trim().to_string();
            if !plausible_address(&address) {
                return Err("That isn't a FreeBank address (it starts with X).".into());
            }
            let v = self.rpc.call("validateaddress", vec![json!(address)]).await?;
            if v["isvalid"] != true {
                return Err("That isn't a valid FreeBank address.".into());
            }
            address
        } else {
            String::new()
        };
        self.act(dev, req, Pending { kind, house: Some(house), address, sats }, face_id).await
    }

    /// Pay `p` within the phone's limit, or hold it for the desktop (or, with the app closed, refuse it). A note
    /// action's fee and carriers count against the limit too (security review M3). `face_id`: this phone's Face ID
    /// signed it.
    async fn act(&self, dev: &str, req: &Value, p: Pending, face_id: bool) -> Result<Value, String> {
        let name = self.device_name(dev);
        let day = self.today();
        let cost = p.cost();
        if !self.reserve(dev, cost, day) {
            return self.hold_or_refuse(dev, &name, &p, req, "limit", face_id);
        }
        let paid = {
            let _gate = self.wallet_gate.lock().await;
            self.pay(&p, None).await
        };
        match paid {
            Ok(txid) => {
                self.log(dev, &name, &p, "sent", txid.clone());
                Ok(json!({"txid": txid}))
            }
            // Within the limit, but the wallet is locked and phone sends aren't on: the desktop
            // decides, and a send it confirms doesn't count against the limit.
            Err(Pay::Locked | Pay::WrongPassphrase) => {
                self.release(dev, cost, day);
                self.hold_or_refuse(dev, &name, &p, req, "locked", face_id)
            }
            Err(Pay::NotTried(e) | Pay::Failed(e)) => {
                self.release(dev, cost, day);
                let e = consolidate_hint(p.kind, e);
                self.log(dev, &name, &p, "failed", json!(e));
                Err(e)
            }
            // It may have gone out: it keeps its place in the phone's limit.
            Err(Pay::MayHaveGone) => {
                self.log(dev, &name, &p, "unknown", json!(ERR_MAY_HAVE_GONE));
                Err(ERR_MAY_HAVE_GONE.into())
            }
        }
    }

    /// The phone's Notes: each house's notes this wallet holds, with what can be done with them. freebankd lists them
    /// only with the wallet unlocked, so a locked wallet is unlocked with the phone-send passphrase for a moment, as for
    /// a send; without that passphrase the phone hears how to allow it.
    async fn notes(&self) -> Result<Value, String> {
        let mine = {
            let _gate = self.wallet_gate.lock().await;
            let mut first = true;
            loop {
                let unlocked = match self.unlock_for_send(None).await {
                    Ok(u) => u,
                    Err(Pay::Locked | Pay::WrongPassphrase) => return Err(ERR_NOTES_LOCKED.into()),
                    Err(Pay::NotTried(e) | Pay::Failed(e)) => return Err(e),
                    Err(Pay::MayHaveGone) => return Err(ERR_MAY_HAVE_GONE.into()),
                };
                let r = self.rpc.call("listmynotes", vec![]).await;
                if unlocked {
                    self.lock_wallet().await;
                }
                match r {
                    Ok(v) => break v,
                    // Someone's unlock ran out between our look and the read: look again, once, as pay() does.
                    Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) && !unlocked && first => first = false,
                    Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) => return Err(ERR_NOTES_LOCKED.into()),
                    Err(e) => return Err(e.plain()),
                }
            }
        };
        let houses = self.rpc.call("listhouses", vec![]).await.unwrap_or(Value::Null);
        let houses = houses.as_array().map(|v| v.as_slice()).unwrap_or(&[]);
        let list = mine
            .as_array()
            .map(|v| v.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(|n| {
                let id = n["house_id"].as_u64().unwrap_or(0);
                let house = houses.iter().find(|h| h["id"].as_u64() == Some(id));
                let units = n["units"].as_u64().unwrap_or(0);
                let redeemable = n["redeemable_units"]
                    .as_u64()
                    .unwrap_or(if n["redeemable"] == true { units } else { 0 });
                let state = n["house_status"].as_str().unwrap_or("").chars().next().unwrap_or('?');
                json!({
                    "house": id,
                    "name": house.map(|h| h["classid"].clone()).unwrap_or(Value::Null),
                    "state": state.to_string(),
                    "state_name": state_name(state),
                    "amount": to_ecx(units),
                    "demanded": to_ecx(n["demanded_units"].as_u64().unwrap_or(0)),
                    "redeemable": to_ecx(redeemable),
                    "can_redeem": n["redeemable"] == true,
                    "can_demand": n["demandable"] == true,
                    "rate_bps": house.map(|h| h["defer_interest_bps"].clone()).unwrap_or(Value::Null),
                    "type": house_type(house),
                })
            })
            .collect();
        Ok(Value::Array(list))
    }

    /// Hold a send for the desktop, or with the app closed (the background part), refuse it: nobody
    /// is there to confirm it.
    fn hold_or_refuse(&self, dev: &str, name: &str, p: &Pending, req: &Value, why: &str, face_id: bool) -> Result<Value, String> {
        if !self.background.load(Ordering::SeqCst) {
            return Ok(self.hold(dev, name, p, req, why, face_id));
        }
        self.log(dev, name, p, "refused", json!({"why": why, "app": "closed"}));
        Err(if why == "limit" { ERR_CLOSED_LIMIT } else { ERR_CLOSED_LOCKED }.into())
    }

    /// The background part runs this phone link (`background.rs`).
    pub fn set_background(&self, on: bool) {
        self.background.store(on, Ordering::SeqCst);
    }

    pub fn is_background(&self) -> bool {
        self.background.load(Ordering::SeqCst)
    }

    /// Daemon mode: `f` runs whenever a paired phone asks for anything, after its session and Face ID checked out.
    pub fn set_waker(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap() = Some(f);
    }

    /// Daemon mode: the node is starting (requests that need it are answered `ERR_STARTING`), or answers again.
    pub fn set_starting(&self, on: bool) {
        self.starting.store(on, Ordering::SeqCst);
    }

    /// Daemon mode: the node couldn't be started; requests that need it hear `why` until the next try.
    pub fn set_wake_failure(&self, why: Option<String>) {
        *self.wake_failure.lock().unwrap() = why;
    }

    fn wake(&self) {
        let f = self.waker.lock().unwrap().clone();
        if let Some(f) = f {
            f();
        }
    }

    /// Hold a send for the desktop. Returns the phone's reply.
    fn hold(&self, dev: &str, name: &str, p: &Pending, req: &Value, why: &str, face_id: bool) -> Value {
        let h = Held {
            confirm: hex::encode(&rand16()[..8]),
            device: dev.into(),
            name: name.into(),
            address: p.address.clone(),
            sats: p.sats,
            kind: p.kind,
            house: p.house,
            at: self.now(),
            why: why.into(),
            req_id: req["id"].clone(),
            face_id,
            busy: false,
            ch: 0,
            cleared: None,
        };
        self.held.lock().unwrap().push(h.clone());
        self.save_held();
        self.log_held(&h, "held", json!({"confirm": h.confirm, "why": why}));
        self.events.emit(EV_HELD, serde_json::to_value(h.view(self.held_ttl())).unwrap());
        json!({"pending": h.confirm, "why": why})
    }

    /// Write held.json: the held sends (except one being paid right now, so a crash mid-send never
    /// tells the phone "nothing was sent") and the restart notices still owed.
    fn save_held(&self) {
        let held: Vec<Held> = self.held.lock().unwrap().iter().filter(|h| !h.busy).cloned().collect();
        let cancelled = self.cancelled.lock().unwrap().clone();
        if let Err(e) = self.store.save_held(&HeldFile { held, cancelled }) {
            eprintln!("phone relay: {e}");
        }
    }

    /// At start: every send still held when the app stopped is cancelled, never paid. Its phone is
    /// told on each new session until the send's time would have run out.
    fn cancel_after_restart(&self, saved: Vec<Held>) {
        let (now, ttl) = (self.now(), self.held_ttl());
        let had = {
            let mut c = self.cancelled.lock().unwrap();
            let had = !saved.is_empty() || !c.is_empty();
            c.extend(saved.iter().map(|h| Cancelled {
                confirm: h.confirm.clone(),
                device: h.device.clone(),
                at: h.at,
                req_id: h.req_id.clone(),
            }));
            c.retain(|x| now < x.at.saturating_add(ttl));
            had
        };
        for h in &saved {
            self.log_held(h, "cancelled", json!(ERR_RESTARTED));
        }
        if had {
            self.save_held();
        }
    }

    /// Drop held sends nobody answered in time, telling their phones, and forget restart notices
    /// whose time is up. The app runs this every second (`expire_forever`); the screens run it too.
    pub fn expire(&self) {
        self.expire_pairing();
        self.close_idle_channels();
        self.settle_scheduled();
        let (now, ttl) = (self.now(), self.held_ttl());
        let gone: Vec<Held> = {
            let mut h = self.held.lock().unwrap();
            let (gone, keep) = h.drain(..).partition(|x| !x.busy && now >= x.at.saturating_add(ttl));
            *h = keep;
            gone
        };
        let forgot = {
            let mut c = self.cancelled.lock().unwrap();
            let before = c.len();
            c.retain(|x| now < x.at.saturating_add(ttl));
            c.len() != before
        };
        if gone.is_empty() && !forgot {
            return;
        }
        self.save_held();
        let msg = expired_text(ttl);
        for h in &gone {
            if let Some(c) = h.cleared {
                self.uncount(c);
            }
            self.final_reply(h, "err", json!(msg));
            self.log_held(h, "expired", json!(h.confirm));
        }
        if !gone.is_empty() {
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    /// Pair requests waiting on a code whose 5 minutes are up are refused.
    fn expire_pairing(&self) {
        let mut p = self.pairing.lock().unwrap();
        if p.asks.is_empty() || p.code.as_ref().is_some_and(|c| self.now() < c.expires) {
            return;
        }
        let gone: Vec<Ask> = p.asks.drain(..).collect();
        drop(p);
        self.refuse_asks(&gone);
    }

    /// Close phone channels that have had neither a session nor a waiting pair request for
    /// IDLE_CHANNEL_SECS since their first frame (P5).
    fn close_idle_channels(&self) {
        let now = self.now();
        let due: Vec<u64> = self
            .idle
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, since)| now >= since.saturating_add(IDLE_CHANNEL_SECS))
            .map(|(ch, _)| *ch)
            .collect();
        for ch in due {
            self.close_channel(ch);
        }
    }

    /// Expire held sends on time while the app runs.
    pub async fn expire_forever(self: Arc<Self>) {
        let mut t = tokio::time::interval(Duration::from_secs(1));
        loop {
            t.tick().await;
            self.expire();
        }
    }

    /// A held send's final reply (`ok` or `err`), under its request id and with its confirm id, to
    /// every live session of its phone.
    fn final_reply(&self, h: &Held, key: &str, value: Value) {
        let mut reply = json!({"id": h.req_id, "pending": h.confirm});
        reply[key] = value;
        let now = self.now();
        {
            let mut finals = self.finals.lock().unwrap();
            finals.retain(|(_, until, _)| now < *until);
            finals.push((h.device.clone(), now + self.held_ttl(), reply.clone()));
        }
        for ch in self.channels_of(&h.device) {
            // The session that asked always hears it; others only once they may (I4, N1).
            if ch == h.ch || self.may_hear(ch, &h.device) {
                self.send_sealed(ch, &reply);
            }
        }
    }

    /// Held sends' outcomes from the held time, to a session that reconnected (N1). The page takes only
    /// the ones it is still waiting for.
    fn replay_finals(&self, ch: u64, dev: &str) {
        let now = self.now();
        let replies: Vec<Value> = {
            let mut finals = self.finals.lock().unwrap();
            finals.retain(|(_, until, _)| now < *until);
            finals.iter().filter(|(d, _, _)| d == dev).map(|(_, _, r)| r.clone()).collect()
        };
        for r in replies {
            self.send_sealed(ch, &r);
        }
    }

    pub fn held(&self) -> Vec<HeldView> {
        self.expire();
        let ttl = self.held_ttl();
        self.held.lock().unwrap().iter().map(|h| h.view(ttl)).collect()
    }

    /// A held send's amount and its fee allowance against the wallet's spendable balance. Only sends: a note action
    /// pays from notes, and its fee is small. A balance that can't be read lets the payment try, as before.
    async fn enough_for(&self, confirm: &str) -> Result<(), String> {
        let p = {
            let held = self.held.lock().unwrap();
            let h = held.iter().find(|h| h.confirm == confirm).ok_or("That send is no longer waiting.")?;
            Pending { kind: h.kind, house: h.house, address: h.address.clone(), sats: h.sats }
        };
        if p.kind != Kind::Send {
            return Ok(());
        }
        let Ok(bal) = self.rpc.call("getbalance", vec![]).await else { return Ok(()) };
        let Ok(have) = store::json_to_sats(&bal) else { return Ok(()) };
        // Its amount alone: the fee the node picks is smaller than any allowance, so a send of nearly everything can
        // still go.
        let need = p.cost();
        if have < need {
            return Err(format!(
                "Not enough ECX: the wallet has {} and this payment is {}, before its fee. Decline it, or add coins and \
                 try again; it's still waiting.",
                to_ecx(have),
                to_ecx(need)
            ));
        }
        Ok(())
    }

    /// Take a held send out, unless the desktop is paying it right now.
    fn take_held(&self, confirm: &str) -> Result<Held, String> {
        let h = {
            let mut held = self.held.lock().unwrap();
            let i = held.iter().position(|h| h.confirm == confirm).ok_or("That send is no longer waiting.")?;
            if held[i].busy {
                return Err("That send is being paid right now.".into());
            }
            held.remove(i)
        };
        self.save_held();
        Ok(h)
    }

    /// "Approve sends on my phone", before the desktop pays held payment `confirm` (security review M1): one the asking
    /// phone's own Face ID signed goes ahead; any other counts against the day's amount, or over it waits for a phone's
    /// approval. Once per hold: a try again with the passphrase doesn't ask twice.
    async fn clear_held(&self, confirm: &str) -> Result<(), String> {
        let (skip, p) = {
            let held = self.held.lock().unwrap();
            let h = held.iter().find(|h| h.confirm == confirm).ok_or("That send is no longer waiting.")?;
            let p = Pending { kind: h.kind, house: h.house, address: h.address.clone(), sats: h.sats };
            (h.face_id || h.cleared.is_some(), p)
        };
        if skip {
            return Ok(());
        }
        let c = self.clear_desktop(p.desk_cost(), p.approve()).await?;
        let kept = match self.held.lock().unwrap().iter_mut().find(|h| h.confirm == confirm) {
            Some(h) => {
                h.cleared = Some(c);
                true
            }
            None => false,
        };
        if !kept {
            self.uncount(c);
            return Err("That send is no longer waiting.".into());
        }
        Ok(())
    }

    /// Mark a held send as being paid (or not any more). Returns what it pays.
    fn set_busy(&self, confirm: &str, busy: bool) -> Result<Pending, String> {
        let r = {
            let mut held = self.held.lock().unwrap();
            let h = held.iter_mut().find(|h| h.confirm == confirm).ok_or("That send is no longer waiting.")?;
            if busy && h.busy {
                return Err("That send is being paid right now.".into());
            }
            h.busy = busy;
            Pending { kind: h.kind, house: h.house, address: h.address.clone(), sats: h.sats }
        };
        self.save_held();
        Ok(r)
    }

    /// The desktop's answer to a held send. The phone gets its final reply under the request's
    /// own id, with the confirm id, on every live session it has. A confirmed send doesn't count
    /// against the phone's limit. A locked wallet is unlocked for this one send with `pass`, or
    /// else with the phone-send passphrase; with neither, nothing happens and `need_passphrase`
    /// says so. A wrong `pass` is an error, and the send keeps waiting.
    pub async fn confirm_send(
        &self,
        confirm: &str,
        allow: bool,
        pass: Option<Zeroizing<String>>,
    ) -> Result<Confirmed, String> {
        self.expire();
        if !allow {
            let h = self.take_held(confirm)?;
            if let Some(c) = h.cleared {
                self.uncount(c);
            }
            self.log_held(&h, "declined", Value::Null);
            self.final_reply(&h, "err", json!(ERR_DECLINED));
            self.events.emit(EV_CHANGED, json!({}));
            return Ok(Confirmed { txid: None, need_passphrase: false });
        }
        // Not enough ECX for a send: said before a phone is asked or the passphrase is (v0.2.6, the UX walk-through:
        // the passphrase came first, then the payment failed and its dialog closed without a word). It keeps waiting.
        self.enough_for(confirm).await?;
        self.clear_held(confirm).await?;
        let _gate = self.wallet_gate.lock().await;
        // Busy while the wallet is unlocked and paid from: expiry and revoke leave it alone.
        let pending = self.set_busy(confirm, true)?;
        let one_off = pass.as_ref().map(|p| p.as_str()).filter(|p| !p.is_empty());
        let paid = match self.pay(&pending, one_off).await {
            Err(Pay::Locked) => {
                let _ = self.set_busy(confirm, false);
                return Ok(Confirmed { txid: None, need_passphrase: true });
            }
            Err(Pay::WrongPassphrase) => {
                let _ = self.set_busy(confirm, false);
                return Err(format!("{ERR_WRONG_PASSPHRASE} The payment is still waiting."));
            }
            Err(Pay::NotTried(e)) => {
                let _ = self.set_busy(confirm, false);
                return Err(format!("{e} The payment is still waiting."));
            }
            Ok(txid) => Ok(txid),
            Err(Pay::Failed(e)) => Err(consolidate_hint(pending.kind, e)),
            Err(Pay::MayHaveGone) => Err(ERR_MAY_HAVE_GONE.to_string()),
        };
        let h = {
            let mut held = self.held.lock().unwrap();
            let i = held.iter().position(|h| h.confirm == confirm).expect("a busy hold stays");
            held.remove(i)
        };
        self.save_held();
        // A payment that may have gone out keeps its "Approve sends on my phone" count.
        let unknown = matches!(&paid, Err(e) if e == ERR_MAY_HAVE_GONE);
        if let (Err(_), Some(c), false) = (&paid, h.cleared, unknown) {
            self.uncount(c);
        }
        let result = match paid {
            Ok(txid) => {
                self.log_held(&h, "sent", txid.clone());
                self.final_reply(&h, "ok", json!({"txid": txid}));
                Ok(Confirmed { txid: txid.as_str().map(String::from), need_passphrase: false })
            }
            Err(e) => {
                self.log_held(&h, if unknown { "unknown" } else { "failed" }, json!(e));
                self.final_reply(&h, "err", json!(e));
                Err(e)
            }
        };
        self.events.emit(EV_CHANGED, json!({}));
        result
    }

    // ----- the wallet ------------------------------------------------------------------------

    async fn wallet_state(&self) -> Result<Wallet, RpcFail> {
        let w = self.rpc.call("getwalletinfo", vec![]).await?;
        Ok(match w.get("unlocked_until").map(|v| v.as_u64()) {
            None => Wallet::Plain,
            Some(Some(0)) | Some(None) => Wallet::Locked,
            Some(Some(t)) => Wallet::Unlocked(t),
        })
    }

    async fn lock_wallet(&self) {
        if let Err(e) = self.rpc.call("walletlock", vec![]).await {
            // Its short unlock runs out on its own.
            eprintln!("phone relay: walletlock failed: {}", e.message);
        }
    }

    /// Use the app's one relock guard, shared with the screens' unlocks.
    pub fn share_relock_guard(&self, g: Arc<crate::wallet::RelockGuard>) {
        *self.relock.lock().unwrap() = g;
    }

    /// `walletpassphrase`, through the relock guard (`crate::wallet::RelockGuard` explains the
    /// freebankd deadlock it avoids).
    async fn unlock_wallet(&self, pass: &str, secs: u64) -> Result<(), RpcFail> {
        let guard = self.relock.lock().unwrap().clone();
        guard
            .run(
                secs,
                || self.rpc.call("walletpassphrase", vec![json!(pass), json!(secs)]),
                // A refusal changes no timer; an unanswered call may have set one.
                |e: &RpcFail| e.maybe,
            )
            .await
            .map(|_| ())
    }

    /// Pay from the node's wallet. An encrypted, locked wallet is unlocked for the send with
    /// `one_off` or else the phone-send passphrase, and locked again straight after; a wallet
    /// that was already unlocked is left as it was. The caller holds the wallet gate.
    async fn pay(&self, p: &Pending, one_off: Option<&str>) -> Result<Value, Pay> {
        let mut first = true;
        loop {
            let unlocked = self.unlock_for_send(one_off).await?;
            let (address, sats, house) = (json!(p.address), p.sats, json!(p.house));
            let r = match p.kind {
                Kind::Send => self.rpc.call("sendtoaddress", vec![address, json!(to_ecx(sats))]).await,
                Kind::NoteSend => self.rpc.call("transfernote", vec![house, json!(sats), json!(NOTE_FEE), address]).await,
                Kind::NoteRedeem => self.rpc.call("redeemnote", vec![house, json!(sats), json!(NOTE_FEE)]).await,
                Kind::NoteDemand => self.rpc.call("demandnote", vec![house, json!(sats), json!(NOTE_FEE)]).await,
            };
            if unlocked {
                self.lock_wallet().await;
            }
            match r {
                // The note calls answer {"txid": …}; sendtoaddress, the txid itself.
                Ok(v) => return Ok(v.get("txid").cloned().unwrap_or(v)),
                // Someone's unlock ran out between our look and the send: look again, once.
                Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) && !unlocked && first => first = false,
                Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) => return Err(Pay::Locked),
                Err(e) if e.maybe => return Err(Pay::MayHaveGone),
                Err(e) => return Err(Pay::Failed(e.plain())),
            }
        }
    }

    /// Ready the wallet to pay. Ok(true) when this unlocked it: the caller locks it again.
    async fn unlock_for_send(&self, one_off: Option<&str>) -> Result<bool, Pay> {
        match self.wallet_state().await.map_err(|e| Pay::NotTried(e.plain()))? {
            Wallet::Plain | Wallet::Unlocked(_) => Ok(false),
            Wallet::Locked => {
                let stored = self.pass.lock().unwrap().clone();
                let pass = match (one_off, stored.as_ref()) {
                    (Some(p), _) => p,
                    (None, Some(p)) => p.as_str(),
                    (None, None) => return Err(Pay::Locked),
                };
                match self.unlock_wallet(pass, SEND_UNLOCK_SECS).await {
                    Ok(()) => Ok(true),
                    Err(e) if e.code == Some(RPC_WALLET_PASSPHRASE_INCORRECT) && one_off.is_some() => {
                        Err(Pay::WrongPassphrase)
                    }
                    Err(e) if e.code == Some(RPC_WALLET_PASSPHRASE_INCORRECT) => {
                        // The passphrase changed since phone sends were turned on: they are off now.
                        self.forget_passphrase();
                        Err(Pay::Locked)
                    }
                    Err(e) => Err(Pay::NotTried(e.plain())),
                }
            }
        }
    }

    /// Turn on "Let my phone send while FreeBank is open": check `pass` against the wallet and keep
    /// it in memory. A locked wallet is unlocked for a second and locked again; an unlocked one is
    /// checked by unlocking it again until the same time, and stays unlocked.
    pub async fn phone_send_on(&self, pass: Zeroizing<String>) -> Result<(), String> {
        if pass.is_empty() {
            return Err("Enter the wallet's passphrase.".into());
        }
        let _gate = self.wallet_gate.lock().await;
        let mut waited = false;
        let (secs, relock) = loop {
            match self.wallet_state().await? {
                Wallet::Plain => return Err(ERR_NOT_ENCRYPTED.into()),
                Wallet::Locked => break (1, true),
                Wallet::Unlocked(t) => {
                    let left = t.saturating_sub(self.now());
                    // Someone's unlock is about to run out: let the node relock first (the
                    // deadlock `crate::wallet::RelockGuard` explains), then check on the locked wallet.
                    if !waited && Duration::from_secs(left) <= RELOCK_MARGIN {
                        waited = true;
                        tokio::time::sleep(Duration::from_secs(left) + RELOCK_MARGIN).await;
                        continue;
                    }
                    break (left.max(1), false);
                }
            }
        };
        match self.unlock_wallet(pass.as_str(), secs).await {
            Ok(()) => {}
            Err(e) if e.code == Some(RPC_WALLET_PASSPHRASE_INCORRECT) => return Err(ERR_WRONG_PASSPHRASE.into()),
            Err(e) => return Err(e.plain()),
        }
        if relock {
            self.lock_wallet().await;
        }
        *self.pass.lock().unwrap() = Some(pass);
        self.events.emit(EV_CHANGED, json!({}));
        Ok(())
    }

    /// Turn phone sends off: the passphrase is wiped from memory. Also run at quit, and when the last
    /// phone is revoked.
    pub fn forget_passphrase(&self) {
        let old = self.pass.lock().unwrap().take();
        if old.is_some() {
            drop(old);
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    /// A phone is paired: closing the app may keep it connected (background.rs).
    pub fn has_phones(&self) -> bool {
        !self.devices.lock().unwrap().devices.is_empty()
    }

    /// For the background part only (background.rs): a copy of the phone-send passphrase, if on.
    pub fn passphrase_for_handover(&self) -> Option<Zeroizing<String>> {
        self.pass.lock().unwrap().clone()
    }

    pub fn phone_send_is_on(&self) -> bool {
        self.pass.lock().unwrap().is_some()
    }

    pub async fn wallet_view(&self) -> WalletView {
        let phone_send = self.phone_send_is_on();
        let (encrypted, locked) = match self.wallet_state().await {
            Ok(Wallet::Plain) => (Some(false), false),
            Ok(Wallet::Locked) => (Some(true), true),
            Ok(Wallet::Unlocked(_)) => (Some(true), false),
            Err(_) => (None, false),
        };
        WalletView { encrypted, locked, phone_send }
    }
}

// ----- Approve on my phone (v0.2.5) ----------------------------------------------------------------

/// "Approve sends on my phone" (operator 2026-10-02: "yes..opt in I assume"; reworked after the v0.2.5 security review,
/// operator 2026-10-03: "yes to that"). Once the desktop's payments in the last 24 hours would come to more than the
/// amount set, a paired phone's Face ID first, over a challenge made for that one approval. Every way the app pays
/// counts, fees included: Send, the credit tabs, and a phone's held payment confirmed here (unless that phone's own
/// Face ID signed it). What would weaken it asks a phone too: turning it off, a higher amount, pairing another phone, a
/// phone's first Face ID, showing the recovery words. The recovery words can turn it off or raise the amount without a
/// phone (one that was lost), but that waits a day, and the phones can cancel it. Revoking a phone or removing its Face
/// ID needs nothing: it can only make payments harder. It guards the app only: someone with this computer and the
/// wallet passphrase could still use the node directly, or copy the wallet, or edit FreeBank's files.
impl Phone {
    /// Over this many sats in a day, the desktop's payments need a phone's approval (None: off). A change the recovery
    /// words scheduled happens here once its day is up.
    pub fn approve_over(&self) -> Option<u64> {
        self.settle_scheduled();
        self.config.lock().unwrap().approve_over
    }

    /// The paired phones with Face ID: the ones that can approve.
    pub fn approvers(&self) -> usize {
        self.approver_ids().len()
    }

    fn approver_ids(&self) -> Vec<String> {
        self.devices.lock().unwrap().devices.iter().filter(|d| d.passkey.is_some()).map(|d| d.id.clone()).collect()
    }

    /// What the desktop paid within the day's amount in the last 24 hours (sats); older payments are forgotten.
    fn desk_total(c: &mut Config, now: u64) -> u64 {
        c.desk_spent.retain(|(at, _)| now < at.saturating_add(DAY_SECS));
        c.desk_spent.iter().fold(0u64, |t, (_, s)| t.saturating_add(*s))
    }

    /// How much more the desktop can pay today without asking (sats); None when it is off.
    pub fn desk_left(&self) -> Option<u64> {
        let over = self.approve_over()?;
        let mut c = self.config.lock().unwrap();
        Some(over.saturating_sub(Self::desk_total(&mut c, self.now())))
    }

    /// Let a desktop payment costing `sats` through. Within the day's amount it counts now (`uncount` gives it back if
    /// the payment doesn't go out); over it, a phone approves `what` first, and then it doesn't count.
    pub async fn clear_desktop(&self, sats: u64, what: Approve) -> Result<Cleared, String> {
        let Some(over) = self.approve_over() else { return Ok(Cleared::Off) };
        {
            let now = self.now();
            let mut c = self.config.lock().unwrap();
            if Self::desk_total(&mut c, now).saturating_add(sats) <= over {
                c.desk_spent.push((now, sats));
                self.store.save_config(&c)?;
                return Ok(Cleared::Counted { at: now, sats });
            }
        }
        self.request_approval(what).await?;
        Ok(Cleared::Approved)
    }

    /// A payment `clear_desktop` counted didn't go out: it no longer counts.
    pub fn uncount(&self, cleared: Cleared) {
        let Cleared::Counted { at, sats } = cleared else { return };
        let mut c = self.config.lock().unwrap();
        if let Some(i) = c.desk_spent.iter().position(|&e| e == (at, sats)) {
            c.desk_spent.remove(i);
            let _ = self.store.save_config(&c);
        }
    }

    /// Set "Approve sends on my phone" (`over` in sats, None: off). On, or a lower amount: at once, once a phone has
    /// Face ID; a change the recovery words scheduled is dropped. Off, or a higher amount: at once with a phone's
    /// approval, or with the recovery words (`words_ok`: given, and this wallet's) a day from now, unless a phone or
    /// this desktop cancels it first: then the answer is when (`Some(due)`, unix seconds).
    pub async fn set_approve_over(&self, over: Option<u64>, words_ok: bool) -> Result<Option<u64>, String> {
        let now_over = self.approve_over();
        if over.is_some() && self.approvers() == 0 {
            return Err("Turn Face ID on in FreeBank on your phone first (its Settings): the phone approves these payments.".into());
        }
        let weaker = match (now_over, over) {
            (Some(_), None) => true,
            (Some(a), Some(b)) => b > a,
            _ => false,
        };
        if weaker && words_ok {
            return self.schedule(over).map(Some);
        }
        if weaker {
            let text = match over {
                None => "Turn \"Approve sends on my phone\" off: the desktop stops asking your phones".to_string(),
                Some(b) => format!(
                    "Ask your phones only when the desktop pays more than {} ECX in a day (now {} ECX)",
                    to_ecx(b),
                    to_ecx(now_over.unwrap_or(0))
                ),
            };
            self.request_approval(Approve::Change { text }).await?;
        }
        {
            let mut c = self.config.lock().unwrap();
            c.approve_over = over;
            if over.is_none() {
                c.desk_spent.clear();
            }
            self.store.save_config(&c)?;
        }
        self.drop_scheduled();
        self.events.emit(EV_CHANGED, json!({}));
        Ok(None)
    }

    /// The node wallet's key id (getwalletinfo's hdmasterkeyid), if it has one: what the recovery words must make.
    pub async fn hd_seed_id(&self) -> Result<Option<String>, String> {
        let w = self.rpc.call("getwalletinfo", vec![]).await.map_err(|e| e.plain())?;
        Ok(w["hdmasterkeyid"].as_str().map(String::from))
    }

    /// The change the recovery words scheduled, if any.
    pub fn scheduled(&self) -> Option<store::Scheduled> {
        self.settle_scheduled();
        self.config.lock().unwrap().scheduled.clone()
    }

    /// The recovery words' way: `over` happens a day from now. Every phone that can approve hears it now, or at its
    /// next session, and can cancel it.
    fn schedule(&self, over: Option<u64>) -> Result<u64, String> {
        let due = self.now() + WORDS_WAIT_SECS;
        self.drop_scheduled();
        {
            let mut c = self.config.lock().unwrap();
            c.scheduled = Some(store::Scheduled { over, due });
            self.store.save_config(&c)?;
        }
        crate::activity::note("phone approval: recovery words used; the change waits a day");
        self.post_scheduled();
        self.events.emit(EV_CHANGED, json!({}));
        Ok(due)
    }

    fn scheduled_id(due: u64) -> String {
        format!("words-{due}")
    }

    /// The scheduled change as a card for the phones that can approve (at start, too): approving makes it now,
    /// declining cancels it.
    fn post_scheduled(&self) {
        let Some(s) = self.config.lock().unwrap().scheduled.clone() else { return };
        let devices = self.approver_ids();
        let id = Self::scheduled_id(s.due);
        let mut challenge = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut challenge);
        let what = Approve::Scheduled { over: s.over, due: s.due };
        self.approvals
            .lock()
            .unwrap()
            .insert(id.clone(), Approval { challenge, devices: devices.clone(), expires: s.due, what, done: None });
        self.tell_live(&id, &devices);
    }

    /// Make the scheduled change once its day is up.
    fn settle_scheduled(&self) {
        let due = self.config.lock().unwrap().scheduled.as_ref().map(|s| s.due);
        if due.is_some_and(|d| self.now() >= d) {
            self.apply_scheduled();
        }
    }

    /// Make the scheduled change now (its day is up, or a phone approved it).
    fn apply_scheduled(&self) {
        let s = {
            let mut c = self.config.lock().unwrap();
            let Some(s) = c.scheduled.take() else { return };
            c.approve_over = s.over;
            if s.over.is_none() {
                c.desk_spent.clear();
            }
            let _ = self.store.save_config(&c);
            s
        };
        crate::activity::note("phone approval: the recovery words' change was made");
        self.end_approval(&Self::scheduled_id(s.due));
        self.events.emit(EV_CHANGED, json!({}));
    }

    /// Forget the scheduled change (cancelled, made at once, or replaced): its cards go.
    fn drop_scheduled(&self) -> Option<store::Scheduled> {
        let s = {
            let mut c = self.config.lock().unwrap();
            let s = c.scheduled.take();
            if s.is_some() {
                let _ = self.store.save_config(&c);
            }
            s
        };
        if let Some(s) = &s {
            self.end_approval(&Self::scheduled_id(s.due));
        }
        s
    }

    /// Cancel the scheduled change: this desktop's Cancel, or a phone's Decline.
    pub fn cancel_scheduled(&self) {
        if self.drop_scheduled().is_some() {
            crate::activity::note("phone approval: the recovery words' change was cancelled");
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    /// Ask the phones with Face ID to approve `what`, and wait for the first answer (or the time to run out). Their live
    /// sessions that passed Face ID hear it now; one that passes it in the meantime hears it then.
    pub async fn request_approval(&self, what: Approve) -> Result<(), String> {
        let devices = self.approver_ids();
        if devices.is_empty() {
            return Err(ERR_NO_APPROVER.into());
        }
        let id = hex::encode(&rand16()[..8]);
        let mut challenge = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut challenge);
        let secs = self.approve_secs.load(Ordering::SeqCst);
        let expires = self.now() + secs;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let text = what.text();
        self.approvals
            .lock()
            .unwrap()
            .insert(id.clone(), Approval { challenge, devices: devices.clone(), expires, what, done: Some(tx) });
        crate::activity::note("phone approval: asked");
        self.events.emit(EV_APPROVAL, json!({"id": id, "text": text, "expires": expires}));
        self.tell_live(&id, &devices);
        let r = match tokio::time::timeout(Duration::from_secs(secs), rx).await {
            Ok(Ok(r)) => r,
            _ => Err(ERR_APPROVE_TIMEOUT.to_string()),
        };
        self.end_approval(&id);
        crate::activity::note(if r.is_ok() { "phone approval: approved" } else { "phone approval: not approved" });
        r
    }

    /// Approval `id`, to the live sessions of `devices` that passed their Face ID: not to someone holding the unlocked
    /// phone without it (security review L1).
    fn tell_live(&self, id: &str, devices: &[String]) {
        let live: Vec<(u64, String)> = self
            .chans
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (_, dev))| devices.contains(dev))
            .map(|(ch, (_, dev))| (*ch, dev.clone()))
            .collect();
        for (ch, dev) in live {
            if !self.may_hear(ch, &dev) {
                continue;
            }
            if let Some(m) = self.approval_msg(id, &dev) {
                self.send_sealed(ch, &m);
            }
        }
    }

    /// What phone `dev` is shown for approval `id`: what it is, the challenge its Face ID signs, and the seconds left
    /// (the phone counts from when it hears them: its clock may differ, security review L6).
    fn approval_msg(&self, id: &str, dev: &str) -> Option<Value> {
        let cred = self.passkey_of(dev)?.cred;
        let now = self.now();
        let map = self.approvals.lock().unwrap();
        let a = map.get(id)?;
        let (kind, amount, address, text) = match &a.what {
            Approve::Send { sats, address } => ("send", json!(to_ecx(*sats)), address.clone(), Value::Null),
            Approve::Action { text, sats } => ("action", sats.map_or(Value::Null, |s| json!(to_ecx(s))), String::new(), json!(text)),
            Approve::Change { text } => ("change", Value::Null, String::new(), json!(text)),
            Approve::Scheduled { over, .. } => {
                ("scheduled", over.map_or(Value::Null, |b| json!(to_ecx(b))), String::new(), json!(a.what.text()))
            }
        };
        Some(json!({"approve": {
            "id": id, "what": kind, "amount": amount, "address": address, "text": text,
            "challenge": crypto::b64u(&a.challenge), "cred": cred, "expires": a.expires,
            "secs": a.expires.saturating_sub(now),
        }}))
    }

    /// A session of `dev` that just passed Face ID hears each approval still waiting for it.
    fn tell_approvals(&self, ch: u64, dev: &str) {
        let now = self.now();
        let ids: Vec<String> = self
            .approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, a)| a.devices.iter().any(|d| d == dev) && now < a.expires)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(m) = self.approval_msg(&id, dev) {
                self.send_sealed(ch, &m);
            }
        }
    }

    /// `approve`: a phone's answer to an approval, `{id, auth}` (Face ID over its challenge) or `{id, decline: true}`
    /// (from a session that passed Face ID). A proof that doesn't check out is refused and the approval keeps waiting.
    /// For the recovery words' change, approving makes it now and declining cancels it.
    fn approve_answer(&self, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        let id = a["id"].as_str().ok_or("Which approval?")?;
        let gone = || ERR_APPROVE_GONE.to_string();
        let (challenge, ok_dev, live, scheduled) = {
            let map = self.approvals.lock().unwrap();
            let ap = map.get(id).ok_or_else(gone)?;
            (
                ap.challenge,
                ap.devices.iter().any(|d| d == dev),
                self.now() <= ap.expires,
                matches!(ap.what, Approve::Scheduled { .. }),
            )
        };
        if !ok_dev {
            return Err("This phone can't answer that approval.".into());
        }
        if !live {
            return Err(gone());
        }
        let decline = a["decline"] == true;
        if decline {
            // Someone holding the unlocked phone without its Face ID can't decline either (security review L1).
            if !self.may_hear(ch, dev) {
                return Err(ERR_AUTH_NEEDED.into());
            }
        } else {
            self.check_stored(dev, &challenge, &a["auth"])?;
        }
        if scheduled {
            // The change this card showed, not one made since.
            let same = self.config.lock().unwrap().scheduled.as_ref().is_some_and(|s| Self::scheduled_id(s.due) == id);
            if !same {
                return Err(gone());
            }
            if decline {
                self.cancel_scheduled();
            } else {
                self.apply_scheduled();
            }
            return Ok(json!({}));
        }
        let answer = if decline { Err(ERR_APPROVE_DECLINED.to_string()) } else { Ok(()) };
        let done = self.approvals.lock().unwrap().get_mut(id).and_then(|ap| ap.done.take()).ok_or_else(gone)?;
        let _ = done.send(answer);
        Ok(json!({}))
    }

    /// The desktop stops waiting for approval `id` ("Cancel" on its waiting screen).
    pub fn cancel_approval(&self, id: &str) {
        let done = self.approvals.lock().unwrap().get_mut(id).and_then(|a| a.done.take());
        if let Some(done) = done {
            let _ = done.send(Err(ERR_APPROVE_CANCELLED.into()));
        }
    }

    /// Forget approval `id`: its phones' cards go, and the desktop's waiting screen.
    fn end_approval(&self, id: &str) {
        let Some(a) = self.approvals.lock().unwrap().remove(id) else { return };
        let live: Vec<u64> = self
            .chans
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (_, dev))| a.devices.contains(dev))
            .map(|(ch, _)| *ch)
            .collect();
        for ch in live {
            self.send_sealed(ch, &json!({"approve-done": id}));
        }
        if !matches!(a.what, Approve::Scheduled { .. }) {
            self.events.emit(EV_APPROVAL, json!({"id": id, "done": true}));
        }
    }

    /// The approvals the desktop is waiting for now, for its screen after a reload: `{id, text, expires}`. Not the
    /// recovery words' change: Settings shows that one.
    pub fn approvals_waiting(&self) -> Vec<Value> {
        let now = self.now();
        self.approvals
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, a)| now <= a.expires && !matches!(a.what, Approve::Scheduled { .. }))
            .map(|(id, a)| json!({"id": id, "text": a.what.text(), "expires": a.expires}))
            .collect()
    }

    /// While it is on, a phone's first Face ID would make it an approver, so another approver agrees first (security
    /// review M2). Ok once one has, in the last 10 minutes. Otherwise that approval is asked now (once at a time), and
    /// the phone hears to try again after it.
    fn first_passkey_allowed(self: &Arc<Self>, dev: &str) -> Result<(), String> {
        if self.approve_over().is_none() || self.passkey_of(dev).is_some() {
            return Ok(());
        }
        if self.grants.lock().unwrap().get(dev).is_some_and(|&until| self.now() <= until) {
            return Ok(());
        }
        if self.approvers() == 0 {
            return Err(ERR_ADD_NO_APPROVER.into());
        }
        // Said no, or not answered: not asked again for 10 minutes (security re-review L6).
        if self.grant_refused.lock().unwrap().get(dev).is_some_and(|&until| self.now() < until) {
            return Err(ERR_ADD_REFUSED.into());
        }
        if self.granting.lock().unwrap().insert(dev.to_string()) {
            let (me, dev) = (self.clone(), dev.to_string());
            let (name, added, seen) = {
                let devs = self.devices.lock().unwrap();
                let d = devs.devices.iter().find(|d| d.id == dev);
                (d.map(|d| d.name.clone()).unwrap_or_default(), d.map_or(0, |d| d.added), d.and_then(|d| d.last_seen))
            };
            tokio::spawn(async move {
                let text = format!(
                    "Let \"{name}\" (paired {}, last seen {}) approve the desktop's payments too: it is turning Face ID on",
                    store::day_text(added),
                    seen.map_or("never".to_string(), store::day_text)
                );
                if me.request_approval(Approve::Change { text }).await.is_ok() {
                    let until = me.now() + GRANT_SECS;
                    me.grants.lock().unwrap().insert(dev.clone(), until);
                } else {
                    let until = me.now() + GRANT_SECS;
                    me.grant_refused.lock().unwrap().insert(dev.clone(), until);
                }
                me.granting.lock().unwrap().remove(&dev);
            });
        }
        Err(ERR_ADD_ASKED.into())
    }

    /// Whether `dev` would be asked to wait for another phone before turning Face ID on (its Settings say so first).
    fn first_passkey_asks(&self, dev: &str) -> bool {
        self.approve_over().is_some()
            && self.passkey_of(dev).is_none()
            && self.grants.lock().unwrap().get(dev).is_none_or(|&until| self.now() > until)
    }

    #[cfg(test)]
    fn set_approve_secs(&self, secs: u64) {
        self.approve_secs.store(secs, Ordering::SeqCst);
    }
}

// ----- The credit tabs' payments, for "Approve sends on my phone" ---------------------------------

/// The credit calls that sign with the wallet (security review M1): each costs at least its fee.
pub const CREDIT_PAYMENTS: &[&str] = &[
    "mintnote",
    "transfernote",
    "redeemnote",
    "demandnote",
    "registerhouse",
    "attesthouse",
    "swapnote",
    "createpool",
    "addpoolliquidity",
    "removepoolliquidity",
    "issuebill",
    "endorsebill",
    "retirebill",
    "claimbillescrow",
    // Node v0.2.19, members-only houses: each pays a fee.
    "addhousemembers",
    "removehousemembers",
    "purgehousemembers",
];

/// What a credit call from the desktop's tabs costs this wallet (sats) and how a phone is asked about it; None for the
/// calls that pay nothing. Counted: the fee, and whatever leaves the wallet for someone else or a pool: notes sent,
/// what goes into a swap or a pool, a bill's bond, a bill handed on or paid (`bill`: its amount, from getbill). A cost
/// that can't be read from the call counts as more than any day's amount, so a phone is asked.
pub fn credit_payment(method: &str, p: &[Value], bill: Option<u64>) -> Option<(u64, Approve)> {
    if !CREDIT_PAYMENTS.contains(&method) {
        return None;
    }
    let units = |i: usize| p.get(i).and_then(Value::as_u64);
    let ecx = |i: usize| p.get(i).and_then(|v| store::json_to_sats(v).ok());
    let id = |i: usize| p.get(i).and_then(Value::as_u64).map_or("?".to_string(), |n| n.to_string());
    let e = |s: Option<u64>| s.map_or("?".to_string(), |s| to_ecx(s).to_string());
    // The fee is the last argument, except where optional ones follow it: transfernote's and mintnote's (house, units,
    // fee, address) and registerhouse's (tier, threshold, classid, denommg, pledges, fee, type; node v0.2.19).
    let fee = match method {
        "transfernote" | "mintnote" => p.get(2),
        "registerhouse" => p.get(5),
        _ => p.last(),
    };
    let fee = fee.and_then(|v| store::json_to_sats(v).ok());
    let (out, text): (Option<u64>, String) = match method {
        "transfernote" => {
            let to = p.get(3).and_then(Value::as_str).unwrap_or("");
            let out = if to.is_empty() { Some(0) } else { units(1) };
            let to = if to.is_empty() { "this wallet".to_string() } else { to.to_string() };
            (out.map(|o| o + NOTE_CARRIER_SATS), format!("Send {} ECX of house #{}'s notes to {to}", e(units(1)), id(0)))
        }
        "swapnote" => (units(2), format!("Swap {} ECX in pool #{}", e(units(2)), id(0))),
        "createpool" => (
            units(1).zip(units(2)).map(|(a, b)| a.saturating_add(b)),
            format!("Create pool #{} with {} ECX of notes and {} ECX", id(0), e(units(1)), e(units(2))),
        ),
        "addpoolliquidity" => (
            units(1).zip(units(2)).map(|(a, b)| a.saturating_add(b)),
            format!("Add {} ECX of notes and {} ECX to pool #{}", e(units(1)), e(units(2)), id(0)),
        ),
        "issuebill" => (ecx(2), format!("Issue a bill for {} ECX, bonded with {} ECX", e(ecx(1)), e(ecx(2)))),
        "endorsebill" => (bill, format!("Hand bill #{} ({} ECX) to another holder", id(0), e(bill))),
        "retirebill" => (bill, format!("Retire bill #{}: pay its holder {} ECX", id(0), e(bill))),
        // Minted to this wallet: only the fee. Minted to an address (node v0.2.19), the notes leave this wallet's house
        // to someone who can redeem them from its reserves: they count (security review of v0.2.6, M1).
        "mintnote" => match p.get(3).and_then(Value::as_str).filter(|a| !a.is_empty()) {
            None => (Some(0), format!("Mint house #{}'s notes", id(0))),
            Some(to) => (units(1), format!("Mint {} ECX of house #{}'s notes to {to}", e(units(1)), id(0))),
        },
        "redeemnote" => (Some(NOTE_CARRIER_SATS), format!("Redeem {} ECX of house #{}'s notes", e(units(1)), id(0))),
        "demandnote" => (Some(NOTE_CARRIER_SATS), format!("Demand {} ECX of house #{}'s notes", e(units(1)), id(0))),
        "registerhouse" => {
            // Its pledges, an array of ECX amounts (security re-review L1).
            let pledged = p.get(4).and_then(Value::as_array).and_then(|a| {
                a.iter().try_fold(0u64, |t, v| store::json_to_sats(v).ok().map(|s| t.saturating_add(s)))
            });
            (pledged, format!("Register a house, pledging {} ECX", e(pledged)))
        }
        "attesthouse" => (Some(0), format!("Attest house #{}'s reserves", id(0))),
        "removepoolliquidity" => (Some(0), format!("Take liquidity out of pool #{}", id(0))),
        "claimbillescrow" => (Some(0), format!("Claim bill #{}'s bond", id(0))),
        "addhousemembers" | "removehousemembers" => {
            let n = p.get(1).and_then(Value::as_array).map_or(0, |a| a.len());
            let (verb, to) = if method == "addhousemembers" { ("Add", "to") } else { ("Remove", "from") };
            (Some(0), format!("{verb} {n} member{} {to} house #{}", if n == 1 { "" } else { "s" }, id(0)))
        }
        "purgehousemembers" => (Some(0), format!("Clear removed members' records from house #{}", id(0))),
        _ => (None, method.to_string()),
    };
    let cost = out.zip(fee).map_or(u64::MAX, |(o, f)| o.saturating_add(f));
    Some((cost, Approve::Action { text, sats: (cost != u64::MAX).then_some(cost) }))
}
