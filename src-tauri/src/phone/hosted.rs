//! Hosted wallets (v0.2.8; operator, 2026-10-04: "its a temporary hosted wallet at the bank until he gets home.. and
//! installed on his desktop/macbook"). Design: `gateway/docs/distribution/PAY_IN_NOTES_DESIGN.md`; wire protocol:
//! `relay/PROTOCOL.md`, "Hosted wallets (app v0.2.8)".
//!
//! An owner's phone invites someone (a shopkeeper) to one of the wallet's members-only houses. The invited phone pairs
//! with the invite's one-time code, the inviting phone allows it with Face ID, and this desktop makes the shopkeeper a
//! wallet of his own in its node, with fresh 24 words (BIP85 index 0, as Setup makes the main wallet, so the desktop
//! app's "Restore from recovery words" brings it back), encrypted with a random passphrase kept here. His phone shows
//! the words once; then the desktop adds the wallet's first address to the house. Later he restores the words on a
//! computer of his own, his phone pairs there, and tells this desktop "moved"; its owner deletes the copy here.
//!
//! **The boundary:** a hosted phone is kept in `hosted.json`, never in the owner's `devices.json`, and is served by
//! `serve_hosted`, never by the owner's narrow door. Its device id carries the `h:` prefix inside the phone link, so
//! a session knows which door it may use. Nothing here reaches the owner's wallet except joining the house, which the
//! desktop signs with the phone-send passphrase.

use super::crypto;
use super::store::{self, to_ecx, Device, Passkey};
use super::*;
use crate::seed::{self, Chain};
use std::path::PathBuf;
use zeroize::Zeroizing;

/// An invite lasts 10 minutes.
pub const INVITE_TTL_SECS: u64 = 600;
/// At most this many invites open at once.
pub const MAX_INVITES: usize = 4;
/// At most this many phones wait to join on one invite.
pub const ASKS_PER_INVITE: usize = 2;
/// A hosted phone's device id inside the phone link (sessions, Face ID).
pub const PREFIX: &str = "h:";
/// The fee for joining the house (`addhousemembers`).
pub const MEMBER_FEE: f64 = 0.001;
/// sECX the house's desktop gives a new hosted wallet for its payments' fees: a note payment needs its fee (0.001) and
/// small carrier outputs, which a wallet holding only notes can't fund (found in the end-to-end run on a real node:
/// "Could not fund the transfer dust + fee!"). About nine note payments.
pub const FEE_FLOAT: f64 = 0.01;
/// The node's words when a wallet has no sECX for a note payment's fee and carriers.
const NO_FEE_MONEY: &str = "Could not fund";
pub const ERR_NO_FEE_MONEY: &str =
    "Your wallet has no sECX left for this payment's fee. Ask your house for a little sECX: it pays the fees of note payments.";
/// How often joining looks again (the member change waits for a block).
pub const JOIN_RETRY_SECS: u64 = 20;
/// Moving home: a note payment's fee and carriers need about this much sECX in the hosted wallet.
pub const MOVE_FEE_NEED: f64 = 0.0015;
/// sECX left below this after the notes have gone counts as nothing (a send would cost more than it moves).
pub const MOVE_DUST: f64 = 0.0001;
/// At most this many fee top-ups from the house per move, and this many refusals in a row before it stops.
pub const MOVE_TOPUPS: u32 = 3;
pub const MOVE_REFUSALS: u32 = 6;
/// After the move, how often the old copy is looked at for money that arrives late, seconds.
pub const MOVED_WATCH_SECS: u64 = 600;
/// How long a hosted wallet stays unlocked for one payment.
const HOSTED_UNLOCK_SECS: u64 = 10;

pub const ERR_INVITE_FACE_ID: &str = "Add Face ID on this phone first: inviting someone takes it.";
pub const ERR_INVITE_NEEDS_SEND: &str =
    "Turn on \"Let my phone send while FreeBank is open\" on your desktop first: joining your house is signed there.";
pub const ERR_NOT_MINE: &str = "That isn't a members-only house this wallet runs.";
pub const ERR_MOVED: &str = "This wallet has moved to your own computer.";
pub const ERR_REMOVED: &str = "This phone was removed on the desktop.";
pub const ERR_HOSTED_FACE_ID: &str = "Add Face ID on this phone first.";

/// Where a hosted wallet is on its way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// The wallet is being made (the node restarts once).
    #[default]
    Setup,
    /// Its words wait to be shown and confirmed.
    Words,
    /// Its address is being added to the house.
    Joining,
    Ready,
    /// Its money is going to the shopkeeper's own computer (fresh words; Michael, 2026-10-05).
    Moving,
    Moved,
    Failed,
}

/// A phone the desktop keeps a wallet for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostedPhone {
    /// As an owner's device id (`Device::id_for`), without the prefix.
    pub id: String,
    pub name: String,
    pub p_pub: String,
    pub added: u64,
    #[serde(default)]
    pub last_seen: Option<u64>,
    #[serde(default)]
    pub passkey: Option<Passkey>,
    pub house: u64,
    pub house_name: String,
    /// The inviting phone's name.
    pub by: String,
    /// `hosted-<id>` in the node.
    pub wallet: String,
    pub step: Step,
    #[serde(default)]
    pub why: Option<String>,
    /// The wallet's first address: its member address at the house.
    #[serde(default)]
    pub member: Option<String>,
    /// The member change sent, once (its txid, or "unknown" when the node didn't answer).
    #[serde(default)]
    pub join_txid: Option<String>,
    #[serde(default)]
    pub join_sent_at: Option<u64>,
    /// The fee float sent (`FEE_FLOAT`), once: its txid, or "unknown".
    #[serde(default)]
    pub float_txid: Option<String>,
    #[serde(default)]
    pub moved_at: Option<u64>,
    /// Moving home (fresh words): the shopkeeper's own computer's address, its member change (txid, when), and the
    /// last transfer to it (txid, when).
    #[serde(default)]
    pub move_to: Option<String>,
    #[serde(default)]
    pub move_member_txid: Option<String>,
    #[serde(default)]
    pub move_member_at: Option<u64>,
    #[serde(default)]
    pub move_txid: Option<String>,
    #[serde(default)]
    pub move_at: Option<u64>,
    /// The move found the wallet holding nothing: deleting the copy deletes it outright.
    #[serde(default)]
    pub empty: bool,
    /// Houses whose notes the move couldn't send or redeem: they stay in this wallet (the re-review of v0.2.8, N1).
    #[serde(default)]
    pub move_skip: Vec<u64>,
    /// Refusals in a row while moving, and fee top-ups from the house, so a move can't loop for ever.
    #[serde(default)]
    pub move_refusals: u32,
    #[serde(default)]
    pub move_topups: u32,
    /// After the move, the old member address taken off the house (its txid), so notes stop going there (N2).
    #[serde(default)]
    pub old_removed: Option<String>,
    /// The owner said delete the copy: the phone is no longer served, and the wallet file moves aside at the node's
    /// next start (`sweep_removed`).
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HostedList {
    pub phones: Vec<HostedPhone>,
}

