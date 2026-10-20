//! Connections to the Nostr relays (NIP-01): one task per relay, each subscribed to events of the link's kind
//! addressed to the desktop's Nostr key, each publishing every event the link sends. A relay that fails is retried
//! with a growing pause; one working relay is enough. Every incoming event's id and signature are checked, and
//! duplicates (the same event from several relays) are dropped, before the link sees it.

use super::nostr::{self, Event};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

#[derive(Serialize, Clone, Debug, Default)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
    pub error: Option<String>,
}

/// Whose events are worth a signature check: paired phones' Nostr keys, and anyone while a pairing code is live. The
/// link keeps it up to date; relay tasks read it (review L5: cheap checks before any work).
#[derive(Default)]
pub struct Gate {
    pub keys: std::sync::RwLock<HashSet<String>>,
    pub pairing: std::sync::atomic::AtomicBool,
}

impl Gate {
    fn wants(&self, pubkey: &str) -> bool {
        nostr::valid_pubkey(pubkey)
            && (self.pairing.load(std::sync::atomic::Ordering::Relaxed) || self.keys.read().unwrap().contains(pubkey))
    }
}

/// The largest WebSocket message or frame taken from a relay: an event of ours is under 32 KiB.
const MAX_WS_MESSAGE: usize = 256 * 1024;
/// Events for a key the link listens to, taken from one relay in a 10-second window; the rest are dropped. Junk for
/// other keys doesn't count against it (re-review R2); raw messages have their own, higher cap.
const MAX_EVENTS_PER_10S: usize = 200;
const MAX_RAW_PER_10S: usize = 5_000;

