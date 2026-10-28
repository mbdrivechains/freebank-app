//! The link over Nostr (v0.6.0, `phone/carrier.rs`; PHONE_LINK_NOSTR.md): a phone played in Rust pairs, opens a
//! session and asks, through a Nostr relay run here (NIP-01's REQ, EVENT and OK, nothing kept), which can deliver
//! events out of order. Against the mocked node.

use super::*;
use crate::phone::carrier;
use crate::phone::nostr::{self, Event, NostrKey};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

/// A NIP-01 relay on 127.0.0.1: each event goes to every subscription whose kinds and `#p` match. With `swap`, each
/// pair of events leaves in the other order (the second first).
async fn test_relay(swap: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (all, _) = broadcast::channel::<Value>(1024);
    let held: Arc<Mutex<Option<Value>>> = Arc::default();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let all = all.clone();
            let held = held.clone();
            tokio::spawn(async move {
                let Ok(ws) = tokio_tungstenite::accept_async(tcp).await else { return };
                let (mut tx, mut rx) = ws.split();
                let mut subs: Vec<(String, Value)> = vec![];
                let mut feed = all.subscribe();
                loop {
                    tokio::select! {
                        m = rx.next() => {
                            let Some(Ok(Message::Text(t))) = m else { return };
                            let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                            match v[0].as_str() {
                                Some("REQ") => subs.push((v[1].as_str().unwrap_or("").to_string(), v[2].clone())),
                                Some("EVENT") => {
                                    let e = v[1].clone();
                                    let ok = json!(["OK", e["id"], true, ""]);
                                    let _ = tx.send(Message::text(ok.to_string())).await;
                                    if swap {
                                        let first = held.lock().unwrap().take();
                                        match first {
                                            None => {
                                                // Held for the next event, or 300 ms at most.
                                                *held.lock().unwrap() = Some(e.clone());
                                                let (held, all) = (held.clone(), all.clone());
                                                tokio::spawn(async move {
                                                    tokio::time::sleep(Duration::from_millis(300)).await;
                                                    let still = held.lock().unwrap().take_if(|h| h["id"] == e["id"]);
                                                    if let Some(h) = still {
                                                        let _ = all.send(h);
                                                    }
                                                });
                                            }
                                            Some(f) => {
                                                let _ = all.send(e);
                                                let _ = all.send(f);
                                            }
                                        }
                                    } else {
                                        let _ = all.send(e);
                                    }
                                }
                                _ => {}
                            }
                        }
                        e = feed.recv() => {
                            let Ok(e) = e else { continue };
                            for (id, f) in &subs {
                                let kinds = f["kinds"].as_array().is_none_or(|k| k.contains(&e["kind"]));
                                let p = e["tags"].as_array().and_then(|t| t.iter().find(|t| t[0] == "p")).map(|t| t[1].clone());
                                let to_p = f["#p"].as_array().is_none_or(|ps| p.as_ref().is_some_and(|p| ps.contains(p)));
                                if kinds && to_p {
                                    let m = json!(["EVENT", id, e]);
                                    let _ = tx.send(Message::text(m.to_string())).await;
                                }
                            }
                        }
                    }
                }
            });
        }
    });
    url
}

/// The phone's end of the link over Nostr: its Nostr key, one channel at a time, frames in order.
struct NPhone {
    nk: NostrKey,
    nd: String,
    tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
        Message,
    >,
    rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    >,
    cid: String,
    s_out: u64,
    s_in: u64,
    early: std::collections::BTreeMap<u64, Value>,
    parts: String,
    /// What came for this phone's earlier channels.
    others: Vec<Value>,
}

impl NPhone {
    async fn connect(relay: &str, nd: &str, nk: NostrKey) -> NPhone {
        let (ws, _) = tokio_tungstenite::connect_async(relay).await.unwrap();
        let (mut tx, rx) = ws.split();
        let req = json!(["REQ", "p", {"kinds": [nostr::KIND], "#p": [nk.pubkey()]}]);
        tx.send(Message::text(req.to_string())).await.unwrap();
        let mut me = NPhone { nk, nd: nd.into(), tx, rx, cid: String::new(), s_out: 0, s_in: 0, early: Default::default(), parts: String::new(), others: vec![] };
        me.new_channel();
        me
    }

    fn new_channel(&mut self) {
        self.cid = hex::encode(rand16());
        self.s_out = 0;
        self.s_in = 0;
        self.early.clear();
    }

    async fn send_body(&mut self, mut body: Value) {
        body["v"] = json!(1);
        body["c"] = json!(self.cid);
        body["s"] = json!(self.s_out);
        self.s_out += 1;
        let e = self.nk.message(&self.nd, body.to_string(), unix_now());
        self.tx.send(Message::text(json!(["EVENT", e]).to_string())).await.unwrap();
    }