/// `hosted-keys.json` (0600): each hosted wallet's passphrase, and its words' entropy until they are confirmed.
#[derive(Default, Serialize, Deserialize)]
pub struct HostedKeys {
    pub keys: Vec<HostedKey>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HostedKey {
    pub id: String,
    pub pass: String,
    #[serde(default)]
    pub entropy: Option<String>,
}

/// An open invite (memory only).
pub struct Invite {
    pub c: [u8; 16],
    pub house: u64,
    pub house_name: String,
    pub by_dev: String,
    pub by_name: String,
    pub expires: u64,
    pub used: bool,
}

/// A hosted pair request waiting for the inviting phone (or the desktop).
#[derive(Clone, Serialize)]
pub struct HostedAsk {
    #[serde(skip)]
    pub ch: u64,
    #[serde(skip)]
    pub p_pub: String,
    #[serde(skip)]
    pub c: [u8; 16],
    #[serde(skip)]
    pub by_dev: String,
    #[serde(skip)]
    pub challenge: [u8; 32],
    pub id: String,
    pub device: String,
    pub name: String,
    pub code: String,
    pub house: u64,
    pub house_name: String,
    pub by: String,
    pub expires: u64,
}

/// What the desktop's Settings show of a hosted phone.
#[derive(Clone, Serialize)]
pub struct HostedView {
    pub id: String,
    pub name: String,
    pub house: u64,
    pub house_name: String,
    pub by: String,
    pub added: u64,
    pub last_seen: Option<u64>,
    pub step: Step,
    pub why: Option<String>,
    pub member: Option<String>,
    pub moved_at: Option<u64>,
    pub remove: bool,
    pub empty: bool,
    pub move_to: Option<String>,
    pub face_id: bool,
    pub online: bool,
}

/// Makes a hosted wallet in the node: the app's implementation creates it, encrypts it and starts the node again
/// (`wallets::HostedMaker`).
pub trait Maker: Send + Sync {
    /// `createwallet name` (or open it, if a file of that name is there), then `encryptwallet pass` unless it has a
    /// passphrase already; the node stops and is started again.
    fn create_encrypted<'a>(&'a self, name: &'a str, pass: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

impl store::Store {
    pub fn load_hosted(&self) -> HostedList {
        self.read_json("hosted.json")
    }
    pub fn save_hosted(&self, h: &HostedList) -> Result<(), String> {
        self.write_private("hosted.json", &serde_json::to_vec_pretty(h).map_err(|e| e.to_string())?)
    }
    /// The keys file, or why it can't be read: a missing file is an empty list, but a damaged or unreadable one is an
    /// error, so nothing writes over it (security review of v0.2.8, L1).
    pub fn load_hosted_keys_checked(&self) -> Result<HostedKeys, String> {
        match std::fs::read(self.dir.join("hosted-keys.json")) {
            Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("hosted-keys.json can't be read ({e}); nothing was changed")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HostedKeys::default()),
            Err(e) => Err(format!("hosted-keys.json can't be read ({e}); nothing was changed")),
        }
    }
    /// Write the keys file, keeping the last one beside it (`hosted-keys.json.bak`).
    pub fn save_hosted_keys(&self, k: &HostedKeys) -> Result<(), String> {
        if let Ok(old) = std::fs::read(self.dir.join("hosted-keys.json")) {
            self.write_private("hosted-keys.json.bak", &old)?;
        }
        self.write_private("hosted-keys.json", &serde_json::to_vec_pretty(k).map_err(|e| e.to_string())?)
    }
}

/// A hosted phone's id inside the phone link, or None for an owner's phone.
pub fn hosted_id(dev: &str) -> Option<&str> {
    dev.strip_prefix(PREFIX)
}

/// One change to the keys file at a time: two tasks reading and writing it at once would drop each other's entry
/// (security review of v0.2.8, L1).
static KEYS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Payments from hosted phones: when each last paid (a scripted client can't hold the wallet gate or flood the log;
/// security review of v0.2.8, L8).
static LAST_PAY: std::sync::Mutex<Option<HashMap<String, u64>>> = std::sync::Mutex::new(None);
/// The hosted wallets whose move loop runs (one each).
static MOVING: std::sync::Mutex<Option<std::collections::HashSet<String>>> = std::sync::Mutex::new(None);
/// The least time between two payments from one hosted phone, seconds.
pub const PAY_GAP_SECS: u64 = 5;

fn hex32() -> String {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    hex::encode(b)
}

impl Phone {
    // ----- the list --------------------------------------------------------------------------

    /// The `h:` id of the hosted phone with this key, if it is still served.
    pub(super) fn hosted_by_pub(&self, p_pub: &str) -> Option<String> {
        self.hosted.lock().unwrap().phones.iter().find(|h| h.p_pub == p_pub && !h.remove).map(|h| format!("{PREFIX}{}", h.id))
    }

    pub(super) fn hosted_get(&self, hid: &str) -> Option<HostedPhone> {
        self.hosted.lock().unwrap().phones.iter().find(|h| h.id == hid).cloned()
    }

    /// Change one hosted phone and save the list.
    pub(super) fn with_hosted<R>(&self, hid: &str, f: impl FnOnce(&mut HostedPhone) -> R) -> Result<R, String> {
        let mut list = self.hosted.lock().unwrap();
        let h = list.phones.iter_mut().find(|h| h.id == hid).ok_or(ERR_REMOVED)?;
        let r = f(h);
        self.store.save_hosted(&list)?;
        Ok(r)
    }

    fn set_step(&self, hid: &str, step: Step, why: Option<String>) {
        let _ = self.with_hosted(hid, |h| {
            h.step = step;
            h.why = why;
        });
        self.events.emit(EV_CHANGED, json!({}));
    }

    fn key_of(&self, hid: &str) -> Option<HostedKey> {
        let _one = KEYS.lock().unwrap();
        self.store.load_hosted_keys_checked().ok()?.keys.into_iter().find(|k| k.id == hid)
    }

    /// Keep (or replace) one wallet's key; refused when the file can't be read.
    fn save_key(&self, key: HostedKey) -> Result<(), String> {
        let _one = KEYS.lock().unwrap();
        let mut keys = self.store.load_hosted_keys_checked()?;
        keys.keys.retain(|k| k.id != key.id);
        keys.keys.push(key);
        self.store.save_hosted_keys(&keys)
    }

    /// List a hosted phone directly, as an allowed invite would (the real-node test).
    #[cfg(test)]
    pub(crate) fn host_for_test(&self, h: HostedPhone) {
        let mut list = self.hosted.lock().unwrap();
        list.phones.push(h);
        self.store.save_hosted(&list).unwrap();
    }

    pub fn hosted_list(&self) -> Vec<HostedView> {
        let list = self.hosted.lock().unwrap().phones.clone();
        list.into_iter()
            .map(|h| HostedView {
                online: !self.channels_of(&format!("{PREFIX}{}", h.id)).is_empty(),
                id: h.id,
                name: h.name,
                house: h.house,
                house_name: h.house_name,
                by: h.by,
                added: h.added,
                last_seen: h.last_seen,
                step: h.step,
                why: h.why,
                member: h.member,
                moved_at: h.moved_at,
                remove: h.remove,
                empty: h.empty,
                move_to: h.move_to,
                face_id: h.passkey.is_some(),
            })
            .collect()
    }

    /// Delete the copy here: the phone is no longer served, and its wallet file moves aside at the node's next start.
    pub fn hosted_remove(&self, hid: &str) -> Result<(), String> {
        self.with_hosted(hid, |h| h.remove = true)?;
        let dev = format!("{PREFIX}{hid}");
        for ch in self.channels_of(&dev) {
            self.chans.lock().unwrap().remove(&ch);
            self.auth.lock().unwrap().remove(&ch);
            self.send_clear(ch, json!({"t": "denied"}));
            self.close_channel(ch);
        }
        crate::activity::note("hosted wallets: a hosted copy marked for deletion");
        self.events.emit(EV_CHANGED, json!({}));
        self.wake.notify_one();
        Ok(())
    }

    /// Whether the link has hosted phones to serve or invites open.
    pub(super) fn hosting_wanted(&self) -> bool {
        let now = self.now();
        self.hosted.lock().unwrap().phones.iter().any(|h| !h.remove)
            || self.invites.lock().unwrap().iter().any(|i| !i.used && now < i.expires)
            || !self.hosted_asks.lock().unwrap().is_empty()
    }

    // ----- inviting (an owner's phone) ---------------------------------------------------------

    /// The members-only and redeem-only houses this wallet runs: one of the house's partner keys is the wallet's.
    pub(super) async fn houses_mine(&self) -> Result<Value, String> {
        let houses = self.rpc.call("listhouses", vec![]).await?;
        let mut mine = Vec::new();
        for h in houses.as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
            let kind = house_type(Some(h));
            if kind == "open" {
                continue;
            }
            let Some(id) = h["id"].as_u64() else { continue };
            let full = self.rpc.call("gethouse", vec![json!(id)]).await?;
            // A member change takes M of the house's N partner keys: this wallet must hold M of them (security
            // review of v0.2.8, L2: with fewer, every invite into it would fail at joining).
            let need = full["threshold"].as_u64().unwrap_or(1).max(1);
            let mut held = 0;
            for p in full["partners"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
                if p["status"].as_str().is_some_and(|st| st != "active") {
                    continue;
                }
                let Some(pk) = p["pubkey"].as_str().and_then(|x| hex::decode(x).ok()) else { continue };
                let info = self.rpc.call("getaddressinfo", vec![json!(seed::p2pkh_address(&pk))]).await;
                if info.is_ok_and(|i| i["ismine"] == true) {
                    held += 1;
                }
            }
            if held >= need {
                mine.push(json!({"house": id, "name": h["classid"], "type": kind}));
            }
        }
        Ok(Value::Array(mine))
    }

    /// `invite`: a one-time code for a hosted pairing into one of this wallet's members-only houses. Takes the
    /// inviting phone's Face ID, and the phone-send passphrase (joining the house is signed here).
    pub(super) async fn invite(self: &Arc<Self>, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        if self.passkey_of(dev).is_none() {
            return Err(ERR_INVITE_FACE_ID.into());
        }
        self.check_assertion(ch, dev, "invite", &a["auth"], None)?;
        if self.pass.lock().unwrap().is_none() {
            return Err(ERR_INVITE_NEEDS_SEND.into());
        }
        let house = a["house"].as_u64().ok_or("Which house?")?;
        let mine = self.houses_mine().await?;
        let h = mine
            .as_array()
            .and_then(|l| l.iter().find(|h| h["house"].as_u64() == Some(house)).cloned())
            .ok_or(ERR_NOT_MINE)?;
        let house_name = h["name"].as_str().unwrap_or("").to_string();
        let by_name = self.device_name(dev);
        let page = self.page_url()?;
        let c = rand16();
        let link = json!({
            "v": 1, "relay": self.relay_url(), "room": self.room, "d": self.d_pub, "c": crypto::b64u(&c),
            "h": {"house": house, "name": house_name, "by": by_name},
        });
        let url = format!("{page}/#pair={}", crypto::b64u(link.to_string().as_bytes()));
        let now = self.now();
        {
            let mut inv = self.invites.lock().unwrap();
            inv.retain(|i| !i.used && now < i.expires);
            if inv.len() >= MAX_INVITES {
                return Err("Four invites are open already. Wait for one to be used or to run out.".into());
            }
            inv.push(Invite { c, house, house_name, by_dev: dev.to_string(), by_name, expires: now + INVITE_TTL_SECS, used: false });
        }
        crate::activity::note("hosted wallets: an invite made");
        self.wake.notify_one();
        Ok(json!({"url": url, "secs": INVITE_TTL_SECS}))
    }

    /// The live invites' codes, newest first, for `on_pair`.
    pub(super) fn invite_codes(&self) -> Vec<[u8; 16]> {
        let now = self.now();
        self.invites.lock().unwrap().iter().rev().filter(|i| !i.used && now < i.expires).map(|i| i.c).collect()
    }

    // ----- joining: the hosted pair request ----------------------------------------------------

    /// A pair request that opened with an invite's code and says `hosted`: ask the inviting phone (and the desktop).
    pub(super) fn hosted_ask(&self, ch: u64, e: &p256::PublicKey, phone: &p256::PublicKey, name: String, c: [u8; 16]) {
        let refuse = || self.send_clear(ch, json!({"t": "pair-refused"}));
        let p_pub = crypto::pub_b64u(phone);
        // A phone can't be both an owner's and hosted.
        if self.devices.lock().unwrap().by_pub(&p_pub).is_some() {
            return refuse();
        }
        let Some((house, house_name, by_dev, by_name, expires)) = self
            .invites
            .lock()
            .unwrap()
            .iter()
            .find(|i| i.c == c && !i.used)
            .map(|i| (i.house, i.house_name.clone(), i.by_dev.clone(), i.by_name.clone(), i.expires))
        else {
            return refuse();
        };
        let mut challenge = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut challenge);
        let ask = HostedAsk {
            ch,
            p_pub,
            c,
            by_dev: by_dev.clone(),
            challenge,
            id: hex::encode(&rand16()[..8]),
            device: Device::id_for(phone),
            name,
            code: crypto::pair_code(&self.d.public_key(), phone, e, &c),
            house,
            house_name,
            by: by_name,
            expires,
        };
        {
            let mut asks = self.hosted_asks.lock().unwrap();
            asks.retain(|a| a.ch != ch);
            // Two asks per invite at most, and a few in all: someone who saw one invite's QR can't block the others
            // (security review of v0.2.8, L4).
            if asks.len() >= MAX_ASKS || asks.iter().filter(|a| a.c == c).count() >= ASKS_PER_INVITE {
                drop(asks);
                return refuse();
            }
            asks.push(ask.clone());
        }
        crate::activity::note("hosted wallets: an invited phone asks to join");
        let mut view = serde_json::to_value(&ask).unwrap();
        view["hosted"] = json!(true);
        self.events.emit(EV_PAIR, view);
        // Every card for this invite again, each saying how many other phones opened it.
        let same: Vec<HostedAsk> = self.hosted_asks.lock().unwrap().iter().filter(|a| a.c == c).cloned().collect();
        for c in self.channels_of(&by_dev) {
            if self.may_hear(c, &by_dev) {
                for a in &same {
                    self.send_sealed(c, &self.allow_msg(a));
                }
            }
        }
    }

