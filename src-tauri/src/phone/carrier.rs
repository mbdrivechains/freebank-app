//! The phone link over Nostr (v0.6.0; PROTOCOL.md, "Over Nostr", in mbdrivechains/freebank-phone): the protocol's
//! frames, carried in Nostr events between this desktop's Nostr key and each phone's, through public relays. Nobody
//! hosts anything for it. Since v0.6.8 it is the only link: the old relay's WebSocket is gone.
//!
//! The protocol above it is v1 unchanged (pairing, the KK session, the narrow door, Face ID, approvals, hosted
//! wallets): this side turns events into the frames `Phone::handle_frame` takes, and the frames `Phone` sends into
//! events. Each phone connection is a channel: 16 random bytes the phone picks, mapped to a channel
//! number from `CH_BASE` up. Frames carry a sequence number per channel and direction, since several relays deliver
//! out of order and a session ends at any frame out of order: early ones wait (up to `GAP_SECS`), a gap that doesn't
//! fill closes the channel, as a dropped WebSocket does. Frames too big for one event go in parts.
//!
//! Event contents: `{"v":1,"c":"<channel>","s":<seq>,"d":<frame>}`, a part `{…,"p":[i,n],"x":"<text>"}`, or the end
//! of a channel `{…,"bye":true}`.

use super::nostr::Event;
use super::relays::{Gate, RelayPool, RelayStatus, Seen};
use super::Phone;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Channels over Nostr are numbered from here; the old relay's were far below (it counted from at most 2^32).
pub const CH_BASE: u64 = 1 << 52;
/// A frame longer than this (its JSON text) goes in parts, so no event's content passes the relays' usual limits.
const PART: usize = 24_000;
const MAX_PARTS: u64 = 16;
/// Early frames wait this long for the one before them.
const GAP_SECS: u64 = 10;
const MAX_EARLY: usize = 64;
/// A channel with nothing either way for this long is forgotten (the old relay's idle timeout).
const IDLE_SECS: u64 = 600;
/// Channels at once, per paired phone key (the oldest gives way to a new one, review M2) and in all.
const PER_KEY: usize = 8;
const MAX_CHANNELS: usize = 64;
/// Keys that aren't a paired phone's: channels only for a pair request that opens (review M1), a few at once.
const STRANGER_PER_KEY: usize = 2;
const MAX_STRANGER: usize = 8;
/// A channel forgotten here isn't opened again this long (a replayed first frame), review M2.
const GONE_SECS: u64 = 600;
/// Channel numbers are never reused, even across the pool's restarts (review L1).
static NEXT_CH: AtomicU64 = AtomicU64::new(0);
/// Events whose time is further than this from ours are dropped: an old event replayed can't open a channel.
const SKEW_SECS: u64 = 300;
const OFF_RECHECK: Duration = Duration::from_secs(30);

pub fn is_nostr(ch: u64) -> bool {
    ch >= CH_BASE
}

/// A sealed message's plaintext, padded with spaces (JSON ignores them) to 1, 4 or 16 KiB, and over Nostr (where big
/// frames go in parts) to a multiple of 16 KiB: relays see a few sizes, not each message's. On the tests' direct
/// channels (the old relay's, whose 64 KiB limit this kept to, review L3) nothing is padded past 16 KiB. Ends that don't
/// pad still read padded ones.
pub fn padded(msg: &Value, nostr: bool) -> Vec<u8> {
    let mut b = msg.to_string().into_bytes();
    let to = match b.len() {
        n if n <= 1024 => 1024,
        n if n <= 4096 => 4096,
        n if n <= 16_384 => 16_384,
        n if nostr => n.div_ceil(16_384) * 16_384,
        n => n,
    };
    b.resize(to, b' ');
    b
}

/// Only one FreeBank process answers over Nostr with this desktop's key: the app or its background part, whichever
/// holds `phone/nostr.lock` (review M3; the old relay's one host per room did this for its link). Held while the
/// process runs.
#[cfg(unix)]
fn take_lock(dir: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("nostr.lock")).ok()?;
    // SAFETY: flock on a descriptor this function owns; the lock goes when the file is closed.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (r == 0).then_some(f)
}

#[cfg(not(unix))]
fn take_lock(dir: &std::path::Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("nostr.lock")).ok()
}