    async fn send(&mut self, frame: Value) {
        self.send_body(json!({"d": frame})).await;
    }

    /// The desktop's next frame on this channel, in order (`{"bye":true}` for its end), within `secs`.
    async fn next(&mut self, secs: u64) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(c) = self.early.remove(&self.s_in) {
                self.s_in += 1;
                if c["bye"] == true {
                    return Some(json!({"bye": true}));
                }
                if let (Some(p), Some(x)) = (c["p"].as_array(), c["x"].as_str()) {
                    self.parts.push_str(x);
                    if p[0].as_u64().unwrap() + 1 == p[1].as_u64().unwrap() {
                        return Some(serde_json::from_str(&std::mem::take(&mut self.parts)).unwrap());
                    }
                    continue;
                }
                return Some(c["d"].clone());
            }
            let m = tokio::time::timeout_at(deadline, self.rx.next()).await.ok()??;
            let Ok(Message::Text(t)) = m else { continue };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v[0] != "EVENT" {
                continue;
            }
            let e: Event = serde_json::from_value(v[2].clone()).unwrap();
            assert!(nostr::verify(&e));
            assert_eq!(e.pubkey, self.nd, "only the desktop's key writes to this phone");
            let c: Value = serde_json::from_str(&e.content).unwrap();
            if c["c"] != json!(self.cid) {
                self.others.push(c);
                continue;
            }
            self.early.insert(c["s"].as_u64().unwrap(), c);
        }
    }
}

/// A desktop whose link over Nostr runs against `relay`, and its pairing link (v2).
async fn desktop(relay: &str) -> (H, Value) {
    let mut h = harness(None);
    h.phone.set_nostr_relays(&[relay.to_string()]).unwrap();
    let rx = std::mem::replace(&mut h.rx, mpsc::unbounded_channel().1);
    tokio::spawn(carrier::run(h.phone.clone(), rx));
    let (url, _) = h.phone.pair_start().unwrap();
    assert!(url.starts_with("https://app.ecxfreebank.com/#pair="), "{url}");
    let link = link_of(&url);
    for _ in 0..250 {
        if h.phone.nostr_status().state == "online" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.phone.nostr_status().state, "online", "{}", h.phone.nostr_status().detail);
    (h, link)
}

/// The pair request over Nostr: v1's, with the phone's Nostr key in it.
fn pair_frame(sim: &Sim, d_pub: &PublicKey, c: &[u8], np: &str) -> (Value, String) {
    let e = crypto::random_secret();
    let k = crypto::pair_key(&e, d_pub, c);
    let n = [7u8; 12];
    let pt = json!({"p": sim.p_pub(), "name": "Nostr phone", "np": np}).to_string();
    let frame = json!({"t": "pair", "e": crypto::pub_b64u(&e.public_key()), "n": crypto::b64u(&n),
           "ct": crypto::b64u(&crypto::seal(&k, &n, pt.as_bytes()))});
    (frame, crypto::pair_code(d_pub, &sim.p.public_key(), &e.public_key(), c))
}

async fn wait_ask(h: &H) {
    for _ in 0..250 {
        if !h.phone.pair_pending().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no pair request arrived");
}

/// Pair over Nostr and open a session on a fresh channel.
async fn pair_and_open(h: &H, link: &Value, relay: &str) -> (NPhone, Sim) {
    let d_pub = h.d_pub();
    let mut sim = Sim::new();
    let mut np = NPhone::connect(relay, link["n"].as_str().unwrap(), NostrKey::random()).await;
    let c = crypto::unb64u(link["c"].as_str().unwrap()).unwrap();
    let (f, code) = pair_frame(&sim, &d_pub, &c, &np.nk.pubkey());
    np.send(f).await;
    wait_ask(h).await;
    assert_eq!(h.phone.pair_pending()[0].code, code);
    assert_eq!(np.next(5).await.unwrap()["t"], "pair-seen", "over Nostr the phone hears its request arrived");
    allow_the_one(h);
    assert_eq!(np.next(5).await.unwrap()["t"], "paired");
    assert!(h.phone.devices().iter().any(|d| d.np.as_deref() == Some(np.nk.pubkey().as_str())));
    np.new_channel();
    let hello = sim.hello();
    np.send(hello).await;
    let ok = np.next(5).await.unwrap();
    sim.hello_ok(&d_pub, &ok);
    (np, sim)
}

#[tokio::test]
async fn a_phone_pairs_and_asks_over_nostr() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    assert_eq!(link["v"], 2);
    assert_eq!(link["r"], json!([relay]));
    assert_eq!(link["n"], json!(h.phone.nkey.pubkey()));
    assert!(link["x"].as_u64().is_some() && link.get("room").is_none());
    let (mut np, mut sim) = pair_and_open(&h, &link, &relay).await;
    let r = sim.req(1, "balance", json!({}));
    np.send(r).await;
    let reply = sim.open(&np.next(5).await.unwrap());
    assert_eq!(reply["id"], 1);
    assert!(reply["ok"]["confirmed"].is_number(), "{reply}");
}