    fn allow_msg(&self, ask: &HostedAsk) -> Value {
        let others = self.hosted_asks.lock().unwrap().iter().filter(|a| a.c == ask.c && a.id != ask.id).count();
        json!({"allow": {
            "others": others,
            "id": ask.id, "name": ask.name, "code": ask.code, "house": ask.house, "house_name": ask.house_name,
            "challenge": crypto::b64u(&ask.challenge),
            "cred": self.passkey_of(&ask.by_dev).map(|k| k.cred),
            "secs": ask.expires.saturating_sub(self.now()),
        }})
    }

    /// A session of the inviting phone that proved Face ID hears the asks still waiting for it.
    pub(super) fn tell_allows(&self, ch: u64, dev: &str) {
        let asks: Vec<HostedAsk> = self.hosted_asks.lock().unwrap().iter().filter(|a| a.by_dev == dev).cloned().collect();
        for a in asks {
            self.send_sealed(ch, &self.allow_msg(&a));
        }
    }

    /// The hosted pair requests waiting, oldest first (the desktop's Settings).
    pub fn hosted_pending(&self) -> Vec<HostedAsk> {
        self.hosted_asks.lock().unwrap().clone()
    }

    /// `allow`: the inviting phone's answer, `{id, auth}` (Face ID over the ask's challenge) or `{id, decline: true}`.
    pub(super) fn allow_answer(self: &Arc<Self>, ch: u64, dev: &str, a: &Value) -> Result<Value, String> {
        let id = a["id"].as_str().ok_or("Which phone?")?;
        let (by_dev, challenge) = self
            .hosted_asks
            .lock()
            .unwrap()
            .iter()
            .find(|x| x.id == id)
            .map(|x| (x.by_dev.clone(), x.challenge))
            .ok_or("That phone is no longer waiting.")?;
        if by_dev != dev {
            return Err("Only the phone that made the invite can answer it.".into());
        }
        if a["decline"] == true {
            if !self.may_hear(ch, dev) {
                return Err(ERR_AUTH_NEEDED.into());
            }
            self.hosted_answer(id, false)?;
        } else {
            self.check_stored(dev, &challenge, &a["auth"])?;
            self.hosted_answer(id, true)?;
        }
        Ok(json!({}))
    }

    /// Allow or refuse a hosted pair request (the inviting phone, or the desktop). Allowing uses up the invite,
    /// refuses the other requests for it, lists the phone and starts making its wallet.
    pub fn hosted_answer(self: &Arc<Self>, id: &str, allow: bool) -> Result<(), String> {
        let ask = {
            let mut asks = self.hosted_asks.lock().unwrap();
            let i = asks.iter().position(|a| a.id == id).ok_or("That phone is no longer waiting.")?;
            asks.remove(i)
        };
        self.allow_done(&ask);
        let refuse = |a: &HostedAsk| {
            self.send_clear(a.ch, json!({"t": "pair-refused"}));
            self.note_idle(a.ch);
        };
        crate::activity::note(if allow { "hosted wallets: an invited phone allowed" } else { "hosted wallets: an invited phone refused" });
        if !allow {
            refuse(&ask);
            self.events.emit(EV_CHANGED, json!({}));
            return Ok(());
        }
        // A phone that is already the owner's, or already hosted, is refused before the invite is used up.
        if self.devices.lock().unwrap().by_pub(&ask.p_pub).is_some() || self.hosted.lock().unwrap().phones.iter().any(|h| h.id == ask.device) {
            refuse(&ask);
            self.events.emit(EV_CHANGED, json!({}));
            return Err("That phone is already paired here.".into());
        }
        let now = self.now();
        let live = {
            let mut inv = self.invites.lock().unwrap();
            match inv.iter_mut().find(|i| i.c == ask.c && !i.used && now < i.expires) {
                Some(i) => {
                    i.used = true;
                    true
                }
                None => false,
            }
        };
        if !live {
            refuse(&ask);
            self.events.emit(EV_CHANGED, json!({}));
            return Err("The invite has run out. Make a new one.".into());
        }
        let rest: Vec<HostedAsk> = {
            let mut asks = self.hosted_asks.lock().unwrap();
            let (gone, keep) = asks.drain(..).partition(|a| a.c == ask.c);
            *asks = keep;
            gone
        };
        for a in &rest {
            self.allow_done(a);
            refuse(a);
        }
        if self.devices.lock().unwrap().by_pub(&ask.p_pub).is_some() {
            refuse(&ask);
            return Err("That phone is already paired here as yours.".into());
        }
        let hp = HostedPhone {
            id: ask.device.clone(),
            name: ask.name.clone(),
            p_pub: ask.p_pub.clone(),
            added: now,
            house: ask.house,
            house_name: ask.house_name.clone(),
            by: ask.by.clone(),
            wallet: format!("hosted-{}", ask.device),
            step: Step::Setup,
            ..Default::default()
        };
        let saved = {
            let mut list = self.hosted.lock().unwrap();
            if list.phones.iter().any(|h| h.id == hp.id) {
                Err("That phone already has a wallet here.".to_string())
            } else {
                list.phones.push(hp.clone());
                self.store.save_hosted(&list)
            }
        };
        if let Err(e) = saved {
            refuse(&ask);
            return Err(e);
        }
        self.send_clear(ask.ch, json!({"t": "paired"}));
        self.note_idle(ask.ch);
        self.events.emit(EV_CHANGED, json!({}));
        let me = self.clone();
        tokio::spawn(async move { me.make_wallet(hp.id).await });
        Ok(())
    }