/// The link over Nostr, fed from the phone's one outbox: while a phone paired over it (or a pairing or invite over it
/// is open), the relay pool, and the channels.
pub async fn run(phone: Arc<Phone>, mut out: mpsc::UnboundedReceiver<Value>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let gate = Arc::new(Gate::default());
    let mut lock: Option<std::fs::File> = None;
    // Outlives the pool's restarts, as `seen` does: `started` is this process's start, so a phone with a slow clock
    // isn't refused after each restart (re-review), and forgotten channels stay forgotten.
    let mut chans = Chans { started: super::unix_now(), ..Default::default() };
    loop {
        if !phone.nostr_wanted() {
            phone.set_nostr_status("off", "No phone is paired over Nostr.");
            while out.try_recv().is_ok() {}
            let _ = timeout(OFF_RECHECK, phone.wake_nostr.notified()).await;
            continue;
        }
        if lock.is_none() {
            lock = take_lock(&phone.store.dir);
            if lock.is_none() && !phone.is_background() {
                // The app takes the link back from a background part that started after it (security review L1; the
                // old relay's link did this when the part took its room).
                if let Some(app_dir) = phone.store.dir.parent().map(|d| d.to_path_buf()) {
                    // take_back waits up to 10 s for the part to stop: off the async workers.
                    let took = tokio::task::spawn_blocking(move || super::background::take_back(&app_dir)).await;
                    if matches!(took, Ok(Ok(Some(_)))) {
                        lock = take_lock(&phone.store.dir);
                    }
                }
            }
            if lock.is_none() {
                phone.set_nostr_status("waiting", "Another FreeBank on this computer (its background part) has the link over Nostr.");
                while out.try_recv().is_ok() {}
                let _ = timeout(Duration::from_secs(3), phone.wake_nostr.notified()).await;
                continue;
            }
        }
        let relays = phone.nostr_listen();
        let (in_tx, mut in_rx) = mpsc::channel::<Event>(256);
        refresh_gate(&phone, &gate);
        let pool = RelayPool::start(&relays, phone.nkey.pubkey(), in_tx, seen.clone(), gate.clone());
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                e = in_rx.recv() => {
                    if let Some(e) = e {
                        let known = gate.keys.read().unwrap().contains(&e.pubkey);
                        chans.incoming(&phone, &pool, &e, known);
                    }
                }
                f = out.recv() => {
                    let Some(f) = f else { return };
                    chans.outgoing(&phone, &pool, &f);
                }
                _ = phone.wake_nostr.notified() => {
                    refresh_gate(&phone, &gate);
                    if phone.nostr_listen() != relays || !phone.nostr_wanted() {
                        // What is queued goes first (revoking the last phone queues its "denied" and its close, then
                        // wakes us), then the pool goes. The pause lets those frames reach the outbox first.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        while let Ok(f) = out.try_recv() {
                            chans.outgoing(&phone, &pool, &f);
                        }
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        break;
                    }
                }
                _ = tick.tick() => {
                    refresh_gate(&phone, &gate);
                    // The week on the old relays is over (M5): go on with the list alone. Or nothing is wanted any more
                    // with time alone (a pairing code ran out, a revoked phone's hour is up).
                    if phone.nostr_listen() != relays || !phone.nostr_wanted() {
                        break;
                    }
                    chans.expire(&phone, &pool);
                    let st = pool.status();
                    let up = st.iter().filter(|r| r.connected).count();
                    if up > 0 {
                        phone.set_nostr_status("online", &format!("{up} of {} relays", st.len()));
                    } else {
                        phone.set_nostr_status("connecting", &why_down(&st));
                    }
                }
            }
        }
        drop(pool);
        chans.forget_all(&phone);
        phone.clear_nostr_channels();
    }
}

fn why_down(st: &[RelayStatus]) -> String {
    match st.iter().find_map(|r| r.error.as_ref().map(|e| format!("{}: {e}", r.url))) {
        Some(e) => format!("No relay is answering yet ({e})."),
        None => "Connecting to the relays.".into(),
    }
}

fn refresh_gate(phone: &Phone, gate: &Gate) {
    *gate.keys.write().unwrap() = phone.nostr_keys();
    gate.pairing.store(phone.nostr_pairing_open(), Ordering::Relaxed);
}

/// One phone connection over Nostr.
struct Chan {
    np: String,
    /// A paired phone's key, when the channel opened.
    known: bool,
    cid: String,
    next_in: u64,
    early: BTreeMap<u64, Value>,
    gap_since: Option<Instant>,
    next_out: u64,
    last: Instant,
    parts: Vec<String>,
    parts_n: u64,
}

#[derive(Default)]
struct Chans {
    by_key: HashMap<(String, String), u64>,
    by_ch: HashMap<u64, Chan>,
    /// Channels forgotten lately, by (key, channel), and when: not opened again (M2).
    gone: HashMap<(String, String), Instant>,
    /// When this process's link started (unix seconds): a first frame made before it is a replay at best (M2).
    started: u64,
}