#[tokio::test]
async fn frames_out_of_order_arrive_in_order() {
    let relay = test_relay(true).await;
    let (h, link) = desktop(&relay).await;
    let (mut np, mut sim) = pair_and_open(&h, &link, &relay).await;
    for id in 1..=4 {
        let r = sim.req(id, "status", json!({}));
        np.send(r).await;
    }
    // The relay swaps each pair; the desktop puts them back in order (the session's counters would end it otherwise).
    let mut ids = vec![];
    for _ in 0..4 {
        ids.push(sim.open(&np.next(15).await.expect("a reply"))["id"].as_u64().unwrap());
    }
    ids.sort();
    assert_eq!(ids, vec![1, 2, 3, 4]);
}

#[tokio::test]
async fn sessions_come_only_from_the_phones_own_nostr_key() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let (_np, sim) = pair_and_open(&h, &link, &relay).await;
    // A second phone paired over Nostr, whose key then says hello with the first phone's device key: denied.
    let (url2, _) = h.phone.pair_start().unwrap();
    let (mut b, _) = pair_and_open(&h, &link_of(&url2), &relay).await;
    b.new_channel();
    let mut twin = sim.twin();
    let hello = twin.hello();
    b.send(hello).await;
    assert_eq!(b.next(5).await.unwrap()["t"], "denied");
    // A stranger's key with it, even while a pairing is open: no channel at all (review M1).
    let _ = h.phone.pair_start().unwrap();
    let mut other = NPhone::connect(&relay, link["n"].as_str().unwrap(), NostrKey::random()).await;
    let hello = sim.twin().hello();
    other.send(hello).await;
    assert!(other.next(2).await.is_none());
}

#[tokio::test]
async fn a_pair_request_naming_another_nostr_key_is_refused() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let sim = Sim::new();
    let mut np = NPhone::connect(&relay, link["n"].as_str().unwrap(), NostrKey::random()).await;
    let c = crypto::unb64u(link["c"].as_str().unwrap()).unwrap();
    let (f, _) = pair_frame(&sim, &h.d_pub(), &c, &NostrKey::random().pubkey());
    np.send(f).await;
    // It doesn't open for this key, so it gets no channel and no answer (review M1).
    assert!(np.next(3).await.is_none());
    assert!(h.phone.pair_pending().is_empty());
}

#[tokio::test]
async fn a_revoked_phone_hears_denied_and_a_ninth_channel_takes_the_oldest_place() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let (mut np, mut sim) = pair_and_open(&h, &link, &relay).await;
    // Eight more channels (reloads that never said bye): the ninth still opens, the first gives way (review M2).
    let first_cid = np.cid.clone();
    for _ in 0..8 {
        np.new_channel();
        let hello = sim.hello();
        np.send(hello).await;
        let ok = np.next(5).await.expect("a new channel is answered");
        sim.hello_ok(&h.d_pub(), &ok);
    }
    assert!(
        np.others.iter().any(|c| c["c"] == json!(first_cid) && c["bye"] == true),
        "the first channel heard bye when the ninth took its place"
    );
    // Revoked: its next hello still gets through, and hears denied (L5), not silence.
    let id = h.phone.devices()[0].id.clone();
    h.phone.revoke(&id).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    np.new_channel();
    let hello = sim.hello();
    np.send(hello).await;
    assert_eq!(np.next(5).await.unwrap()["t"], "denied");
}

#[tokio::test]
async fn strangers_are_not_heard_once_pairing_is_over() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let (_np, _sim) = pair_and_open(&h, &link, &relay).await;
    // The code is used up: a stranger's events don't get past the gate, so nothing at all comes back.
    let mut stranger = NPhone::connect(&relay, link["n"].as_str().unwrap(), NostrKey::random()).await;
    stranger.send(Sim::new().hello()).await;
    assert!(stranger.next(2).await.is_none());
}