    fn allow_done(&self, ask: &HostedAsk) {
        for c in self.channels_of(&ask.by_dev) {
            self.send_sealed(c, &json!({"allow-done": ask.id}));
        }
    }

    /// Asks whose invite ran out are refused.
    pub(super) fn expire_hosted(&self) {
        let now = self.now();
        self.invites.lock().unwrap().retain(|i| now < i.expires);
        let gone: Vec<HostedAsk> = {
            let mut asks = self.hosted_asks.lock().unwrap();
            let (gone, keep) = asks.drain(..).partition(|a| now >= a.expires);
            *asks = keep;
            gone
        };
        for a in &gone {
            self.allow_done(a);
            self.send_clear(a.ch, json!({"t": "pair-refused"}));
            self.note_idle(a.ch);
        }
        if !gone.is_empty() {
            self.events.emit(EV_CHANGED, json!({}));
        }
    }

    /// A channel closed: its hosted pair request goes.
    pub(super) fn drop_hosted_asks_on(&self, ch: u64) -> bool {
        let mut asks = self.hosted_asks.lock().unwrap();
        let before = asks.len();
        asks.retain(|a| a.ch != ch);
        asks.len() != before
    }

    pub(super) fn hosted_ask_on(&self, ch: u64) -> bool {
        self.hosted_asks.lock().unwrap().iter().any(|a| a.ch == ch)
    }

    // ----- making the wallet, and joining the house --------------------------------------------