impl Chans {
    /// An event from a relay (its id and signature already checked, duplicates dropped, by the pool). `known`: from a
    /// paired phone's key (or one revoked lately).
    fn incoming(&mut self, phone: &Arc<Phone>, pool: &RelayPool, e: &Event, known: bool) {
        let now = super::unix_now();
        if e.created_at.abs_diff(now) > SKEW_SECS {
            return;
        }
        let Ok(c) = serde_json::from_str::<Value>(&e.content) else { return };
        let (Some(cid), Some(s)) = (c["c"].as_str(), c["s"].as_u64()) else { return };
        if c["v"] != 1 || cid.len() != 32 || !cid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return;
        }
        let key = (e.pubkey.clone(), cid.to_string());
        let ch = match self.by_key.get(&key) {
            Some(ch) => *ch,
            None => {
                // A channel starts at its first frame, made since this pool started, and never twice.
                self.gone.retain(|_, t| t.elapsed() < Duration::from_secs(GONE_SECS));
                if s != 0 || e.created_at + 2 < self.started || self.gone.contains_key(&key) {
                    return;
                }
                let mine = |x: &Chan| x.np == e.pubkey;
                if known {
                    if self.by_ch.len() >= MAX_CHANNELS {
                        return;
                    }
                    // A phone's ninth channel: its least active one gives way (reloads leave channels behind, M2).
                    if self.by_ch.values().filter(|x| mine(x)).count() >= PER_KEY {
                        let oldest = self.by_ch.iter().filter(|(_, x)| mine(x)).min_by_key(|(_, x)| x.last).map(|(ch, _)| *ch);
                        if let Some(old) = oldest {
                            self.close(phone, pool, old);
                        }
                    }
                } else {
                    // A stranger: only a pair request that opens with a live code or invite, and naming this key (M1).
                    let strangers = self.by_ch.values().filter(|x| !x.known).count();
                    if strangers >= MAX_STRANGER
                        || self.by_ch.values().filter(|x| mine(x)).count() >= STRANGER_PER_KEY
                        || !c.get("d").is_some_and(|d| phone.pair_opens(d, &e.pubkey))
                    {
                        return;
                    }
                }
                let ch = CH_BASE + NEXT_CH.fetch_add(1, Ordering::Relaxed);
                self.by_key.insert(key, ch);
                self.by_ch.insert(
                    ch,
                    Chan {
                        np: e.pubkey.clone(),
                        known,
                        cid: cid.to_string(),
                        next_in: 0,
                        early: BTreeMap::new(),
                        gap_since: None,
                        next_out: 0,
                        last: Instant::now(),
                        parts: vec![],
                        parts_n: 0,
                    },
                );
                ch
            }
        };
        let Some(chan) = self.by_ch.get_mut(&ch) else { return };
        chan.last = Instant::now();
        if s < chan.next_in {
            return;
        }
        if s > chan.next_in {
            if chan.early.len() < MAX_EARLY {
                chan.early.insert(s, c);
            }
            chan.gap_since.get_or_insert_with(Instant::now);
            return;
        }
        let mut ready = vec![c];
        chan.next_in += 1;
        while let Some(next) = chan.early.remove(&chan.next_in) {
            ready.push(next);
            chan.next_in += 1;
        }
        chan.gap_since = if chan.early.is_empty() { None } else { Some(Instant::now()) };
        for piece in ready {
            if !self.piece(phone, pool, ch, piece) {
                return;
            }
        }
    }

    /// One frame in order: delivered, or a part kept, or the channel's end. False once the channel is gone.
    fn piece(&mut self, phone: &Arc<Phone>, pool: &RelayPool, ch: u64, c: Value) -> bool {
        if c["bye"] == true {
            self.forget(ch);
            phone.handle_frame(json!({"t": "closed", "ch": ch}));
            return false;
        }
        let Some(chan) = self.by_ch.get_mut(&ch) else { return false };
        let frame = if let (Some(p), Some(x)) = (c["p"].as_array(), c["x"].as_str()) {
            let (i, n) = (p.first().and_then(Value::as_u64), p.get(1).and_then(Value::as_u64));
            let fits = |i: u64, n: u64| n >= 2 && n <= MAX_PARTS && i < n && i == chan.parts.len() as u64 && (i == 0 || n == chan.parts_n);
            match (i, n) {
                (Some(i), Some(n)) if fits(i, n) && x.len() <= PART => {
                    chan.parts_n = n;
                    chan.parts.push(x.to_string());
                    if i + 1 < n {
                        return true;
                    }
                    let text: String = chan.parts.drain(..).collect();
                    match serde_json::from_str::<Value>(&text) {
                        Ok(f) => f,
                        Err(_) => return self.close(phone, pool, ch),
                    }
                }
                _ => return self.close(phone, pool, ch),
            }
        } else {
            match c.get("d") {
                Some(d) if chan.parts.is_empty() => d.clone(),
                _ => return self.close(phone, pool, ch),
            }
        };
        let np = chan.np.clone();
        phone.handle_frame(json!({"ch": ch, "d": frame, "np": np}));
        true
    }

    /// A frame (or a close) from `Phone` for a channel over Nostr.
    fn outgoing(&mut self, phone: &Phone, pool: &RelayPool, f: &Value) {
        let Some(ch) = f["ch"].as_u64() else { return };
        if f["t"] == "close" {
            self.send(phone, pool, ch, json!({"bye": true}));
            self.forget(ch);
            return;
        }
        let Some(d) = f.get("d") else { return };
        let text = d.to_string();
        if text.len() <= PART {
            self.send(phone, pool, ch, json!({"d": d}));
            return;
        }
        let parts = split_text(&text, PART);
        let n = parts.len() as u64;
        if n > MAX_PARTS {
            return;
        }
        for (i, x) in parts.into_iter().enumerate() {
            self.send(phone, pool, ch, json!({"p": [i, n], "x": x}));
        }
    }

    fn send(&mut self, phone: &Phone, pool: &RelayPool, ch: u64, mut body: Value) {
        let Some(chan) = self.by_ch.get_mut(&ch) else { return };
        body["v"] = json!(1);
        body["c"] = json!(chan.cid);
        body["s"] = json!(chan.next_out);
        chan.next_out += 1;
        chan.last = Instant::now();
        pool.publish(phone.nkey.message(&chan.np, body.to_string(), super::unix_now()));
    }

    /// Close a channel from this side: the phone hears "bye" (and starts again), `Phone` hears "closed".
    fn close(&mut self, phone: &Arc<Phone>, pool: &RelayPool, ch: u64) -> bool {
        self.send(phone, pool, ch, json!({"bye": true}));
        self.forget(ch);
        phone.handle_frame(json!({"t": "closed", "ch": ch}));
        false
    }

    fn forget(&mut self, ch: u64) {
        if let Some(c) = self.by_ch.remove(&ch) {
            let key = (c.np, c.cid);
            self.by_key.remove(&key);
            self.gone.insert(key, Instant::now());
        }
    }

    /// Every second: gaps that didn't fill, and idle channels.
    fn expire(&mut self, phone: &Arc<Phone>, pool: &RelayPool) {
        let gone: Vec<u64> = self
            .by_ch
            .iter()
            .filter(|(_, c)| {
                c.gap_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(GAP_SECS))
                    || c.last.elapsed() >= Duration::from_secs(IDLE_SECS)
            })
            .map(|(ch, _)| *ch)
            .collect();
        for ch in gone {
            self.close(phone, pool, ch);
        }
    }

    /// The pool is going (the relays changed, or nothing is paired over Nostr any more): its channels are forgotten.
    fn forget_all(&mut self, phone: &Arc<Phone>) {
        for ch in self.by_ch.keys().copied().collect::<Vec<_>>() {
            self.forget(ch);
            phone.handle_frame(json!({"t": "closed", "ch": ch}));
        }
    }
}