#[tokio::test]
async fn big_frames_go_in_parts_both_ways() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let (mut np, mut sim) = pair_and_open(&h, &link, &relay).await;
    // From the phone: a request too big for one event, in parts. The desktop puts it together and answers it.
    let r = sim.req(5, "status", json!({"pad": "x".repeat(40_000)}));
    let text = r.to_string();
    let parts: Vec<String> = text.as_bytes().chunks(20_000).map(|c| String::from_utf8(c.to_vec()).unwrap()).collect();
    let n = parts.len();
    assert!(n >= 2);
    for (i, x) in parts.into_iter().enumerate() {
        np.send_body(json!({"p": [i, n], "x": x})).await;
    }
    assert_eq!(sim.open(&np.next(5).await.unwrap())["id"], 5);
    // From the desktop: a message padded past 16 KiB comes in parts and opens whole.
    let ch = *h.phone.chans.lock().unwrap().keys().find(|c| carrier::is_nostr(**c)).unwrap();
    assert!(h.phone.send_sealed(ch, &json!({"big": "y".repeat(40_000)})));
    assert_eq!(sim.open(&np.next(5).await.unwrap())["big"].as_str().unwrap().len(), 40_000);
}

#[tokio::test]
async fn revoking_a_phone_over_nostr_ends_its_session_and_the_link() {
    let relay = test_relay(false).await;
    let (h, link) = desktop(&relay).await;
    let (mut np, _sim) = pair_and_open(&h, &link, &relay).await;
    let id = h.phone.devices()[0].id.clone();
    h.phone.revoke(&id).unwrap();
    assert_eq!(np.next(5).await.unwrap()["t"], "denied");
    assert_eq!(np.next(5).await.unwrap(), json!({"bye": true}));
    // It stays up an hour for the revoked phone to hear denied (L5), then goes off.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(h.phone.nostr_status().state, "online");
    h.later(3_601);
    for _ in 0..250 {
        if h.phone.nostr_status().state == "off" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the link over Nostr should be off with nothing paired over it");
}

/// The real phone page against this desktop over a real relay: `phone/e2e/nostr.mjs` (freebank-distribution) runs it
/// with `FB_NOSTR_RELAY=ws://127.0.0.1:<port>` and `FB_NOSTR_OUT=<file>`. The desktop writes its pairing link there,
/// then each pair request's comparison code (`code <digits>`), and allows the first request; it answers for 90 s.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn nostr_page_e2e() {
    let relay = std::env::var("FB_NOSTR_RELAY").expect("FB_NOSTR_RELAY");
    let out = std::env::var("FB_NOSTR_OUT").expect("FB_NOSTR_OUT");
    let (h, _) = desktop(&relay).await;
    // The page checks the link's expiry against its own clock: the harness's runs at today's time here.
    h.now.store(unix_now(), Ordering::SeqCst);
    let (url, _) = h.phone.pair_start().unwrap();
    std::fs::write(&out, format!("link {url}\n")).unwrap();
    let t0 = std::time::Instant::now();
    let mut allowed = false;
    while t0.elapsed() < Duration::from_secs(90) {
        if !allowed {
            if let Some(a) = h.phone.pair_pending().first().cloned() {
                let mut f = std::fs::OpenOptions::new().append(true).open(&out).unwrap();
                std::io::Write::write_all(&mut f, format!("code {}\n", a.code.replace(' ', "")).as_bytes()).unwrap();
                // The page shows its code first; the owner compares, then allows.
                tokio::time::sleep(Duration::from_millis(1500)).await;
                h.phone.pair_answer(&a.id, true).unwrap();
                allowed = true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn after_a_relay_change_the_old_relays_are_heard_for_a_week() {
    let h = harness(None);
    let old = h.phone.nostr_relays();
    h.phone.set_nostr_relays(&["wss://relay.example.org".to_string()]).unwrap();
    let listen = h.phone.nostr_listen();
    assert_eq!(listen[0], "wss://relay.example.org");
    assert!(old.iter().all(|r| listen.contains(r)), "phones still on the old list hear the new one (M5): {listen:?}");
    assert_eq!(h.phone.nostr_relays(), vec!["wss://relay.example.org".to_string()], "status names the new list only");
    // A second change within the week keeps both earlier lists (re-review), and the week starts over.
    h.later(86_400);
    h.phone.set_nostr_relays(&["wss://relay.example.net".to_string()]).unwrap();
    let listen = h.phone.nostr_listen();
    assert!(listen.contains(&"wss://relay.example.org".to_string()) && old.iter().all(|r| listen.contains(r)), "{listen:?}");
    h.later(7 * 86_400 + 1);
    assert_eq!(h.phone.nostr_listen(), vec!["wss://relay.example.net".to_string()]);
    assert!(h.phone.set_nostr_relays(&["https://x".to_string()]).is_err());
    assert!(h.phone.set_nostr_relays(&[]).is_err());
}