    /// After a restart of the app: finish what was under way.
    pub async fn resume_hosted(self: Arc<Self>) {
        let list = self.hosted.lock().unwrap().phones.clone();
        if !list.iter().any(|h| !h.remove && matches!(h.step, Step::Setup | Step::Joining | Step::Moving | Step::Moved)) {
            return;
        }
        // The node first: at login it isn't up yet, and a wallet made against no node would fail for good (security
        // review of v0.2.8, L7). Up to 30 minutes; in daemon mode asking wakes it.
        for _ in 0..360 {
            if self.rpc.call("getblockcount", vec![]).await.is_ok() {
                break;
            }
            tokio::time::sleep(if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(5) }).await;
        }
        for h in list.into_iter().filter(|h| !h.remove) {
            self.clone().continue_hosted(&h);
        }
    }

    /// Carry on with whatever a hosted wallet was doing.
    fn continue_hosted(self: Arc<Self>, h: &HostedPhone) {
        let id = h.id.clone();
        match h.step {
            Step::Setup => {
                tokio::spawn(async move { self.make_wallet(id).await });
            }
            Step::Joining => {
                tokio::spawn(async move { self.join(id).await });
            }
            Step::Moving | Step::Moved if h.move_to.is_some() => {
                tokio::spawn(async move { self.move_home(id).await });
            }
            _ => {}
        }
    }

    /// The desktop's "Try again" for a hosted wallet that failed: making it again (each part can run again), or
    /// joining again.
    pub fn hosted_retry(self: &Arc<Self>, hid: &str) -> Result<(), String> {
        let h = self.hosted_get(hid).filter(|h| !h.remove).ok_or(ERR_REMOVED)?;
        if h.step != Step::Failed {
            return Err("That wallet hasn't failed.".into());
        }
        let step = match (&h.member, &h.move_to) {
            (None, _) => Step::Setup,
            (Some(_), Some(_)) => Step::Moving,
            (Some(_), None) if self.key_of(hid).is_some_and(|k| k.entropy.is_some()) => Step::Words,
            (Some(_), None) => Step::Joining,
        };
        self.with_hosted(hid, |x| x.move_refusals = 0)?;
        self.set_step(hid, step, None);
        let h = self.hosted_get(hid).ok_or(ERR_REMOVED)?;
        self.clone().continue_hosted(&h);
        Ok(())
    }

    /// Make the hosted wallet: create and encrypt it (the node restarts), give it fresh words' key, read its first
    /// address. Each part can run again after an interruption.
    pub(crate) async fn make_wallet(self: Arc<Self>, hid: String) {
        let r = self.clone().make_wallet_steps(&hid).await;
        match r {
            Ok(()) => {
                crate::activity::note("hosted wallets: a wallet made");
                self.set_step(&hid, Step::Words, None);
            }
            Err(e) => {
                crate::activity::note("hosted wallets: making a wallet failed");
                self.set_step(&hid, Step::Failed, Some(e));
            }
        }
    }

    async fn make_wallet_steps(self: Arc<Self>, hid: &str) -> Result<(), String> {
        let maker = self.maker.lock().unwrap().clone().ok_or("This desktop can't make wallets for others here.")?;
        let wallet = self.hosted_get(hid).ok_or(ERR_REMOVED)?.wallet;
        // The passphrase and the words are kept before the wallet is made, so an interruption loses neither. A keys
        // file that can't be read stops here rather than a new key being written over one (the re-review, info).
        let existing = {
            let _one = KEYS.lock().unwrap();
            self.store.load_hosted_keys_checked()?.keys.into_iter().find(|k| k.id == hid)
        };
        let key = match existing {
            Some(k) if k.entropy.is_some() => k,
            Some(_) => return Err("This wallet's words were confirmed already; it can't be made again.".into()),
            None => {
                let k = HostedKey { id: hid.to_string(), pass: hex32(), entropy: Some(hex::encode(*seed::new_entropy())) };
                self.save_key(k.clone())?;
                k
            }
        };
        let pass = Zeroizing::new(key.pass.clone());
        let entropy: Zeroizing<[u8; 32]> = Zeroizing::new(
            hex::decode(key.entropy.as_deref().unwrap_or(""))
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or("The hosted wallet's words are damaged.")?,
        );
        maker.create_encrypted(&wallet, &pass).await?;
        let chain = Chain::from_name(self.rpc.call("getblockchaininfo", vec![]).await?["chain"].as_str().unwrap_or("main"))?;
        let hd = seed::freebank_hd_seed(&entropy)?;
        let want = seed::key_id_hex(&seed::key_id(&hd)?);
        let first = seed::address(&hd, false, 0)?;
        let info = self.rpc.call_in(&wallet, "getwalletinfo", vec![]).await?;
        if info["hdmasterkeyid"].as_str() != Some(want.as_str()) {
            let wif = seed::wif(&hd, chain);
            self.unlock_hosted(&wallet, &pass, 60).await.map_err(|e| e.plain())?;
            let r = self.rpc.call_in(&wallet, "sethdseed", vec![json!(true), json!(wif.as_str())]).await;
            let _ = self.rpc.call_in(&wallet, "walletlock", vec![]).await;
            r.map_err(|e| format!("The new wallet didn't take its key: {}", e.plain()))?;
            let now = self.rpc.call_in(&wallet, "getwalletinfo", vec![]).await?;
            if now["hdmasterkeyid"].as_str() != Some(want.as_str()) {
                return Err("The new wallet didn't take the key from its words.".into());
            }
        }
        // Its first address, the words' first: the member address. Asked whether it's the wallet's, not given out by
        // getnewaddress, so a step run again after an interruption finds the same one (security review of v0.2.8, L7).
        let info = self.rpc.call_in(&wallet, "getaddressinfo", vec![json!(first)]).await?;
        if info["ismine"] != true {
            return Err("The new wallet doesn't hold its words' first address.".into());
        }
        self.with_hosted(hid, |h| h.member = Some(first))?;
        Ok(())
    }

    /// Unlock a hosted wallet for `secs`, clear of the node's relock (`crate::wallet::RelockGuard`).
    async fn unlock_hosted(&self, wallet: &str, pass: &str, secs: u64) -> Result<(), RpcFail> {
        let guard = self.relock.lock().unwrap().clone();
        guard
            .run(
                secs,
                || self.rpc.call_in(wallet, "walletpassphrase", vec![json!(pass), json!(secs)]),
                |e: &RpcFail| e.maybe,
            )
            .await
            .map(|_| ())
    }

    /// Whether `address` is an active member of `house` (`listhousemembers` from that address, one entry: big houses
    /// list in pages; security review of v0.2.8, L2).
    async fn member_active(&self, house: u64, address: &str) -> bool {
        match self.rpc.call("listhousemembers", vec![json!(house), json!(address), json!(1)]).await {
            Ok(list) => list.as_array().and_then(|l| l.first()).is_some_and(|m| m["address"] == address && m["active"] == true),
            Err(_) => false,
        }
    }

    /// Add the member address to the house, once (with the fee float, once), and wait until the member list shows it
    /// active. Signed by this desktop's wallet with the phone-send passphrase; without it, it waits.
    pub(super) async fn join(self: Arc<Self>, hid: String) {
        let mut wait = JOIN_RETRY_SECS;
        loop {
            let Some(h) = self.hosted_get(&hid) else { return };
            if h.step != Step::Joining || h.remove {
                return;
            }
            let Some(member) = h.member.clone() else {
                return self.set_step(&hid, Step::Failed, Some("The wallet has no member address.".into()));
            };
            if self.member_active(h.house, &member).await {
                crate::activity::note("hosted wallets: a wallet joined its house");
                return self.set_step(&hid, Step::Ready, None);
            }
            // Sent long ago and still not active: send it again.
            let stale = h.join_sent_at.is_some_and(|t| self.now() > t + 900);
            if h.join_txid.is_none() || stale {
                let r = self.add_member(h.house, &member, h.float_txid.is_none()).await;
                wait = self.after_member_change(&hid, r, |x, txid, now| {
                    x.join_txid = Some(txid);
                    x.join_sent_at = Some(now);
                });
            }
            tokio::time::sleep(if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(wait) }).await;
        }
    }

    /// What a member change's answer means for the hosted wallet: its record, what its phone hears, and how long to wait
    /// before looking again. The node's own words go to this desktop's log only, not to the hosted phone (security
    /// review of v0.2.8, info).
    fn after_member_change(&self, hid: &str, r: Result<(String, Option<String>), Pay>, sent: impl FnOnce(&mut HostedPhone, String, u64)) -> u64 {
        let now = self.now();
        let wait = match r {
            Ok((txid, float)) => {
                let _ = self.with_hosted(hid, |x| {
                    sent(x, txid, now);
                    if float.is_some() {
                        x.float_txid = float;
                    }
                    x.why = None;
                });
                JOIN_RETRY_SECS
            }
            Err(Pay::Locked | Pay::WrongPassphrase) => {
                let _ = self.with_hosted(hid, |x| x.why = Some("Waiting for the house's desktop.".into()));
                JOIN_RETRY_SECS
            }
            Err(Pay::MayHaveGone) => {
                let _ = self.with_hosted(hid, |x| sent(x, "unknown".into(), now));
                JOIN_RETRY_SECS
            }
            Err(Pay::NotTried(e) | Pay::Failed(e)) => {
                crate::activity::note(&format!("hosted wallets: a member change: {}", crate::activity::mask_numbers(&e)));
                let _ = self.with_hosted(hid, |x| x.why = Some("Waiting for the house's desktop.".into()));
                // The node's "one member change per house per block" passes with the next block; anything else waits
                // longer before it is tried again.
                if e.contains("member op") || e.contains("next block") { JOIN_RETRY_SECS } else { 300 }
            }
        };
        self.events.emit(EV_CHANGED, json!({}));
        wait
    }

    /// One member change for `member` at `house`, signed by this desktop's wallet, and with `float` the fee float to
    /// it in the same unlock (`FEE_FLOAT`). The change's txid and the float's.
    async fn add_member(&self, house: u64, member: &str, float: bool) -> Result<(String, Option<String>), Pay> {
        self.member_op("addhousemembers", house, member, float).await
    }

    /// One member change (`addhousemembers` or `removehousemembers`) for `member` at `house`, signed by this desktop's
    /// wallet; with `float`, the fee float to it in the same unlock.
    async fn member_op(&self, method: &str, house: u64, member: &str, float: bool) -> Result<(String, Option<String>), Pay> {
        let _gate = self.wallet_gate.lock().await;
        let unlocked = self.unlock_for_send(None).await?;
        let r = self.rpc.call(method, vec![json!(house), json!([member]), json!(MEMBER_FEE)]).await;
        let mut float_txid = None;
        if r.is_ok() && float {
            match self.rpc.call("sendtoaddress", vec![json!(member), json!(FEE_FLOAT)]).await {
                Ok(v) => float_txid = Some(v.as_str().unwrap_or("unknown").to_string()),
                Err(e) if e.maybe => float_txid = Some("unknown".to_string()),
                Err(_) => crate::activity::note("hosted wallets: the fee float couldn't be sent"),
            }
        }
        if unlocked {
            self.lock_wallet().await;
        }
        match r {
            Ok(v) => Ok((v["txid"].as_str().map(String::from).unwrap_or_else(|| "unknown".into()), float_txid)),
            Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) => Err(Pay::Locked),
            Err(e) if e.maybe => Err(Pay::MayHaveGone),
            Err(e) => Err(Pay::Failed(e.plain())),
        }
    }

    /// Moving home (fresh words; Michael, 2026-10-05): add the shopkeeper's own computer's address to the house, then
    /// send it each house's notes (or redeem those it can't take), one at a time, each confirmed before the next, then
    /// the rest of the sECX. Then `moved`, and this keeps watching the old copy: money that reaches it later (a customer
    /// using the shop's old QR code) is sent on, and the old address is taken off the house (the re-review of v0.2.8,
    /// N2). One loop per hosted wallet.
    pub(super) async fn move_home(self: Arc<Self>, hid: String) {
        if !MOVING.lock().unwrap().get_or_insert_with(Default::default).insert(hid.clone()) {
            return;
        }
        loop {
            let Some(h) = self.hosted_get(&hid) else { break };
            if h.remove {
                break;
            }
            let wait = match h.step {
                Step::Moving => self.move_once(&hid, &h).await,
                Step::Moved => self.watch_moved(&hid, &h).await,
                _ => break,
            };
            tokio::time::sleep(if cfg!(test) { Duration::from_millis(50) } else { Duration::from_secs(wait) }).await;
        }
        MOVING.lock().unwrap().get_or_insert_with(Default::default).remove(&hid);
    }

    /// One turn of the move: the next step, or `moved` when nothing that can move is left. How long to wait next.
    async fn move_once(&self, hid: &str, h: &HostedPhone) -> u64 {
        let Some(to) = h.move_to.clone() else {
            self.set_step(hid, Step::Failed, Some("No address to move to.".into()));
            return JOIN_RETRY_SECS;
        };
        match self.move_step(hid, h, &to).await {
            Ok(true) => {
                let now = self.now();
                let skipped = !h.move_skip.is_empty();
                let _ = self.with_hosted(hid, |x| {
                    x.step = Step::Moved;
                    x.moved_at.get_or_insert(now);
                    x.empty = !skipped;
                    x.why = skipped.then(|| "Some notes couldn't be moved: they stay in this copy; the shop's old words reach them.".into());
                    x.move_refusals = 0;
                });
                crate::activity::note("hosted wallets: a hosted wallet moved home");
                self.events.emit(EV_HOSTED_MOVED, json!({"id": hid, "name": h.name}));
                self.events.emit(EV_CHANGED, json!({}));
                JOIN_RETRY_SECS
            }
            Ok(false) => JOIN_RETRY_SECS,
            Err(secs) => secs,
        }
    }

    /// After the move: take the old member address off the house, once, and send on anything that arrives later.
    async fn watch_moved(&self, hid: &str, h: &HostedPhone) -> u64 {
        if h.old_removed.is_none() {
            if let Some(old) = h.member.clone() {
                if let Ok((txid, _)) = self.member_op("removehousemembers", h.house, &old, false).await {
                    let _ = self.with_hosted(hid, |x| x.old_removed = Some(txid));
                }
            }
        }
        if h.move_to.is_some() && self.holds_anything(&h.wallet, &h.move_skip).await == Some(true) {
            let _ = self.with_hosted(hid, |x| {
                x.step = Step::Moving;
                x.empty = false;
                x.why = Some("Money reached the old address after the move: sending it on.".into());
            });
            self.events.emit(EV_CHANGED, json!({}));
            return JOIN_RETRY_SECS;
        }
        MOVED_WATCH_SECS
    }

    /// Whether the hosted wallet holds anything that could move: sECX over dust (confirmed or not), or notes of a house
    /// not given up on. None when the node doesn't answer.
    async fn holds_anything(&self, w: &str, skip: &[u64]) -> Option<bool> {
        let balance = self.rpc.call_in(w, "getbalance", vec![]).await.ok()?.as_f64()?;
        let pending = self.rpc.call_in(w, "getunconfirmedbalance", vec![]).await.ok()?.as_f64()?;
        let notes = self.rpc.call_in(w, "listmynotes", vec![]).await.ok()?;
        let any_notes = notes.as_array().is_some_and(|l| {
            l.iter().any(|n| n["units"].as_u64().unwrap_or(0) > 0 && !skip.contains(&n["house_id"].as_u64().unwrap_or(0)))
        });
        Some(any_notes || balance + pending > MOVE_DUST)
    }

    /// One step of moving home. Ok(true): nothing that can move is left, confirmed. Ok(false): something is on its
    /// way. Err(secs): wait that long.
    async fn move_step(&self, hid: &str, h: &HostedPhone, to: &str) -> Result<bool, u64> {
        let w = h.wallet.as_str();
        let why = |t: &str| {
            let _ = self.with_hosted(hid, |x| x.why = Some(t.to_string()));
        };
        // 1. His address joins the house.
        if !self.member_active(h.house, to).await {
            let stale = h.move_member_at.is_some_and(|t| self.now() > t + 900);
            if h.move_member_txid.is_none() || stale {
                let r = self.member_op("addhousemembers", h.house, to, false).await;
                return Err(self.after_member_change(hid, r, |x, txid, now| {
                    x.move_member_txid = Some(txid);
                    x.move_member_at = Some(now);
                }));
            }
            why("Adding your computer's address to the house.");
            return Ok(false);
        }
        // 2. The last transfer confirms before the next one goes.
        if let Some(t) = h.move_txid.as_deref().filter(|t| *t != "unknown") {
            let confirmed = self.rpc.call_in(w, "gettransaction", vec![json!(t)]).await.ok().and_then(|v| v["confirmations"].as_i64());
            if confirmed.is_none_or(|c| c < 1) {
                why("Waiting for the last transfer to confirm.");
                return Ok(false);
            }
        } else if h.move_txid.is_some() && h.move_at.is_some_and(|t| self.now() < t + 600) {
            why("Waiting for the last transfer to confirm.");
            return Ok(false);
        }
        let pending = self.rpc.call_in(w, "getunconfirmedbalance", vec![]).await.ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
        let balance = self.rpc.call_in(w, "getbalance", vec![]).await.ok().and_then(|v| v.as_f64()).ok_or(JOIN_RETRY_SECS)?;
        if pending > 0.0 {
            why("Waiting for a payment to confirm.");
            return Ok(false);
        }
        // 3. Each house's notes, one house at a time: sent where his address may hold them (this house, open houses,
        //    another members-only house he belongs to), redeemed for sECX where it may not (a redeem-only house, or one
        //    he isn't a member of), and left here, said, when neither works (the re-review of v0.2.8, N1).
        let notes = self.rpc.call_in(w, "listmynotes", vec![]).await.map_err(|_| JOIN_RETRY_SECS)?;
        let houses = self.rpc.call("listhouses", vec![]).await.unwrap_or(Value::Null);
        let kind_of = |id: u64| {
            let h = houses.as_array().and_then(|l| l.iter().find(|x| x["id"].as_u64() == Some(id)));
            house_type(h)
        };
        let next = notes.as_array().map(|v| v.as_slice()).unwrap_or(&[]).iter().find(|n| {
            n["units"].as_u64().unwrap_or(0) > 0 && !h.move_skip.contains(&n["house_id"].as_u64().unwrap_or(0))
        });
        if let Some(n) = next {
            let house = n["house_id"].as_u64().unwrap_or(0);
            let units = n["units"].as_u64().unwrap_or(0);
            let redeemable = n["redeemable_units"].as_u64().unwrap_or(if n["redeemable"] == true { units } else { 0 });
            let send = match kind_of(house) {
                "open" => true,
                "members" => house == h.house || self.member_active(house, to).await,
                _ => false,
            };
            if !send && redeemable < units {
                let _ = self.with_hosted(hid, |x| {
                    x.move_skip.push(house);
                    x.why = Some(format!("House #{house}'s notes can't be moved or redeemed now: they stay in this copy."));
                });
                return Ok(false);
            }
            // The fee first: a note payment needs some sECX, which the house tops up a few times at most.
            if balance < MOVE_FEE_NEED {
                return self.top_up(hid, h).await;
            }
            if send {
                why("Sending your notes to your computer.");
                return self.move_send(hid, h, "transfernote", vec![json!(house), json!(units), json!(NOTE_FEE), json!(to)]).await;
            }
            why("Redeeming notes your computer can't hold; their sECX follows.");
            return self.move_send(hid, h, "redeemnote", vec![json!(house), json!(units), json!(NOTE_FEE)]).await;
        }
        // 4. Then the sECX: dust counts as nothing.
        if balance > MOVE_DUST {
            why("Sending your sECX to your computer.");
            return self.move_send(hid, h, "sendtoaddress", vec![json!(to), json!(balance), json!(""), json!(""), json!(true)]).await;
        }
        Ok(true)
    }

    /// The house sends the hosted wallet a fee float again, for the move's next note payment; a few times at most.
    async fn top_up(&self, hid: &str, h: &HostedPhone) -> Result<bool, u64> {
        if h.move_topups >= MOVE_TOPUPS {
            self.set_step(hid, Step::Failed, Some("There's no sECX left for the move's fees.".into()));
            return Err(JOIN_RETRY_SECS);
        }
        let member = h.member.clone().ok_or(300u64)?;
        let r = {
            let _gate = self.wallet_gate.lock().await;
            let unlocked = self.unlock_for_send(None).await.map_err(|_| JOIN_RETRY_SECS)?;
            let r = self.rpc.call("sendtoaddress", vec![json!(member), json!(FEE_FLOAT)]).await;
            if unlocked {
                self.lock_wallet().await;
            }
            r
        };
        let now = self.now();
        match r {
            Ok(v) => {
                let txid = v.as_str().unwrap_or("unknown").to_string();
                let _ = self.with_hosted(hid, |x| {
                    x.move_topups += 1;
                    x.move_txid = Some(txid);
                    x.move_at = Some(now);
                    x.why = Some("The house is adding sECX for the move's fees.".into());
                });
                Ok(false)
            }
            Err(e) if e.maybe => {
                let _ = self.with_hosted(hid, |x| {
                    x.move_topups += 1;
                    x.move_txid = Some("unknown".into());
                    x.move_at = Some(now);
                });
                Ok(false)
            }
            Err(_) => Err(300),
        }
    }

    /// One transfer from the hosted wallet while moving home, unlocked with its passphrase for the moment. Refusals in
    /// a row stop the move after a few (`MOVE_REFUSALS`), so it can't loop for ever; the phone can also stop it.
    async fn move_send(&self, hid: &str, h: &HostedPhone, method: &str, params: Vec<Value>) -> Result<bool, u64> {
        let w = h.wallet.as_str();
        let pass = self.key_of(hid).map(|k| Zeroizing::new(k.pass)).ok_or(300u64)?;
        let r = {
            let _gate = self.wallet_gate.lock().await;
            self.unlock_hosted(w, &pass, HOSTED_UNLOCK_SECS).await.map_err(|_| JOIN_RETRY_SECS)?;
            let r = self.rpc.call_in(w, method, params).await;
            let _ = self.rpc.call_in(w, "walletlock", vec![]).await;
            r
        };
        let now = self.now();
        let txid = match r {
            Ok(v) => v.get("txid").cloned().unwrap_or(v).as_str().unwrap_or("unknown").to_string(),
            Err(e) if e.maybe => "unknown".to_string(),
            Err(e) => {
                crate::activity::note(&format!("hosted wallets: moving home: {}", crate::activity::mask_numbers(&e.plain())));
                let stop = h.move_refusals + 1 >= MOVE_REFUSALS;
                let _ = self.with_hosted(hid, |x| {
                    x.move_refusals += 1;
                    x.why = Some(format!("A transfer to your computer was refused: {}", e.plain()));
                    if stop {
                        x.step = Step::Failed;
                    }
                });
                self.events.emit(EV_CHANGED, json!({}));
                return Err(300);
            }
        };
        let _ = self.with_hosted(hid, |x| {
            x.move_txid = Some(txid);
            x.move_at = Some(now);
            x.move_refusals = 0;
        });
        Ok(false)
    }

    /// Before deleting a copy the move emptied, look again: money that arrived since is sent on first (N2). Ok(true):
    /// it holds nothing and can go; Ok(false): it was never emptied (it moves aside instead).
    pub async fn hosted_empty_now(self: &Arc<Self>, hid: &str) -> Result<bool, String> {
        let h = self.hosted_get(hid).ok_or(ERR_REMOVED)?;
        if !h.empty {
            return Ok(false);
        }
        match self.holds_anything(&h.wallet, &[]).await {
            // Deleted outright only once the old address is off the house too: until then customers' old QR codes can
            // still pay it, so the copy moves aside instead, passphrase included (the final re-review, N2).
            Some(false) => {
                let old_active = match &h.member {
                    Some(m) => h.old_removed.is_none() || self.member_active(h.house, m).await,
                    None => false,
                };
                if old_active {
                    self.with_hosted(hid, |x| x.empty = false)?;
                    return Ok(false);
                }
                Ok(true)
            }
            Some(true) => {
                self.with_hosted(hid, |x| {
                    x.empty = false;
                    x.step = Step::Moving;
                    x.why = Some("Money reached the old address after the move: sending it on.".into());
                })?;
                let me = self.clone();
                let id = hid.to_string();
                tokio::spawn(async move { me.move_home(id).await });
                Err("Money reached this copy after the move. It's being sent on to their computer; delete the copy once that's done.".into())
            }
            None => Err("The node isn't answering, so the copy can't be checked. Try again once it runs.".into()),
        }
    }

    // ----- a hosted phone's door ---------------------------------------------------------------

    pub(super) async fn serve_hosted(self: &Arc<Self>, ch: u64, dev: &str, hid: &str, req: &Value) -> Result<Value, String> {
        let a = &req["a"];
        let m = req["m"].as_str().unwrap_or("");
        let h = self.hosted_get(hid).filter(|h| !h.remove).ok_or(ERR_REMOVED)?;
        // A passkey first; then the Face ID gate, as for an owner's phone.
        match m {
            "auth-start" => {
                if h.passkey.is_none() && a["for"] != "add" {
                    return Err(ERR_HOSTED_FACE_ID.into());
                }
                return self.auth_start(ch, dev, a);
            }
            "auth" => return self.auth_open(ch, dev, a),
            "passkey-add" => return self.passkey_add(ch, dev, a),
            "status" if h.passkey.is_none() => return self.hosted_status(&h).await,
            _ if h.passkey.is_none() => return Err(ERR_HOSTED_FACE_ID.into()),
            _ if !self.verified(ch, dev) => return Err(ERR_AUTH_NEEDED.into()),
            _ => {}
        }
        if h.step == Step::Moved && m != "status" {
            return Err(ERR_MOVED.into());
        }
        self.wake();
        if matches!(m, "balance" | "history" | "notes" | "houses" | "send" | "note-send" | "note-redeem" | "words-ok" | "move-home") {
            if self.starting.load(Ordering::SeqCst) {
                return Err(ERR_STARTING.into());
            }
            if let Some(why) = self.wake_failure.lock().unwrap().clone() {
                return Err(why);
            }
        }
        let w = h.wallet.as_str();
        match m {
            "status" => self.hosted_status(&h).await,
            "words" => {
                if h.step != Step::Words {
                    return Err("The words were shown already.".into());
                }
                let key = self.key_of(hid).and_then(|k| k.entropy).ok_or("The words were shown already.")?;
                let entropy: Zeroizing<[u8; 32]> = Zeroizing::new(
                    hex::decode(&key).ok().and_then(|b| b.try_into().ok()).ok_or("The hosted wallet's words are damaged.")?,
                );
                let words = seed::words(&entropy);
                Ok(json!({"words": *words}))
            }
            "words-ok" => {
                if h.step != Step::Words {
                    return Err("The words were confirmed already.".into());
                }
                self.check_assertion(ch, dev, "change", &a["auth"], None)?;
                // The words are his now: this desktop keeps only the wallet and its passphrase.
                if let Some(mut k) = self.key_of(hid) {
                    k.entropy = None;
                    self.save_key(k)?;
                }
                self.set_step(hid, Step::Joining, None);
                let me = self.clone();
                let id = hid.to_string();
                tokio::spawn(async move { me.join(id).await });
                Ok(json!({}))
            }
            "balance" => {
                let confirmed = self.rpc.call_in(w, "getbalance", vec![]).await?.as_f64().ok_or("bad balance")?;
                let pending = self.rpc.call_in(w, "getunconfirmedbalance", vec![]).await.ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
                Ok(json!({"confirmed": confirmed, "pending": pending}))
            }
            "history" => {
                let count = a["count"].as_u64().unwrap_or(20).clamp(1, HISTORY_MAX);
                let r = self.rpc.call_in(w, "listtransactions", vec![json!("*"), json!(count)]).await?;
                Ok(history_view(&r))
            }
            "notes" => {
                let mine = match self.rpc.call_in(w, "listmynotes", vec![]).await {
                    Ok(v) => v,
                    // A node before v0.2.19 lists notes only unlocked.
                    Err(e) if e.code == Some(RPC_WALLET_UNLOCK_NEEDED) => {
                        let pass = self.key_of(hid).map(|k| Zeroizing::new(k.pass)).ok_or("The hosted wallet's passphrase is missing.")?;
                        self.unlock_hosted(w, &pass, HOSTED_UNLOCK_SECS).await.map_err(|e| e.plain())?;
                        let r = self.rpc.call_in(w, "listmynotes", vec![]).await;
                        let _ = self.rpc.call_in(w, "walletlock", vec![]).await;
                        r?
                    }
                    Err(e) => return Err(e.plain()),
                };
                let houses = self.rpc.call("listhouses", vec![]).await.unwrap_or(Value::Null);
                Ok(notes_view(&mine, &houses))
            }
            "houses" => {
                let r = self.rpc.call("listhouses", vec![]).await?;
                Ok(Value::Array(r.as_array().map(|v| v.as_slice()).unwrap_or(&[]).iter().map(house_view).collect()))
            }
            "receive" => {
                let member = h.member.clone().ok_or("The wallet isn't ready yet.")?;
                Ok(json!({"address": member}))
            }
            "send" | "note-send" | "note-redeem" => {
                if h.step != Step::Ready {
                    return Err("The wallet isn't ready yet.".into());
                }
                self.check_assertion(ch, dev, "send", &a["auth"], None)?;
                // One payment every few seconds per hosted phone (security review of v0.2.8, L8).
                {
                    let now = self.now();
                    let mut last = LAST_PAY.lock().unwrap();
                    let map = last.get_or_insert_with(HashMap::new);
                    if map.get(hid).is_some_and(|&t| now < t + PAY_GAP_SECS) {
                        return Err("One payment at a time: try again in a few seconds.".into());
                    }
                    map.insert(hid.to_string(), now);
                }
                let kind = Kind::from_method(m).expect("one of the three");
                let sats = store::json_to_sats(&a["amount"])?;
                if sats == 0 {
                    return Err("amount must be more than zero".into());
                }
                let house = if kind == Kind::Send { None } else { Some(a["house"].as_u64().ok_or("Which house's notes?")?) };
                let address = if kind == Kind::NoteRedeem {
                    String::new()
                } else {
                    let address = a["address"].as_str().unwrap_or("").trim().to_string();
                    if !plausible_address(&address) {
                        return Err("That isn't a FreeBank address (it starts with X).".into());
                    }
                    if self.rpc.call("validateaddress", vec![json!(address)]).await?["isvalid"] != true {
                        return Err("That isn't a valid FreeBank address.".into());
                    }
                    address
                };
                let r = self.hosted_pay(hid, w, kind, house, &address, sats).await;
                let what = match kind {
                    Kind::Send => "send",
                    Kind::NoteSend => "note-send",
                    _ => "note-redeem",
                };
                let entry = |result: &str, detail: Value| {
                    json!({"time": self.now(), "hosted": hid, "name": h.name, "kind": what, "house": house,
                           "address": address, "amount": to_ecx(sats), "result": result, "detail": detail})
                };
                match &r {
                    Ok(v) => self.write_log(entry("sent", json!(v))),
                    Err(e) => self.write_log(entry(if e == ERR_MAY_HAVE_GONE { "unknown" } else { "failed" }, json!(e))),
                }
                r.map(|txid| json!({"txid": txid}))
            }
            "move-home" => {
                if h.step != Step::Ready {
                    return Err("The wallet isn't ready to move.".into());
                }
                self.check_assertion(ch, dev, "change", &a["auth"], None)?;
                let to = a["address"].as_str().unwrap_or("").trim().to_string();
                if !plausible_address(&to) || self.rpc.call("validateaddress", vec![json!(to)]).await?["isvalid"] != true {
                    return Err("That isn't a FreeBank address.".into());
                }
                // His own computer's, not this wallet's, nor one of the house's own keys (the re-review of v0.2.8, N4).
                if self.rpc.call_in(w, "getaddressinfo", vec![json!(to)]).await.is_ok_and(|i| i["ismine"] == true) {
                    return Err("That address is this hosted wallet's own: use your computer's.".into());
                }
                if self.rpc.call("getaddressinfo", vec![json!(to)]).await.is_ok_and(|i| i["ismine"] == true) {
                    return Err("That address belongs to the house's own wallet: use your computer's.".into());
                }
                // One destination per hosted wallet: a later move-home must name the same, so a hosted phone can't admit
                // a string of addresses to the owner's house by moving, stopping and moving again (the final
                // re-review of v0.2.8, N4).
                if let Some(prev) = h.move_to.as_deref().filter(|p| *p != to) {
                    return Err(format!("This wallet is moving to {prev} already. To move it elsewhere, ask the house."));
                }
                self.with_hosted(hid, |x| {
                    x.step = Step::Moving;
                    x.move_to = Some(to.clone());
                    x.why = None;
                })?;
                crate::activity::note("hosted wallets: moving a hosted wallet home");
                // The owner sees who is moving, and the new member address the house is about to add.
                self.events.emit(EV_HOSTED_MOVING, json!({"id": hid, "name": h.name, "address": to, "house_name": h.house_name}));
                self.events.emit(EV_CHANGED, json!({}));
                let me = self.clone();
                let id = hid.to_string();
                tokio::spawn(async move { me.move_home(id).await });
                Ok(json!({}))
            }
            // Stop a move that can't finish, back to an ordinary hosted wallet (N1).
            "move-stop" => {
                if h.step != Step::Moving && h.step != Step::Failed {
                    return Err("No move is under way.".into());
                }
                self.check_assertion(ch, dev, "change", &a["auth"], None)?;
                self.with_hosted(hid, |x| {
                    x.step = Step::Ready;
                    x.why = None;
                    x.move_refusals = 0;
                })?;
                crate::activity::note("hosted wallets: a move home stopped");
                self.events.emit(EV_CHANGED, json!({}));
                Ok(json!({}))
            }
            _ => Err("unknown method".into()),
        }
    }

    async fn hosted_status(&self, h: &HostedPhone) -> Result<Value, String> {
        let r = self.rpc.call("getblockchaininfo", vec![]).await.unwrap_or(Value::Null);
        let synced = match r["initialblockdownload"].as_bool() {
            Some(ibd) => !ibd,
            None => !r.is_null() && r["blocks"] == r["headers"],
        };
        Ok(json!({
            "hosted": true, "house": h.house, "house_name": h.house_name, "step": h.step, "why": h.why, "move_to": h.move_to,
            "blocks": r["blocks"], "synced": synced, "face_id": h.passkey.is_some(), "credit": true,
        }))
    }

    /// One payment from a hosted wallet, unlocked with its passphrase for the moment. An answer that doesn't come
    /// back says it may have gone out (as for every payment since v0.2.7).
    async fn hosted_pay(&self, hid: &str, w: &str, kind: Kind, house: Option<u64>, address: &str, sats: u64) -> Result<String, String> {
        let pass = self.key_of(hid).map(|k| Zeroizing::new(k.pass)).ok_or("The hosted wallet's passphrase is missing.")?;
        let _gate = self.wallet_gate.lock().await;
        self.unlock_hosted(w, &pass, HOSTED_UNLOCK_SECS).await.map_err(|e| e.plain())?;
        let r = match kind {
            Kind::Send => self.rpc.call_in(w, "sendtoaddress", vec![json!(address), json!(to_ecx(sats))]).await,
            Kind::NoteSend => self.rpc.call_in(w, "transfernote", vec![json!(house), json!(sats), json!(NOTE_FEE), json!(address)]).await,
            _ => self.rpc.call_in(w, "redeemnote", vec![json!(house), json!(sats), json!(NOTE_FEE)]).await,
        };
        let _ = self.rpc.call_in(w, "walletlock", vec![]).await;
        match r {
            Ok(v) => Ok(v.get("txid").cloned().unwrap_or(v).as_str().unwrap_or("").to_string()),
            Err(e) if e.maybe => Err(ERR_MAY_HAVE_GONE.into()),
            Err(e) if e.message.contains(NO_FEE_MONEY) => Err(ERR_NO_FEE_MONEY.into()),
            Err(e) => Err(e.plain()),
        }
    }
}