/// `text` in pieces of at most `max` bytes, cut at character boundaries.
fn split_text(text: &str, max: usize) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while !rest.is_empty() {
        let mut cut = rest.len().min(max);
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_gives_a_few_sizes_and_stays_json() {
        // {"x":"…"} is 8 bytes more than its letters.
        for (letters, want) in [(10, 1024), (1016, 1024), (1017, 4096), (5000, 16_384), (20_000, 32_768)] {
            let v = json!({"x": "a".repeat(letters)});
            let p = padded(&v, true);
            assert_eq!(p.len(), want, "{letters}");
            assert_eq!(serde_json::from_slice::<Value>(&p).unwrap(), v);
        }
        // On the direct channels (the old relay's), nothing past 16 KiB is padded (L3).
        assert_eq!(padded(&json!({"x": "a".repeat(20_000)}), false).len(), 20_008);
        assert_eq!(padded(&json!({"x": "a".repeat(5000)}), false).len(), 16_384);
    }

    #[test]
    fn long_text_splits_at_character_boundaries() {
        let t = "é".repeat(30_000);
        let parts = split_text(&t, PART);
        assert!(parts.iter().all(|p| p.len() <= PART));
        assert_eq!(parts.concat(), t);
        assert!(split_text("", PART).is_empty());
    }

    #[test]
    fn one_process_holds_the_link_over_nostr() {
        let d = std::env::temp_dir().join(format!("fb-nostr-lock-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let held = take_lock(&d).expect("the first takes it");
        assert!(take_lock(&d).is_none(), "a second process (here, a second open) waits (M3)");
        drop(held);
        assert!(take_lock(&d).is_some(), "and gets it once the first lets go");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn channel_numbers_are_apart_from_the_relays() {
        assert!(!is_nostr(u64::from(u32::MAX) * 2));
        assert!(is_nostr(CH_BASE));
    }
}