/// Event ids already passed on, so the same event from three relays reaches the link once.
#[derive(Default)]
pub struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }

    fn first_time(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        if self.order.len() > 4096 {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

pub struct RelayPool {
    out: broadcast::Sender<Event>,
    status: Arc<Mutex<Vec<RelayStatus>>>,
    tasks: Vec<tauri::async_runtime::JoinHandle<()>>,
}

impl Drop for RelayPool {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// A relay address the link may use: wss:// anywhere, or ws:// on this computer (tests, a relay of your own).
pub fn usable(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else { return false };
    match u.scheme() {
        "wss" => u.host_str().is_some_and(|h| !h.is_empty()),
        "ws" => matches!(u.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]")),
        _ => false,
    }
}

impl RelayPool {
    /// Connect to `relays`, subscribed to events for `pubkey`; incoming events that pass `gate` go to `incoming`.
    /// `seen` outlives the pool, so a reconnect doesn't pass on an event twice.
    pub fn start(
        relays: &[String],
        pubkey: String,
        incoming: mpsc::Sender<Event>,
        seen: Arc<Mutex<Seen>>,
        gate: Arc<Gate>,
    ) -> RelayPool {
        let (out, _) = broadcast::channel(256);
        let status = Arc::new(Mutex::new(
            relays.iter().map(|r| RelayStatus { url: r.clone(), ..Default::default() }).collect::<Vec<_>>(),
        ));
        let mut tasks = vec![];
        for (i, url) in relays.iter().enumerate().filter(|(_, u)| usable(u)) {
            let ctx = Ctx {
                i,
                url: url.clone(),
                pubkey: pubkey.clone(),
                incoming: incoming.clone(),
                status: status.clone(),
                seen: seen.clone(),
                gate: gate.clone(),
            };
            let rx = out.subscribe();
            // Tauri's runtime: this may be called from a command on the main thread, which has no Tokio context.
            tasks.push(tauri::async_runtime::spawn(async move { run(ctx, rx).await }));
        }
        RelayPool { out, status, tasks }
    }

    pub fn publish(&self, e: Event) {
        let _ = self.out.send(e);
    }

    pub fn status(&self) -> Vec<RelayStatus> {
        self.status.lock().unwrap().clone()
    }
}

struct Ctx {
    i: usize,
    url: String,
    pubkey: String,
    incoming: mpsc::Sender<Event>,
    status: Arc<Mutex<Vec<RelayStatus>>>,
    seen: Arc<Mutex<Seen>>,
    gate: Arc<Gate>,
}

async fn run(ctx: Ctx, mut out: broadcast::Receiver<Event>) {
    let Ctx { i, url, pubkey, incoming, status, seen, gate } = ctx;
    let mut pause = Duration::from_secs(2);
    let mut window = (std::time::Instant::now(), 0usize, 0usize);
    let set = |connected: bool, error: Option<String>| {
        if let Some(s) = status.lock().unwrap().get_mut(i) {
            s.connected = connected;
            s.error = error;
        }
    };
    loop {
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_WS_MESSAGE))
            .max_frame_size(Some(MAX_WS_MESSAGE));
        let r = tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false),
        )
        .await;
        let ws = match r {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => {
                set(false, Some(e.to_string()));
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(60));
                continue;
            }
            Err(_) => {
                set(false, Some("no answer".into()));
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        let (mut tx, mut rx) = ws.split();
        let since = super::unix_now().saturating_sub(120);
        let req = json!(["REQ", "fb", {"kinds": [nostr::KIND], "#p": [pubkey], "since": since}]);
        if tx.send(Message::Text(req.to_string().into())).await.is_err() {
            continue;
        }
        set(true, None);
        pause = Duration::from_secs(2);
        let mut ping = tokio::time::interval(Duration::from_secs(50));
        ping.tick().await;
        let why = loop {
            tokio::select! {
                m = rx.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        // Every message counts against the raw cap before it is even parsed (review of v0.6.0, L9).
                        if window.0.elapsed() > Duration::from_secs(10) {
                            window = (std::time::Instant::now(), 0, 0);
                        }
                        window.2 += 1;
                        if window.2 > MAX_RAW_PER_10S {
                            continue;
                        }
                        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                        match v.get(0).and_then(Value::as_str) {
                            Some("EVENT") => {
                                let Some(ev) = v.get(2).and_then(|e| serde_json::from_value::<Event>(e.clone()).ok()) else { continue };
                                // The cheap checks first; a signature only for a key the link listens to.
                                if ev.kind != nostr::KIND || nostr::tag(&ev, "p") != Some(pubkey.as_str()) || ev.content.len() > 40_000 {
                                    continue;
                                }
                                if !gate.wants(&ev.pubkey) || seen.lock().unwrap().contains(&ev.id) {
                                    continue;
                                }
                                if !nostr::verify(&ev) {
                                    continue;
                                }
                                // Only events that are what they claim count against the relay's budget, so a relay's
                                // forgeries (or strangers' junk while a pairing is open) can't crowd out real ones (L2).
                                window.1 += 1;
                                if window.1 > MAX_EVENTS_PER_10S || !seen.lock().unwrap().first_time(&ev.id) {
                                    continue;
                                }
                                let _ = incoming.send(ev).await;
                            }
                            Some("OK") if v.get(2) == Some(&Value::Bool(false)) => {
                                let msg = v.get(3).and_then(Value::as_str).unwrap_or("").to_string();
                                if msg.starts_with("rate-limited") {
                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                }
                                set(true, Some(format!("refused an event: {msg}")));
                            }
                            Some("CLOSED") => break "the relay closed the subscription".to_string(),
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Ping(p))) => { let _ = tx.send(Message::Pong(p)).await; }
                    Some(Ok(Message::Close(_))) | None => break "closed".to_string(),
                    Some(Err(e)) => break e.to_string(),
                    _ => {}
                },
                e = out.recv() => match e {
                    Ok(e) => {
                        let m = json!(["EVENT", e]).to_string();
                        if let Err(err) = tx.send(Message::Text(m.into())).await { break err.to_string(); }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => return,
                },
                _ = ping.tick() => {
                    if let Err(e) = tx.send(Message::Ping(Vec::new().into())).await { break e.to_string(); }
                }
            }
        };
        set(false, Some(why));
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(60));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn relay_addresses() {
        assert!(super::usable("wss://relay.damus.io"));
        assert!(super::usable("ws://127.0.0.1:7000"));
        assert!(!super::usable("ws://relay.damus.io"));
        assert!(!super::usable("https://relay.damus.io"));
        assert!(!super::usable("wss://"));
    }

    #[test]
    fn duplicates_are_seen_once() {
        let mut s = super::Seen::default();
        assert!(s.first_time("a"));
        assert!(!s.first_time("a"));
        for i in 0..5000 {
            s.first_time(&i.to_string());
        }
        assert!(s.first_time("a"), "old ids are forgotten after 4096 newer ones");
    }
}