/// The wallet's member addresses (Michael, 2026-10-05: "yes to both"): one per members-only or redeem-only house where
/// an address of this wallet is an active member, `[{house, name, address}]`. A members-only house's notes go only to
/// its members, and Receive makes a new address each time, which wouldn't be one. `call` reaches the wallet.
pub async fn member_addresses<F, Fut>(call: F) -> Vec<Value>
where
    F: Fn(&'static str, Vec<Value>) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let Ok(houses) = call("listhouses", vec![]).await else { return Vec::new() };
    let houses: Vec<(u64, Value)> = houses
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|h| house_type(Some(h)) != "open")
        .filter_map(|h| Some((h["id"].as_u64()?, h["classid"].clone())))
        .collect();
    if houses.is_empty() {
        return Vec::new();
    }
    let Ok(received) = call("listreceivedbyaddress", vec![json!(0), json!(true)]).await else { return Vec::new() };
    let mine: std::collections::HashSet<String> = received
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|r| r["address"].as_str().map(String::from))
        .collect();
    let mut out = Vec::new();
    'houses: for (id, name) in houses {
        // The member list in pages of 1000, each from the last address seen (inclusive).
        let mut start = String::new();
        for _ in 0..100 {
            let Ok(page) = call("listhousemembers", vec![json!(id), json!(start), json!(1000)]).await else { continue 'houses };
            let page = page.as_array().cloned().unwrap_or_default();
            for m in &page {
                let a = m["address"].as_str().unwrap_or("");
                if m["active"] == true && mine.contains(a) {
                    out.push(json!({"house": id, "name": name, "address": a}));
                    continue 'houses;
                }
            }
            match page.last().and_then(|m| m["address"].as_str()) {
                Some(last) if page.len() >= 1000 && last != start => start = last.to_string(),
                _ => continue 'houses,
            }
        }
    }
    out
}

/// At the node's start, the hosted copies the owner deleted (security review of v0.2.8, M1 and L3):
/// - a copy whose move home emptied it is deleted outright, with its passphrase;
/// - any other moves into `<app data>/hosted-removed/` (owner-only) with its passphrase beside it, since money may
///   still be in it.
///
/// The file is looked for wherever the node keeps wallets (`walletdir=` included). One that isn't found, or can't be
/// moved, stays on the list with why, and is tried again at the next start.
pub fn sweep_removed(app_dir: &Path, datadir: &Path) {
    let store = store::Store::new(app_dir);
    let mut list = store.load_hosted();
    if !list.phones.iter().any(|h| h.remove) {
        return;
    }
    let _one = KEYS.lock().unwrap();
    let Ok(mut keys) = store.load_hosted_keys_checked() else {
        crate::activity::note("hosted wallets: the keys file can't be read; deleted copies wait");
        return;
    };
    let aside = app_dir.join("hosted-removed");
    let stamp = unix_now();
    let mut kept = Vec::new();
    for mut h in list.phones.drain(..) {
        if !h.remove {
            kept.push(h);
            continue;
        }
        let Some(from) = crate::node::wallet_dirs(datadir).into_iter().map(|d| d.join(&h.wallet)).find(|f| f.exists()) else {
            h.why = Some("Its wallet file wasn't found where the node keeps wallets.".into());
            kept.push(h);
            continue;
        };
        let done = if h.empty {
            remove_any(&from).map(|_| "hosted wallets: an emptied hosted copy deleted").map_err(|e| e.to_string())
        } else {
            move_aside(&from, &aside, &format!("{}-{stamp}", h.wallet)).map(|to| {
                if let Some(k) = keys.keys.iter().find(|k| k.id == h.id) {
                    if let Ok(mut f) = crate::node::install::private_file(&to.with_extension("passphrase")) {
                        use std::io::Write;
                        let _ = writeln!(f, "{}", k.pass);
                    }
                }
                "hosted wallets: a deleted hosted copy moved aside"
            })
        };
        match done {
            Ok(note) => {
                keys.keys.retain(|k| k.id != h.id);
                crate::activity::note(note);
            }
            Err(e) => {
                h.why = Some(format!("Its wallet file couldn't be moved: {e}"));
                kept.push(h);
            }
        }
    }
    list.phones = kept;
    let _ = store.save_hosted(&list);
    let _ = store.save_hosted_keys(&keys);
}

/// Remove a wallet file (or folder).
fn remove_any(p: &Path) -> std::io::Result<()> {
    if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) }
}

/// Move a wallet file into `dir` (made owner-only) as `name`: a rename, or across disks a copy and then the removal.
fn move_aside(from: &Path, dir: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let to = dir.join(name);
    if std::fs::rename(from, &to).is_ok() {
        return Ok(to);
    }
    if from.is_dir() {
        return Err("a folder on another disk".into());
    }
    std::fs::copy(from, &to).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::remove_file(from).map_err(|e| e.to_string())?;
    Ok(to)
}
