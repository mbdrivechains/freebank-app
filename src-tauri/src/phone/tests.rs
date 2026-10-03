//! The desktop side against the shared interop vectors (testdata/phone/kk-v1.json, a copy of
//! freebank-phone's relay/testvectors/kk-v1.json, written by the phone page's generator), and
//! pairing, revoke, the limit ledger, held sends (expiry, held.json, restarts) and phone sends from
//! an encrypted wallet, with a mocked node. The end-to-end run against the real relay is
//! `relay_e2e`, and `page_host` serves the real phone page (both ignored; they need FB_RELAY_URL).

use super::crypto::{self, Direction};
use super::*;
use p256::{PublicKey, SecretKey};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

const VECTORS: &str = include_str!("../../testdata/phone/kk-v1.json");
const TO: &str = "XkPvjfFq8Hj9wy9Wq3pRr1sBdXJxZ5nT2m";
/// The mock wallet's passphrase when it is encrypted.
const PASS: &str = "correct horse battery staple";
const LOCKED_MSG: &str = "Error: Please enter the wallet passphrase with walletpassphrase first.";
const WRONG_MSG: &str = "Error: The wallet passphrase entered was incorrect.";
const NOT_ENOUGH: &str = "Not enough ECX in your desktop wallet for this payment and its fee.";

// ----- mocks ---------------------------------------------------------------------------------

/// The node: a wallet with 1.5 ECX, three transactions, and optionally a passphrase. Its unlock
/// runs out by the harness clock, like Core's. Like freebankd (Core 0.16), an unlock that lands
/// on the moment the previous unlock's relock timer fires deadlocks it; the mock only notes it.
struct MockRpc {
    calls: Mutex<Vec<(String, Vec<Value>)>>,
    fail_send: AtomicBool,
    /// The next getwalletinfo answers as usual, then the unlock ends (it ran out in between).
    lock_after_look: AtomicBool,
    now: Arc<AtomicU64>,
    wallet: Mutex<MockWallet>,
}

#[derive(Default)]
struct MockWallet {
    /// Some: encrypted with this passphrase.
    pass: Option<String>,
    /// getwalletinfo's txcount (3, like listtransactions) and hdmasterkeyid.
    txcount: u64,
    hd: Option<String>,
    unlocked_until: u64,
    /// When the relock timer of the last walletpassphrase fires (real time; walletlock leaves it).
    relock_due: Option<std::time::Instant>,
    deadlocked: bool,
}

impl MockRpc {
    fn new(now: Arc<AtomicU64>) -> Self {
        Self {
            calls: Mutex::default(),
            fail_send: AtomicBool::new(false),
            lock_after_look: AtomicBool::new(false),
            now,
            wallet: Mutex::new(MockWallet { txcount: 3, ..Default::default() }),
        }
    }

    /// Encrypt the wallet (or change its passphrase). It is locked after.
    fn encrypt(&self, pass: &str) {
        let mut w = self.wallet.lock().unwrap();
        w.pass = Some(pass.into());
        w.unlocked_until = 0;
    }

    fn unlock_until(&self, t: u64) {
        self.wallet.lock().unwrap().unlocked_until = t;
    }

    /// Can it pay right now?
    fn unlocked(&self) -> bool {
        let w = self.wallet.lock().unwrap();
        w.pass.is_none() || w.unlocked_until > self.now.load(Ordering::SeqCst)
    }

    fn sends(&self) -> Vec<Vec<Value>> {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == "sendtoaddress").map(|(_, p)| p.clone()).collect()
    }

    fn methods(&self) -> Vec<String> {
        self.calls.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    fn params_of(&self, method: &str) -> Vec<Vec<Value>> {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }

    fn forget_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    fn deadlocked(&self) -> bool {
        self.wallet.lock().unwrap().deadlocked
    }
}

impl Rpc for MockRpc {
    fn call<'a>(&'a self, method: &'a str, params: Vec<Value>) -> BoxFuture<'a, Result<Value, RpcFail>> {
        Box::pin(async move {
            let n = {
                let mut c = self.calls.lock().unwrap();
                c.push((method.to_string(), params.clone()));
                c.len()
            };
            let now = self.now.load(Ordering::SeqCst);
            let unencrypted = |m: &str| RpcFail::rpc(-15, format!("Error: running with an unencrypted wallet, but {m} was called."));
            match method {
                "getbalance" => Ok(json!(1.5)),
                "getunconfirmedbalance" => Ok(json!(0.0)),
                "getnewaddress" => Ok(json!(TO)),
                "validateaddress" => Ok(json!({"isvalid": params[0] == TO})),
                "getblockchaininfo" => Ok(json!({"blocks": 120, "headers": 120, "initialblockdownload": false})),
                // Oldest first, as Core lists them.
                "listtransactions" => Ok(json!([
                    {"txid": "aa", "category": "receive", "amount": 1.0, "confirmations": 3, "time": 5,
                     "address": "Xabc", "vout": 0, "walletconflicts": []},
                    {"txid": "bb", "category": "send", "amount": -0.2, "confirmations": 2, "time": 6,
                     "address": "Xdef", "vout": 1, "walletconflicts": []},
                    {"txid": "cc", "category": "receive", "amount": 0.7, "confirmations": 0, "time": 7,
                     "address": "Xghi", "vout": 0, "walletconflicts": []}
                ])),
                "getwalletinfo" => {
                    let mut w = self.wallet.lock().unwrap();
                    let mut info = json!({"walletname": "wallet.dat", "balance": 1.5, "keypoolsize": 1000, "txcount": w.txcount});
                    if let Some(hd) = &w.hd {
                        info["hdmasterkeyid"] = json!(hd);
                    }
                    if w.pass.is_some() {
                        info["unlocked_until"] = json!(if w.unlocked_until > now { w.unlocked_until } else { 0 });
                    }
                    if self.lock_after_look.swap(false, Ordering::SeqCst) {
                        w.unlocked_until = 0;
                    }
                    Ok(info)
                }
                "walletpassphrase" => {
                    let mut w = self.wallet.lock().unwrap();
                    match w.pass.clone() {
                        None => Err(unencrypted("walletpassphrase")),
                        Some(p) if params[0] != json!(p) => Err(RpcFail::rpc(-14, WRONG_MSG)),
                        Some(_) => {
                            let secs = params[1].as_u64().unwrap();
                            let at = std::time::Instant::now();
                            let near = Duration::from_millis(500);
                            if w.relock_due.is_some_and(|due| at + near >= due && at <= due + near) {
                                w.deadlocked = true;
                            }
                            w.relock_due = Some(at + Duration::from_secs(secs));
                            w.unlocked_until = now + secs;
                            Ok(Value::Null)
                        }
                    }
                }
                "walletlock" => {
                    let mut w = self.wallet.lock().unwrap();
                    if w.pass.is_none() {
                        return Err(unencrypted("walletlock"));
                    }
                    w.unlocked_until = 0;
                    Ok(Value::Null)
                }
                "sendtoaddress" => {
                    let locked = {
                        let w = self.wallet.lock().unwrap();
                        w.pass.is_some() && w.unlocked_until <= now
                    };
                    if locked {
                        Err(RpcFail::rpc(-13, LOCKED_MSG))
                    } else if self.fail_send.load(Ordering::SeqCst) {
                        Err(RpcFail::rpc(-6, "Insufficient funds"))
                    } else {
                        Ok(json!(format!("txid-{n}")))
                    }
                }
                _ => Err(RpcFail::rpc(-32601, "Method not found")),
            }
        })
    }
}

#[derive(Default)]
struct MockEvents(Mutex<Vec<(String, Value)>>);

impl Events for MockEvents {
    fn emit(&self, name: &str, payload: Value) {
        self.0.lock().unwrap().push((name.to_string(), payload));
    }
}

impl MockEvents {
    fn named(&self, name: &str) -> Vec<Value> {
        self.0.lock().unwrap().iter().filter(|(n, _)| n == name).map(|(_, v)| v.clone()).collect()
    }
}

struct H {
    phone: Arc<Phone>,
    rx: mpsc::UnboundedReceiver<Value>,
    rpc: Arc<MockRpc>,
    ev: Arc<MockEvents>,
    now: Arc<AtomicU64>,
    dir: std::path::PathBuf,
}

impl Drop for H {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

static N: AtomicU64 = AtomicU64::new(0);

fn temp_dir(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fb-phone-{what}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("phone")).unwrap();
    dir
}

fn harness(d: Option<&SecretKey>) -> H {
    let dir = temp_dir("test");
    if let Some(d) = d {
        std::fs::write(dir.join("phone/desktop.key"), hex::encode(d.to_bytes())).unwrap();
    }
    let now = Arc::new(AtomicU64::new(1_790_000_000));
    let rpc = Arc::new(MockRpc::new(now.clone()));
    let ev = Arc::new(MockEvents::default());
    let n2 = now.clone();
    let (phone, rx) = Phone::new(&dir, rpc.clone(), ev.clone(), Arc::new(move || n2.load(Ordering::SeqCst))).unwrap();
    H { phone, rx, rpc, ev, now, dir }
}

impl H {
    async fn next(&mut self) -> Value {
        // Long enough for an unlock that waits for the node's relock (RELOCK_MARGIN and more).
        tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await.expect("a frame").unwrap()
    }
    /// Feed a phone frame on `ch`.
    fn feed(&self, ch: u64, d: Value) {
        self.phone.handle_frame(json!({"ch": ch, "d": d}));
    }
    fn d_pub(&self) -> PublicKey {
        crypto::parse_pub(&self.phone.d_pub).unwrap()
    }
    fn at(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
    fn later(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }
    /// Quit and start the app again on the same folder, node and clock.
    fn restart(&mut self) {
        let n2 = self.now.clone();
        let (phone, rx) =
            Phone::new(&self.dir, self.rpc.clone(), self.ev.clone(), Arc::new(move || n2.load(Ordering::SeqCst))).unwrap();
        self.phone = phone;
        self.rx = rx;
    }
    /// Nothing more was sent to any phone.
    fn quiet(&mut self) -> bool {
        self.rx.try_recv().is_err()
    }
}

/// The phone, played in Rust.
struct Sim {
    p: SecretKey,
    ep: SecretKey,
    tx: Option<Direction>,
    rx: Option<Direction>,
}

impl Sim {
    fn new() -> Self {
        Self { p: crypto::random_secret(), ep: crypto::random_secret(), tx: None, rx: None }
    }
    /// The same phone (same device key), for a second session.
    fn twin(&self) -> Self {
        Self { p: self.p.clone(), ep: crypto::random_secret(), tx: None, rx: None }
    }
    fn p_pub(&self) -> String {
        crypto::pub_b64u(&self.p.public_key())
    }
    fn pair_frame(&self, d_pub: &PublicKey, c: &[u8], name: &str) -> Value {
        self.pair_frame_and_code(d_pub, c, name).0
    }
    /// The pair request, and the comparison code this phone shows for it.
    fn pair_frame_and_code(&self, d_pub: &PublicKey, c: &[u8], name: &str) -> (Value, String) {
        let e = crypto::random_secret();
        let k = crypto::pair_key(&e, d_pub, c);
        let n = [3u8; 12];
        let pt = json!({"p": self.p_pub(), "name": name}).to_string();
        let frame = json!({"t": "pair", "e": crypto::pub_b64u(&e.public_key()), "n": crypto::b64u(&n),
               "ct": crypto::b64u(&crypto::seal(&k, &n, pt.as_bytes()))});
        (frame, crypto::pair_code(d_pub, &self.p.public_key(), &e.public_key(), c))
    }
    fn hello(&mut self) -> Value {
        self.ep = crypto::random_secret();
        json!({"t": "hello", "p": self.p_pub(), "e": crypto::pub_b64u(&self.ep.public_key())})
    }
    /// `f` is the phone frame `{"t":"hello-ok",…}`.
    fn hello_ok(&mut self, d_pub: &PublicKey, f: &Value) {
        assert_eq!(f["t"], "hello-ok", "{f}");
        let ed = crypto::parse_pub(f["e"].as_str().unwrap()).unwrap();
        let (k_pd, k_dp) = crypto::phone_session_keys(&self.p, &self.ep, d_pub, &ed);
        self.tx = Some(Direction::new(k_pd));
        self.rx = Some(Direction::new(k_dp));
    }
    fn req(&mut self, id: u64, m: &str, a: Value) -> Value {
        let pt = json!({"id": id, "m": m, "a": a}).to_string();
        json!({"t": "m", "ct": crypto::b64u(&self.tx.as_mut().unwrap().seal(pt.as_bytes()))})
    }
    /// `f` is the phone frame `{"t":"m",…}`.
    fn open(&mut self, f: &Value) -> Value {
        assert_eq!(f["t"], "m", "{f}");
        let ct = crypto::unb64u(f["ct"].as_str().unwrap()).unwrap();
        serde_json::from_slice(&self.rx.as_mut().unwrap().open(&ct).unwrap()).unwrap()
    }
}

fn code_of(url: &str) -> Vec<u8> {
    let frag = url.split("#pair=").nth(1).unwrap();
    let link: Value = serde_json::from_slice(&crypto::unb64u(frag).unwrap()).unwrap();
    crypto::unb64u(link["c"].as_str().unwrap()).unwrap()
}

/// A session for an already paired `sim` on channel `ch`.
async fn session(h: &mut H, sim: &mut Sim, ch: u64) {
    let hello = sim.hello();
    h.feed(ch, hello);
    let ok = h.next().await;
    assert_eq!(ok["ch"], ch);
    sim.hello_ok(&h.d_pub(), &ok["d"]);
}

/// Pair `sim` on channel `ch` (allowing it) and open a session on it.
/// The one pair request waiting, allowed.
fn allow_the_one(h: &H) {
    let asks = h.phone.pair_pending();
    assert_eq!(asks.len(), 1, "one pair request waiting");
    h.phone.pair_answer(&asks[0].id, true).unwrap();
}

async fn paired(h: &mut H, sim: &mut Sim, ch: u64) {
    let (url, _) = h.phone.pair_start().unwrap();
    h.feed(ch, sim.pair_frame(&h.d_pub(), &code_of(&url), "Test phone"));
    allow_the_one(h);
    assert_eq!(h.next().await["d"]["t"], "paired");
    session(h, sim, ch).await;
}

/// Send a request on `ch` and open the reply.
async fn ask(h: &mut H, sim: &mut Sim, ch: u64, id: u64, m: &str, a: Value) -> Value {
    let f = sim.req(id, m, a);
    h.feed(ch, f);
    let r = h.next().await;
    assert_eq!(r["ch"], ch);
    sim.open(&r["d"])
}

async fn limit_left(h: &mut H, sim: &mut Sim, ch: u64) -> f64 {
    ask(h, sim, ch, 999, "status", json!({})).await["ok"]["limit_left"].as_f64().unwrap()
}

fn pass(s: &str) -> Option<Zeroizing<String>> {
    Some(Zeroizing::new(s.to_string()))
}

// ----- the shared vectors --------------------------------------------------------------------

fn vkey(v: &Value, name: &str) -> SecretKey {
    let label = v["keys"][name]["scalar_label"].as_str().unwrap();
    let k = SecretKey::from_slice(&crypto::sha256(label.as_bytes())).unwrap();
    assert_eq!(crypto::pub_b64u(&k.public_key()), v["keys"][name]["pub"], "pub of {name}");
    k
}

fn b(v: &Value) -> Vec<u8> {
    crypto::unb64u(v.as_str().unwrap()).unwrap()
}

fn k32(v: &Value) -> [u8; 32] {
    b(v).try_into().unwrap()
}

/// The pairing vector's comparison code (`pair.sas`), which also matches the value computed for
/// these keys with Python's hashlib, separately from this code, before the vectors carried it:
/// SHA-256("fb-pair-sas-v1" || D || P || E || C) = 821a4951…, 0x821a4951 mod 10^6 = 760785.
fn vector_code(v: &Value) -> String {
    let sas = &v["pair"]["sas"];
    assert_eq!(sas["display"], "760 785");
    assert_eq!(sas["code"].as_str().unwrap(), sas["display"].as_str().unwrap().replace(' ', ""));
    sas["display"].as_str().unwrap().to_string()
}

#[test]
fn vectors_crypto() {
    let v: Value = serde_json::from_str(VECTORS).unwrap();
    let (d, p, e, ep, ed) = (vkey(&v, "D"), vkey(&v, "P"), vkey(&v, "E"), vkey(&v, "eP"), vkey(&v, "eD"));
    assert_eq!(crypto::room(&d.public_key()), v["room"]["room"]);

    // Pairing: the desktop's key, and the phone's ciphertext opens to its plaintext.
    let c = b(&v["pair"]["c"]);
    assert_eq!(crypto::b64u(&crypto::ecdh(&d, &e.public_key())), v["pair"]["ecdh"]);
    let k = crypto::pair_key(&d, &e.public_key(), &c);
    assert_eq!(crypto::b64u(&k), v["pair"]["k"]);
    let n: [u8; 12] = b(&v["pair"]["nonce"]).try_into().unwrap();
    let pt = crypto::open(&k, &n, &b(&v["pair"]["ct"])).unwrap();
    assert_eq!(String::from_utf8(pt.clone()).unwrap(), v["pair"]["plaintext"]);
    assert_eq!(crypto::b64u(&crypto::seal(&k, &n, &pt)), v["pair"]["ct"]);
    // The comparison code (P1) from D_pub, P_pub, E_pub and C.
    assert_eq!(crypto::pair_code(&d.public_key(), &p.public_key(), &e.public_key(), &c), vector_code(&v));

    // Session: dh terms, th, keys.
    let s = &v["session"];
    assert_eq!(crypto::b64u(&crypto::ecdh(&ed, &ep.public_key())), s["dh1"]);
    assert_eq!(crypto::b64u(&crypto::ecdh(&d, &ep.public_key())), s["dh2"]);
    assert_eq!(crypto::b64u(&crypto::ecdh(&ed, &p.public_key())), s["dh3"]);
    let th = crypto::transcript_hash(&p.public_key(), &d.public_key(), &ep.public_key(), &ed.public_key());
    assert_eq!(crypto::b64u(&th), s["th"]);
    let (k_pd, k_dp) = crypto::desktop_session_keys(&d, &ed, &p.public_key(), &ep.public_key());
    assert_eq!(crypto::b64u(&k_pd), s["k_pd"]);
    assert_eq!(crypto::b64u(&k_dp), s["k_dp"]);

    // The host proof (P4): the same D signs "fb-relay-proof-v1" || n; p256 signs deterministically
    // (RFC 6979), so the signature is the vector's own.
    let pv = &v["proof"];
    assert_eq!(pv["context"], "fb-relay-proof-v1");
    let n = b(&pv["n"]);
    assert_eq!(hex::encode(&n), pv["n_hex"]);
    assert_eq!(hex::encode([crypto::PROOF_LABEL, &n].concat()), pv["msg_hex"]);
    assert_eq!(crypto::b64u(&crypto::proof(&d, &n)), pv["sig"]);

    // Framing, both directions, counters from 0.
    let mut rx = Direction::new(k32(&s["k_pd"]));
    for m in s["phone_to_desktop"].as_array().unwrap() {
        assert_eq!(crypto::b64u(&crypto::counter_nonce(m["counter"].as_u64().unwrap())), m["nonce"]);
        let pt = rx.open(&b(&m["ct"])).unwrap();
        assert_eq!(String::from_utf8(pt).unwrap(), m["plaintext"]);
    }
    let mut tx = Direction::new(k_dp);
    for m in s["desktop_to_phone"].as_array().unwrap() {
        let ct = tx.seal(m["plaintext"].as_str().unwrap().as_bytes());
        assert_eq!(crypto::b64u(&ct), m["ct"]);
    }
}

/// The same vectors through the desktop's own frame handling: pair frame, hello, first requests.
#[tokio::test]
async fn vectors_through_the_desktop() {
    let v: Value = serde_json::from_str(VECTORS).unwrap();
    let (d, ed) = (vkey(&v, "D"), vkey(&v, "eD"));
    let mut h = harness(Some(&d));
    assert_eq!(h.phone.room, v["room"]["room"]);
    assert_eq!(h.phone.d_pub, v["room"]["d_pub"]);
    // The relay's challenge answered with the vector's proof frame.
    let challenge = &v["proof"]["challenge_frame"];
    assert_eq!(challenge["t"], "challenge");
    assert_eq!(h.phone.proof_frame(challenge["n"].as_str().unwrap()).unwrap(), v["proof"]["frame"]);

    h.phone.test_code(b(&v["pair"]["c"]).try_into().unwrap());
    h.feed(4, v["pair"]["frame"].clone());
    let asks = h.ev.named(EV_PAIR);
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0]["name"], "iPhone");
    assert_eq!(asks[0]["code"], vector_code(&v), "the comparison code the phone shows");
    allow_the_one(&h);
    assert_eq!(h.next().await, json!({"ch": 4, "d": {"t": "paired"}}));

    let s = &v["session"];
    let p = crypto::parse_pub(s["hello"]["p"].as_str().unwrap()).unwrap();
    let ep = crypto::parse_pub(s["hello"]["e"].as_str().unwrap()).unwrap();
    let dev = store::Device::id_for(&p);
    h.phone.accept_hello(4, &dev, &p, &ep, &ed);
    assert_eq!(h.next().await, json!({"ch": 4, "d": s["hello_ok"]}));

    // The vector's first request is `balance`; the mock node has 1.5 confirmed, 0 pending.
    h.feed(4, s["phone_to_desktop"][0]["frame"].clone());
    let f = h.next().await;
    let mut rx = Direction::new(k32(&s["k_dp"]));
    let got: Value = serde_json::from_slice(&rx.open(&b(&f["d"]["ct"])).unwrap()).unwrap();
    let want: Value = serde_json::from_str(s["desktop_to_phone"][0]["plaintext"].as_str().unwrap()).unwrap();
    assert_eq!(got["id"], want["id"]);
    assert_eq!(got["ok"]["confirmed"].as_f64(), want["ok"]["confirmed"].as_f64());
    assert_eq!(got["ok"]["pending"].as_f64(), want["ok"]["pending"].as_f64());

    // The second is a send to a made-up address: refused, under counter 1.
    h.feed(4, s["phone_to_desktop"][1]["frame"].clone());
    let f = h.next().await;
    let got: Value = serde_json::from_slice(&rx.open(&b(&f["d"]["ct"])).unwrap()).unwrap();
    assert_eq!(got["id"], 2);
    assert!(got["err"].is_string());
    assert!(h.rpc.sends().is_empty());
}

// ----- pairing -------------------------------------------------------------------------------

fn link_of(url: &str) -> Value {
    serde_json::from_slice(&crypto::unb64u(url.split("#pair=").nth(1).unwrap()).unwrap()).unwrap()
}

#[tokio::test]
async fn pairing_url_shape() {
    let h = harness(None);
    // The public relay by default: the page at app.ecxfreebank.com, naming that same relay (the
    // page refuses a link whose relay isn't its own host).
    assert_eq!(h.phone.relay_url(), "wss://app.ecxfreebank.com/ws");
    let (url, exp) = h.phone.pair_start().unwrap();
    assert!(url.starts_with("https://app.ecxfreebank.com/#pair="), "{url}");
    assert_eq!(exp, h.at() + PAIR_TTL_SECS);
    let link = link_of(&url);
    assert_eq!(link["v"], 1);
    assert_eq!(link["relay"], "wss://app.ecxfreebank.com/ws");
    assert_eq!(link["room"], h.phone.room.as_str());
    assert_eq!(link["d"], h.phone.d_pub.as_str());
    assert_eq!(code_of(&url).len(), 16);

    // The override the tests use: a local relay over ws://, its page over http://, same host.
    h.phone.set_relay("ws://127.0.0.1:18480/ws").unwrap();
    let (url, _) = h.phone.pair_start().unwrap();
    assert!(url.starts_with("http://127.0.0.1:18480/#pair="), "{url}");
    assert_eq!(link_of(&url)["relay"], "ws://127.0.0.1:18480/ws");
    h.phone.set_relay("wss://relay.example.org/ws").unwrap();
    let (url, _) = h.phone.pair_start().unwrap();
    assert!(url.starts_with("https://relay.example.org/#pair="), "{url}");
    assert!(h.phone.set_relay("https://x/ws").is_err());
    assert!(h.phone.wanted(), "an open pairing wants the link");
    assert_eq!(h.phone.store.load_config().relay_url, "wss://relay.example.org/ws");
}

#[tokio::test]
async fn pairing_expires() {
    let mut h = harness(None);
    let sim = Sim::new();
    let (url, _) = h.phone.pair_start().unwrap();
    h.later(PAIR_TTL_SECS);
    assert!(!h.phone.wanted(), "an expired pairing doesn't keep the link up");
    h.feed(1, sim.pair_frame(&h.d_pub(), &code_of(&url), "late"));
    assert_eq!(h.next().await, json!({"ch": 1, "d": {"t": "pair-refused"}}));
    assert!(h.ev.named(EV_PAIR).is_empty());
    assert!(h.phone.pair_pending().is_empty());
    assert!(h.phone.pair_answer("nope", true).is_err());
}

/// P1: whoever saw the QR code can ask too. Each request waits with its own comparison code; the
/// owner allows the one their phone shows, and that refuses the rest and spends the code.
#[tokio::test]
async fn pairing_shows_each_request_with_its_own_code() {
    let mut h = harness(None);
    let (intruder, owner) = (Sim::new(), Sim::new());
    let (url, _) = h.phone.pair_start().unwrap();
    let c = code_of(&url);
    let (f1, intruder_code) = intruder.pair_frame_and_code(&h.d_pub(), &c, "Owner's iPhone");
    let (f2, owner_code) = owner.pair_frame_and_code(&h.d_pub(), &c, "Owner's iPhone");
    h.feed(1, f1);
    h.feed(2, f2);
    assert!(h.quiet(), "both wait for the desktop");
    let asks = h.phone.pair_pending();
    assert_eq!(asks.len(), 2);
    assert_eq!(asks.iter().map(|a| a.code.clone()).collect::<Vec<_>>(), [intruder_code.clone(), owner_code.clone()]);
    assert_ne!(asks[0].id, asks[1].id);
    assert_eq!(h.ev.named(EV_PAIR).len(), 2);
    assert_eq!(h.ev.named(EV_PAIR)[1]["code"], owner_code.as_str());
    // The names can't tell them apart; the codes can.
    let mine = asks.iter().find(|a| a.code == owner_code).unwrap();
    h.phone.pair_answer(&mine.id, true).unwrap();
    let mut got = HashMap::new();
    for _ in 0..2 {
        let f = h.next().await;
        got.insert(f["ch"].as_u64().unwrap(), f["d"].clone());
    }
    assert_eq!(got[&2], json!({"t": "paired"}));
    assert_eq!(got[&1], json!({"t": "pair-refused"}), "the rest are refused");
    assert!(h.phone.pair_pending().is_empty());
    let devs = h.phone.devices();
    assert_eq!(devs.len(), 1);
    assert_eq!(devs[0].p_pub, owner.p_pub());
    assert_eq!(devs[0].limit_sats, store::DEFAULT_LIMIT_SATS);
    assert_eq!(h.phone.store.load_devices().devices.len(), 1, "stored on disk");
    // The code is spent.
    h.feed(3, intruder.pair_frame(&h.d_pub(), &c, "again"));
    assert_eq!(h.next().await, json!({"ch": 3, "d": {"t": "pair-refused"}}));
    assert!(h.phone.pair_pending().is_empty());
    assert!(intruder_code != owner_code || intruder.p_pub() != owner.p_pub());
}

#[tokio::test]
async fn pair_requests_are_bounded_and_one_per_channel() {
    let mut h = harness(None);
    let (url, _) = h.phone.pair_start().unwrap();
    let c = code_of(&url);
    let sim = Sim::new();
    // A second request on the same channel replaces the first.
    h.feed(1, sim.pair_frame(&h.d_pub(), &c, "first"));
    h.feed(1, sim.pair_frame(&h.d_pub(), &c, "second"));
    let asks = h.phone.pair_pending();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].name, "second");
    for ch in 2..(MAX_ASKS as u64 + 1) {
        h.feed(ch, Sim::new().pair_frame(&h.d_pub(), &c, "more"));
    }
    assert_eq!(h.phone.pair_pending().len(), MAX_ASKS);
    assert!(h.quiet());
    h.feed(99, Sim::new().pair_frame(&h.d_pub(), &c, "one too many"));
    assert_eq!(h.next().await, json!({"ch": 99, "d": {"t": "pair-refused"}}));
    assert_eq!(h.phone.pair_pending().len(), MAX_ASKS);
}

#[tokio::test]
async fn pair_requests_end_with_their_code() {
    let mut h = harness(None);
    let (url, _) = h.phone.pair_start().unwrap();
    h.feed(1, Sim::new().pair_frame(&h.d_pub(), &code_of(&url), "slow"));
    assert!(h.phone.wanted());
    // A new code: requests for the old one are refused.
    let (url2, _) = h.phone.pair_start().unwrap();
    assert_eq!(h.next().await, json!({"ch": 1, "d": {"t": "pair-refused"}}));
    assert!(h.phone.pair_pending().is_empty());
    h.feed(2, Sim::new().pair_frame(&h.d_pub(), &code_of(&url), "old code"));
    assert_eq!(h.next().await, json!({"ch": 2, "d": {"t": "pair-refused"}}));
    // The code's 5 minutes run out: the request waiting on it is refused.
    h.feed(3, Sim::new().pair_frame(&h.d_pub(), &code_of(&url2), "late"));
    let id = h.phone.pair_pending()[0].id.clone();
    h.later(PAIR_TTL_SECS - 1);
    h.phone.expire();
    assert_eq!(h.phone.pair_pending().len(), 1, "not yet");
    // (Meanwhile the refused phones' channels sat idle, and are closed.)
    let mut closed = vec![h.next().await, h.next().await];
    closed.sort_by_key(|f| f["ch"].as_u64());
    assert_eq!(closed, [json!({"t": "close", "ch": 1}), json!({"t": "close", "ch": 2})]);
    h.later(1);
    // Answering after the time is up refuses it too.
    assert!(h.phone.pair_answer(&id, true).is_err());
    assert_eq!(h.next().await, json!({"ch": 3, "d": {"t": "pair-refused"}}));
    assert!(h.phone.devices().is_empty());
    h.feed(4, Sim::new().pair_frame(&h.d_pub(), &code_of(&url2), "later still"));
    assert_eq!(h.next().await, json!({"ch": 4, "d": {"t": "pair-refused"}}));
    h.feed(5, Sim::new().pair_frame(&h.d_pub(), &code_of(&url2), "expired"));
    h.phone.expire();
    assert!(h.phone.pair_pending().is_empty());
}

#[tokio::test]
async fn pairing_wrong_code_and_deny() {
    let mut h = harness(None);
    let sim = Sim::new();
    let (url, _) = h.phone.pair_start().unwrap();
    // Wrong code: refused, and the right code still works.
    h.feed(1, sim.pair_frame(&h.d_pub(), &[0u8; 16], "x"));
    assert_eq!(h.next().await["d"]["t"], "pair-refused");
    h.feed(2, sim.pair_frame(&h.d_pub(), &code_of(&url), "Mallory\u{7}'s phone with a very long name indeed, far too long"));
    let ask = h.phone.pair_pending().pop().unwrap();
    assert!(!ask.name.contains('\u{7}') && ask.name.chars().count() <= NAME_MAX);
    h.phone.pair_answer(&ask.id, false).unwrap();
    assert_eq!(h.next().await, json!({"ch": 2, "d": {"t": "pair-refused"}}));
    assert!(h.phone.devices().is_empty());
    // Denying one request leaves the code for the phone the owner is holding.
    let owner = Sim::new();
    h.feed(3, owner.pair_frame(&h.d_pub(), &code_of(&url), "mine"));
    assert!(h.quiet());
    allow_the_one(&h);
    assert_eq!(h.next().await, json!({"ch": 3, "d": {"t": "paired"}}));
    assert_eq!(h.phone.devices()[0].name, "mine");
}

#[tokio::test]
async fn pairing_phone_leaves_before_the_answer() {
    let mut h = harness(None);
    let sim = Sim::new();
    let (url, _) = h.phone.pair_start().unwrap();
    h.feed(5, sim.pair_frame(&h.d_pub(), &code_of(&url), "gone"));
    let id = h.phone.pair_pending()[0].id.clone();
    h.phone.handle_frame(json!({"t": "closed", "ch": 5}));
    assert!(h.phone.pair_pending().is_empty());
    assert!(h.phone.pair_answer(&id, true).is_err());
    assert!(h.phone.devices().is_empty());
    assert!(h.quiet());
    // The code still works for a phone that is still there.
    h.feed(6, sim.pair_frame(&h.d_pub(), &code_of(&url), "back"));
    allow_the_one(&h);
    assert_eq!(h.next().await, json!({"ch": 6, "d": {"t": "paired"}}));
}

/// P5: a channel that hasn't completed hello or a pair request within 30 seconds of its first
/// frame is closed, so a stranger with the room id can't hold the room's phone slots.
#[tokio::test]
async fn idle_channels_are_closed_after_30_seconds() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    // A stranger's hello (an unknown key) is denied; garbage gets nothing.
    let mut stranger = Sim::new();
    let hello = stranger.hello();
    h.feed(9, hello);
    assert_eq!(h.next().await, json!({"ch": 9, "d": {"t": "denied"}}));
    h.feed(10, json!({"t": "nonsense"}));
    // More frames don't restart the clock.
    h.later(20);
    let hello = stranger.hello();
    h.feed(9, hello);
    assert_eq!(h.next().await["d"]["t"], "denied");
    h.later(IDLE_CHANNEL_SECS - 21);
    h.phone.expire();
    assert!(h.quiet(), "not yet");
    h.later(1);
    h.phone.expire();
    let mut closed = vec![h.next().await, h.next().await];
    closed.sort_by_key(|f| f["ch"].as_u64());
    assert_eq!(closed, [json!({"t": "close", "ch": 9}), json!({"t": "close", "ch": 10})]);
    h.phone.expire();
    assert!(h.quiet(), "once");
    assert!(h.phone.online(&h.phone.devices()[0].id), "the paired phone's session stays");

    // A waiting pair request keeps its channel while it waits; once answered, 30 s to go.
    let (url, _) = h.phone.pair_start().unwrap();
    h.feed(11, Sim::new().pair_frame(&h.d_pub(), &code_of(&url), "waiting"));
    h.later(60);
    h.phone.expire();
    assert!(h.quiet());
    let id = h.phone.pair_pending()[0].id.clone();
    h.phone.pair_answer(&id, false).unwrap();
    assert_eq!(h.next().await, json!({"ch": 11, "d": {"t": "pair-refused"}}));
    h.later(IDLE_CHANNEL_SECS);
    h.phone.expire();
    assert_eq!(h.next().await, json!({"t": "close", "ch": 11}));

    // A session that ends (a bad frame) gives its channel 30 s to say hello again.
    h.feed(1, json!({"t": "m", "ct": "AAAA"}));
    assert_eq!(h.next().await, json!({"ch": 1, "d": {"t": "closed"}}));
    h.later(IDLE_CHANNEL_SECS - 1);
    session(&mut h, &mut sim, 1).await;
    h.later(5);
    h.phone.expire();
    assert!(h.quiet(), "a hello in time keeps it");
    // A channel the relay says is gone is forgotten.
    h.feed(12, json!({"t": "nonsense"}));
    h.phone.handle_frame(json!({"t": "closed", "ch": 12}));
    h.later(IDLE_CHANNEL_SECS);
    h.phone.expire();
    assert!(h.quiet());
}

/// P6: names are shown and stored without control, bidirectional or zero-width characters, and
/// at most 40 characters long.
#[test]
fn device_names_are_cleaned() {
    assert_eq!(clean_name("  Mum's iPhone  "), "Mum's iPhone");
    // A right-to-left override can make a name read as another; zero-width ones hide in it.
    assert_eq!(clean_name("Evil\u{202E}enohPi"), "EvilenohPi");
    assert_eq!(clean_name("i\u{200B}Ph\u{200D}one\u{FEFF}\u{2066}x\u{2069}\u{200E}\u{061C}"), "iPhonex");
    assert_eq!(clean_name("line\nbreak\u{7}\u{0}\u{85}"), "linebreak");
    assert_eq!(clean_name("a\u{2028}b\u{E0041}c\u{00AD}d\u{2060}"), "abcd");
    assert_eq!(clean_name(&"x".repeat(50)).chars().count(), NAME_MAX);
    // Counted in characters, not bytes, and trimmed after the cut.
    assert_eq!(clean_name(&"ü".repeat(45)), "ü".repeat(40));
    assert_eq!(clean_name(&format!("{} y", "x".repeat(39))), "x".repeat(39));
    assert_eq!(clean_name("\u{202E}\u{200B} \n"), "Phone");
    // Accents and emoji stay.
    assert_eq!(clean_name("Zoë's 📱"), "Zoë's 📱");
}

#[tokio::test]
async fn names_are_cleaned_before_they_are_shown_or_stored() {
    let mut h = harness(None);
    let (url, _) = h.phone.pair_start().unwrap();
    let dirty = format!("Bank\u{202E}lagel\u{200B} {}", "y".repeat(60));
    h.feed(1, Sim::new().pair_frame(&h.d_pub(), &code_of(&url), &dirty));
    let ask = h.phone.pair_pending().pop().unwrap();
    assert_eq!(ask.name, clean_name(&dirty));
    assert!(!ask.name.contains('\u{202E}') && ask.name.chars().count() <= NAME_MAX);
    assert_eq!(h.ev.named(EV_PAIR)[0]["name"], ask.name.as_str());
    allow_the_one(&h);
    assert_eq!(h.phone.store.load_devices().devices[0].name, clean_name(&dirty));
    // A name stored before this cleaning is cleaned when it is loaded.
    let mut devs = h.phone.store.load_devices();
    devs.devices[0].name = "Old\u{202E}name".into();
    h.phone.store.save_devices(&devs).unwrap();
    h.restart();
    assert_eq!(h.phone.devices()[0].name, "Oldname");
}

/// P4: the relay's challenge is 32 bytes; the proof signs the context string and it.
#[tokio::test]
async fn proof_frame_wants_a_32_byte_challenge() {
    use p256::ecdsa::signature::Verifier;
    let h = harness(None);
    assert!(h.phone.proof_frame(&crypto::b64u(&[1u8; 31])).is_err());
    assert!(h.phone.proof_frame("not base64url!").is_err());
    let n = [5u8; 32];
    let f = h.phone.proof_frame(&crypto::b64u(&n)).unwrap();
    assert_eq!(f["t"], "proof");
    let sig = p256::ecdsa::Signature::from_slice(&crypto::unb64u(f["sig"].as_str().unwrap()).unwrap()).unwrap();
    let vk = p256::ecdsa::VerifyingKey::from(&h.d_pub());
    vk.verify(&[b"fb-relay-proof-v1".as_slice(), &n].concat(), &sig).unwrap();
}

// ----- sessions and revoke -------------------------------------------------------------------

#[tokio::test]
async fn receive_waits_until_a_new_wallet_is_protected() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 7).await;
    // A new wallet (no transactions yet) without its passphrase and recovery words: no address,
    // and none is made.
    h.rpc.wallet.lock().unwrap().txcount = 0;
    let r = ask(&mut h, &mut sim, 7, 1, "receive", json!({})).await;
    assert_eq!(r, json!({"id": 1, "err": crate::recovery::WALLET_NOT_SET_UP}));
    assert!(!h.rpc.methods().contains(&"getnewaddress".to_string()));
    // A passphrase alone isn't enough: FreeBank's words must give its seed.
    h.rpc.encrypt(PASS);
    let r = ask(&mut h, &mut sim, 7, 2, "receive", json!({})).await;
    assert_eq!(r["err"], json!(crate::recovery::WALLET_NOT_SET_UP));
    // Protected: the words saved in the app's folder give the seed the wallet has.
    let e = crate::seed::new_entropy();
    let id = crate::seed::key_id(&crate::seed::freebank_hd_seed(&e).unwrap()).unwrap();
    let quick = crate::seed::Kdf { m_kib: 64, t: 1, p: 1 };
    crate::seed::write_private(&crate::seed::seed_path(&h.dir), &crate::seed::seal(&e, &id, PASS, quick).unwrap()).unwrap();
    h.rpc.wallet.lock().unwrap().hd = Some(crate::seed::key_id_hex(&id));
    let r = ask(&mut h, &mut sim, 7, 3, "receive", json!({})).await;
    assert_eq!(r, json!({"id": 3, "ok": {"address": TO}}));
    // An older wallet without protection (it has transactions) still gives addresses.
    let mut h2 = harness(None);
    let mut sim2 = Sim::new();
    paired(&mut h2, &mut sim2, 7).await;
    let r = ask(&mut h2, &mut sim2, 7, 1, "receive", json!({})).await;
    assert_eq!(r, json!({"id": 1, "ok": {"address": TO}}));
}

#[tokio::test]
async fn unknown_phone_is_denied() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    let hello = sim.hello();
    h.feed(1, hello);
    assert_eq!(h.next().await, json!({"ch": 1, "d": {"t": "denied"}}));
    // A message with no session: closed.
    h.feed(1, json!({"t": "m", "ct": "AAAA"}));
    assert_eq!(h.next().await, json!({"ch": 1, "d": {"t": "closed"}}));
}

#[tokio::test]
async fn door_methods_and_replay() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 7).await;
    let id = h.phone.devices()[0].id.clone();
    assert!(h.phone.online(&id));

    let r = ask(&mut h, &mut sim, 7, 1, "receive", json!({})).await;
    assert_eq!(r, json!({"id": 1, "ok": {"address": TO}}));

    let r = ask(&mut h, &mut sim, 7, 2, "history", json!({"count": 500})).await;
    assert_eq!(r["ok"][0]["txid"], "cc");
    assert!(r["ok"][0].get("vout").is_none(), "only the listed fields");
    let lt = h.rpc.params_of("listtransactions")[0].clone();
    assert_eq!(lt, vec![json!("*"), json!(50)], "count capped at 50");

    let r = ask(&mut h, &mut sim, 7, 3, "status", json!({})).await;
    assert_eq!(r["ok"], json!({"blocks": 120, "synced": true, "limit_left": 0.1, "face_id": false, "face_id_sends": false}));

    let r = ask(&mut h, &mut sim, 7, 4, "dumpprivkey", json!({})).await;
    assert_eq!(r, json!({"id": 4, "err": "unknown method"}));

    // A replayed frame (an old counter) ends the session.
    let f = sim.req(5, "balance", json!({}));
    h.feed(7, f.clone());
    sim.open(&h.next().await["d"]);
    h.feed(7, f);
    assert_eq!(h.next().await, json!({"ch": 7, "d": {"t": "closed"}}));
    assert!(!h.phone.online(&id));
    // A fresh hello works again.
    session(&mut h, &mut sim, 7).await;
    assert_eq!(ask(&mut h, &mut sim, 7, 6, "balance", json!({})).await["id"], 6);
    assert!(h.phone.devices()[0].last_seen.is_some());
}

#[tokio::test]
async fn history_is_newest_first() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 1, "history", json!({"count": 10})).await;
    let list = r["ok"].as_array().unwrap();
    let txids: Vec<&str> = list.iter().map(|t| t["txid"].as_str().unwrap()).collect();
    assert_eq!(txids, ["cc", "bb", "aa"], "the node lists oldest first; the phone gets newest first");
    let times: Vec<u64> = list.iter().map(|t| t["time"].as_u64().unwrap()).collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]));
    assert_eq!(list[1], json!({"txid": "bb", "category": "send", "amount": -0.2, "confirmations": 2, "time": 6, "address": "Xdef"}));
}

#[tokio::test]
async fn revoke_cuts_the_phone_off() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 2).await;
    let id = h.phone.devices()[0].id.clone();
    h.phone.revoke(&id).unwrap();
    assert_eq!(h.next().await, json!({"ch": 2, "d": {"t": "denied"}}));
    assert_eq!(h.next().await, json!({"t": "close", "ch": 2}));
    let f = sim.req(1, "balance", json!({}));
    h.feed(2, f);
    assert_eq!(h.next().await["d"]["t"], "closed");
    let hello = sim.hello();
    h.feed(3, hello);
    assert_eq!(h.next().await, json!({"ch": 3, "d": {"t": "denied"}}));
    assert!(h.phone.store.load_devices().devices.is_empty());
    assert!(!h.phone.wanted());
    assert!(h.phone.revoke(&id).is_err());
}

// ----- sends ---------------------------------------------------------------------------------

#[tokio::test]
async fn sends_within_the_limit_go_out() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.04})).await;
    assert!(r["ok"]["txid"].as_str().unwrap().starts_with("txid-"), "{r}");
    assert_eq!(h.rpc.sends(), vec![vec![json!(TO), json!(0.04)]]);
    let left = limit_left(&mut h, &mut sim, 1).await;
    assert!((left - 0.06).abs() < 1e-12);

    // A failed send says why in plain words and gives the allowance back.
    h.rpc.fail_send.store(true, Ordering::SeqCst);
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.05})).await;
    assert_eq!(r["err"], NOT_ENOUGH);
    assert_eq!(h.phone.devices()[0].left_on(store::day_of(h.at())), 6_000_000);

    // Bad input never reaches the node.
    for a in [
        json!({"address": "1abc", "amount": 0.01}),
        json!({"address": TO, "amount": -1}),
        json!({"address": TO, "amount": 0}),
        json!({"address": TO, "amount": "0.01"}),
        json!({"address": TO, "amount": 0.000000001}),
        json!({"address": "XkPvjfFq8Hj9wy9Wq3pRr1sBdXJxZ5nT2n", "amount": 0.01}),
    ] {
        assert!(ask(&mut h, &mut sim, 1, 9, "send", a).await["err"].is_string());
    }
    assert_eq!(h.rpc.sends().len(), 2);
    let log = h.phone.store.recent_sends(10);
    assert_eq!(log.len(), 2);
    assert_eq!(log[0]["result"], "failed");
    assert_eq!(log[1]["result"], "sent");
    assert_eq!(h.ev.named(EV_SEND).len(), 2);
    // An unencrypted wallet is never unlocked or locked.
    assert!(!h.rpc.methods().iter().any(|m| m == "walletpassphrase" || m == "walletlock"));
}

#[tokio::test]
async fn held_send_confirmed_on_the_desktop() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.05).unwrap();
    let r = ask(&mut h, &mut sim, 1, 11, "send", json!({"address": TO, "amount": 0.06})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    assert_eq!(r, json!({"id": 11, "ok": {"pending": confirm, "why": "limit"}}));
    assert!(h.rpc.sends().is_empty());
    let held = h.phone.held();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].name, "Test phone");
    assert_eq!(held[0].amount, 0.06);
    assert_eq!(held[0].why, "limit");
    assert_eq!(held[0].expires, h.at() + HELD_TTL_SECS);
    let ev = &h.ev.named(EV_HELD)[0];
    assert_eq!(ev["confirm"], confirm.as_str());
    assert_eq!(ev["why"], "limit");

    let c = h.phone.confirm_send(&confirm, true, None).await.unwrap();
    let txid = c.txid.clone().unwrap();
    assert!(!c.need_passphrase);
    let r = sim.open(&h.next().await["d"]);
    assert_eq!(r, json!({"id": 11, "pending": confirm, "ok": {"txid": txid}}), "the final reply repeats the confirm id");
    assert_eq!(h.rpc.sends(), vec![vec![json!(TO), json!(0.06)]]);
    assert!(h.phone.held().is_empty());
    assert!(h.phone.confirm_send(&confirm, true, None).await.is_err(), "answered once");
    // A confirmed send leaves the phone's own allowance alone.
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.05);
}

/// With the app closed (the background part), nobody can confirm: a send the app would hold is
/// refused at once, and nothing is held or sent. Within the limit, sends still go out.
#[tokio::test]
async fn with_the_app_closed_over_the_limit_is_refused() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.phone.set_background(true);
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.05).unwrap();
    let r = ask(&mut h, &mut sim, 1, 11, "send", json!({"address": TO, "amount": 0.06})).await;
    assert_eq!(r, json!({"id": 11, "err": ERR_CLOSED_LIMIT}));
    assert!(h.phone.held().is_empty() && h.rpc.sends().is_empty());
    assert!(h.ev.named(EV_HELD).is_empty());
    let r = ask(&mut h, &mut sim, 1, 12, "send", json!({"address": TO, "amount": 0.04})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert_eq!(h.rpc.sends(), vec![vec![json!(TO), json!(0.04)]]);
}

#[tokio::test]
async fn held_send_declined_zero_limit_and_revoke() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.0).unwrap();
    let r = ask(&mut h, &mut sim, 1, 21, "send", json!({"address": TO, "amount": 0.00000001})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    assert_eq!(
        h.phone.confirm_send(&confirm, false, None).await.unwrap(),
        Confirmed { txid: None, need_passphrase: false }
    );
    assert_eq!(sim.open(&h.next().await["d"]), json!({"id": 21, "pending": confirm, "err": "declined on the desktop"}));
    assert!(h.rpc.sends().is_empty());
    // One line per send, with its latest state.
    let log = h.phone.store.recent_sends(10);
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!((log[0]["result"].as_str(), log[0]["held"].as_str()), (Some("declined"), Some(confirm.as_str())));

    // A held send is dropped when the phone is revoked.
    ask(&mut h, &mut sim, 1, 22, "send", json!({"address": TO, "amount": 1.0})).await;
    assert_eq!(h.phone.held().len(), 1);
    h.phone.revoke(&id).unwrap();
    assert!(h.phone.held().is_empty());
    assert!(h.phone.store.load_held().held.is_empty());
    assert_eq!(h.phone.store.recent_sends(1)[0]["result"], "declined");
}

#[tokio::test]
async fn limit_ledger_rolls_over_the_day() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.1})).await;
    assert!(r["ok"]["txid"].is_string());
    let r = ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 0.01})).await;
    assert!(r["ok"]["pending"].is_string());
    h.later(86_400);
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.01})).await;
    assert!(r["ok"]["txid"].is_string());
}

// ----- held sends: expiry, held.json, restarts -----------------------------------------------

#[test]
fn expiry_wording() {
    assert_eq!(expired_text(HELD_TTL_SECS), "not confirmed on the desktop within 10 minutes");
    assert_eq!(expired_text(60), "not confirmed on the desktop within 1 minute");
    assert_eq!(expired_text(20), "not confirmed on the desktop within 20 seconds");
}

#[tokio::test]
async fn held_send_expires_after_ten_minutes() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    // A second live session of the same phone hears the final reply too.
    let mut sim2 = sim.twin();
    session(&mut h, &mut sim2, 2).await;
    let r = ask(&mut h, &mut sim, 1, 31, "send", json!({"address": TO, "amount": 0.5})).await;
    assert_eq!(r["ok"]["why"], "limit");
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();

    h.later(HELD_TTL_SECS - 1);
    h.phone.expire();
    assert_eq!(h.phone.held().len(), 1, "not yet");
    assert!(h.quiet());

    let changed = h.ev.named(EV_CHANGED).len();
    h.later(1);
    h.phone.expire();
    assert!(h.phone.held().is_empty());
    let want = json!({"id": 31, "pending": confirm, "err": "not confirmed on the desktop within 10 minutes"});
    let mut got = HashMap::new();
    for _ in 0..2 {
        let f = h.next().await;
        got.insert(f["ch"].as_u64().unwrap(), f["d"].clone());
    }
    assert_eq!(sim.open(&got[&1]), want);
    assert_eq!(sim2.open(&got[&2]), want);
    assert!(h.quiet(), "once");
    assert!(h.ev.named(EV_CHANGED).len() > changed, "the desktop's alert for it goes away");
    assert_eq!(h.phone.store.recent_sends(1)[0]["result"], "expired");
    assert!(h.phone.store.load_held().held.is_empty(), "gone from held.json");
    assert!(h.phone.confirm_send(&confirm, true, None).await.is_err(), "too late to confirm");
    assert!(h.rpc.sends().is_empty());
}

#[tokio::test]
async fn held_send_expiry_can_be_shortened_for_tests() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.phone.set_held_ttl(20);
    let r = ask(&mut h, &mut sim, 1, 5, "send", json!({"address": TO, "amount": 0.5})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    assert_eq!(h.phone.held()[0].expires, h.at() + 20);
    h.later(20);
    // The screens expire it too when they look.
    assert!(h.phone.held().is_empty());
    assert_eq!(
        sim.open(&h.next().await["d"]),
        json!({"id": 5, "pending": confirm, "err": "not confirmed on the desktop within 20 seconds"})
    );
    h.phone.set_held_ttl(10_000);
    let r = ask(&mut h, &mut sim, 1, 6, "send", json!({"address": TO, "amount": 0.5})).await;
    assert!(r["ok"]["pending"].is_string());
    assert_eq!(h.phone.held()[0].expires, h.at() + HELD_TTL_SECS, "never longer than 10 minutes");
}

#[tokio::test]
async fn held_sends_are_saved_privately() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 41, "send", json!({"address": TO, "amount": 0.2})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    let saved = h.phone.store.load_held();
    assert_eq!(saved.held.len(), 1);
    let s = &saved.held[0];
    assert_eq!((s.confirm.as_str(), s.sats, s.why.as_str(), s.at), (confirm.as_str(), 20_000_000, "limit", h.at()));
    assert_eq!(s.req_id, 41);
    assert_eq!(s.address, TO);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(h.dir.join("phone/held.json")).unwrap().permissions().mode();
        assert_eq!(m & 0o777, 0o600);
    }
    assert!(!h.dir.join("phone/.held.json.tmp").exists(), "written by rename");
    h.phone.confirm_send(&confirm, true, None).await.unwrap();
    assert!(h.phone.store.load_held().held.is_empty());
}

#[tokio::test]
async fn restart_cancels_held_sends_and_tells_each_new_session() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 41, "send", json!({"address": TO, "amount": 0.2})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    // A second held send, from later: its notice outlives the first one's.
    h.later(120);
    let r = ask(&mut h, &mut sim, 1, 42, "send", json!({"address": TO, "amount": 0.3})).await;
    let confirm2 = r["ok"]["pending"].as_str().unwrap().to_string();

    h.later(60);
    h.restart();
    assert!(h.phone.held().is_empty(), "cancelled, not held");
    assert!(h.phone.confirm_send(&confirm, true, None).await.is_err(), "and never paid");
    assert!(h.rpc.sends().is_empty());
    let log = h.phone.store.recent_sends(2);
    assert!(log.iter().all(|l| l["result"] == "cancelled" && l["detail"] == ERR_RESTARTED), "{log:?}");
    assert_eq!(h.phone.store.load_held().cancelled.len(), 2);

    // Every new session of that phone hears both, until each one's 10 minutes are up.
    let want = |id: u64, c: &str| json!({"id": id, "pending": c, "err": "the desktop app restarted; nothing was sent"});
    for ch in [5, 6] {
        session(&mut h, &mut sim, ch).await;
        assert_eq!(sim.open(&h.next().await["d"]), want(41, &confirm));
        assert_eq!(sim.open(&h.next().await["d"]), want(42, &confirm2));
        assert!(h.quiet());
    }
    // The session works as usual after them.
    assert_eq!(ask(&mut h, &mut sim, 6, 7, "balance", json!({})).await["ok"]["confirmed"], 1.5);

    // Another phone isn't told.
    let mut other = Sim::new();
    paired(&mut h, &mut other, 7).await;
    assert!(h.quiet());

    // Restarting again within the 10 minutes still owes the notices; they aren't logged twice.
    h.restart();
    let raw = std::fs::read_to_string(h.phone.store.dir.join("sends.log")).unwrap();
    assert_eq!(raw.lines().filter(|l| l.contains(r#""result":"cancelled""#)).count(), 2, "{raw}");
    session(&mut h, &mut sim, 8).await;
    assert_eq!(sim.open(&h.next().await["d"]), want(41, &confirm));
    assert_eq!(sim.open(&h.next().await["d"]), want(42, &confirm2));

    // The first one's time runs out (held at t0, now t0 + 10 min): only the second is told.
    h.later(HELD_TTL_SECS - 180);
    h.phone.expire();
    session(&mut h, &mut sim, 9).await;
    assert_eq!(sim.open(&h.next().await["d"]), want(42, &confirm2));
    assert!(h.quiet());
    assert_eq!(h.phone.store.load_held().cancelled.len(), 1);

    // Then neither.
    h.later(120);
    h.phone.expire();
    session(&mut h, &mut sim, 10).await;
    assert!(h.quiet());
    assert!(h.phone.store.load_held().cancelled.is_empty());
    assert!(h.rpc.sends().is_empty(), "a held send is never paid after a restart");
}

#[tokio::test]
async fn revoke_drops_restart_notices() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.5})).await;
    h.restart();
    assert_eq!(h.phone.store.load_held().cancelled.len(), 1);
    let id = h.phone.devices()[0].id.clone();
    h.phone.revoke(&id).unwrap();
    assert!(h.phone.store.load_held().cancelled.is_empty());
}

// ----- encrypted wallets ---------------------------------------------------------------------

#[tokio::test]
async fn unencrypted_wallet_needs_no_passphrase() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    assert_eq!(h.phone.wallet_view().await, WalletView { encrypted: Some(false), locked: false, phone_send: false });
    assert_eq!(h.phone.phone_send_on(Zeroizing::new(PASS.into())).await, Err(ERR_NOT_ENCRYPTED.to_string()));
    assert!(!h.phone.phone_send_is_on());
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.01})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert!(!h.rpc.methods().iter().any(|m| m == "walletpassphrase" || m == "walletlock"));
}

#[tokio::test]
async fn locked_wallet_holds_phone_sends_until_they_are_turned_on() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    assert_eq!(h.phone.wallet_view().await, WalletView { encrypted: Some(true), locked: true, phone_send: false });
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.01})).await;
    let confirm = r["ok"]["pending"].as_str().expect("held").to_string();
    assert_eq!(r, json!({"id": 3, "ok": {"pending": confirm, "why": "locked"}}));
    assert_eq!(h.phone.held()[0].why, "locked");
    assert!(h.rpc.sends().is_empty());
    assert!(h.rpc.params_of("walletpassphrase").is_empty(), "no passphrase, no unlock");
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.1, "a held send takes nothing from the limit");
    assert_eq!(h.phone.store.recent_sends(1)[0]["detail"]["why"], "locked");
}

#[tokio::test]
async fn turning_phone_sends_on_checks_the_passphrase() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    assert_eq!(h.phone.phone_send_on(Zeroizing::new("wrong".into())).await, Err(ERR_WRONG_PASSPHRASE.to_string()));
    assert!(h.phone.phone_send_on(Zeroizing::new(String::new())).await.is_err());
    assert!(!h.phone.phone_send_is_on());
    assert!(!h.rpc.unlocked());

    h.rpc.forget_calls();
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    assert!(h.phone.phone_send_is_on());
    // Checked by unlocking for one second, then locked again.
    assert_eq!(h.rpc.methods(), ["getwalletinfo", "walletpassphrase", "walletlock"]);
    assert_eq!(h.rpc.params_of("walletpassphrase")[0], vec![json!(PASS), json!(1)]);
    assert!(!h.rpc.unlocked());
    assert_eq!(h.phone.wallet_view().await, WalletView { encrypted: Some(true), locked: true, phone_send: true });
}

#[tokio::test]
async fn phone_send_unlocks_for_the_send_and_locks_after() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    h.rpc.forget_calls();
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert_eq!(h.rpc.methods(), ["validateaddress", "getwalletinfo", "walletpassphrase", "sendtoaddress", "walletlock"]);
    assert_eq!(h.rpc.params_of("walletpassphrase")[0], vec![json!(PASS), json!(SEND_UNLOCK_SECS)]);
    assert!(!h.rpc.unlocked(), "locked again");
    assert!((limit_left(&mut h, &mut sim, 1).await - 0.08).abs() < 1e-12);
}

#[tokio::test]
async fn an_unlocked_wallet_is_left_unlocked() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    let until = h.at() + 300;
    h.rpc.unlock_until(until);
    assert_eq!(h.phone.wallet_view().await.locked, false);

    // Turning phone sends on checks the passphrase by unlocking again until the same time.
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    assert_eq!(h.rpc.params_of("walletpassphrase")[0], vec![json!(PASS), json!(300)]);
    assert!(h.rpc.params_of("walletlock").is_empty());
    assert!(h.rpc.unlocked());

    // A send neither unlocks nor locks it.
    h.rpc.forget_calls();
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string());
    assert_eq!(h.rpc.methods(), ["validateaddress", "getwalletinfo", "sendtoaddress"]);
    assert!(h.rpc.unlocked(), "still unlocked, until {until}");

    // Nor does one with phone sends off.
    h.phone.forget_passphrase();
    let r = ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string());
    assert!(h.rpc.unlocked());

    // Once its time is up, the wallet is locked again, and a phone send is held.
    h.later(300);
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.02})).await;
    assert_eq!(r["ok"]["why"], "locked");
}

#[tokio::test]
async fn a_wallet_that_locks_itself_before_the_send_is_looked_at_again() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    // Unlocked when the desktop looks, locked by the time it sends: it looks again and unlocks.
    h.rpc.unlock_until(h.at() + 300);
    h.rpc.lock_after_look.store(true, Ordering::SeqCst);
    h.rpc.forget_calls();
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert_eq!(
        h.rpc.methods(),
        ["validateaddress", "getwalletinfo", "sendtoaddress", "getwalletinfo", "walletpassphrase", "sendtoaddress", "walletlock"]
    );
    assert!(!h.rpc.unlocked());

    // With phone sends off it can't unlock: the send is held.
    h.phone.forget_passphrase();
    h.rpc.unlock_until(h.at() + 300);
    h.rpc.lock_after_look.store(true, Ordering::SeqCst);
    let r = ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 0.02})).await;
    assert_eq!(r["ok"]["why"], "locked", "{r}");
    assert!((limit_left(&mut h, &mut sim, 1).await - 0.08).abs() < 1e-12, "only the first counts");
}

/// The mock deadlocks where freebankd does: an unlock right as the last unlock's relock fires.
#[tokio::test]
async fn the_mock_node_deadlocks_like_freebankd() {
    let rpc = MockRpc::new(Arc::new(AtomicU64::new(1_790_000_000)));
    rpc.encrypt(PASS);
    rpc.call("walletpassphrase", vec![json!(PASS), json!(1)]).await.unwrap();
    rpc.call("walletlock", vec![]).await.unwrap();
    rpc.call("walletpassphrase", vec![json!(PASS), json!(10)]).await.unwrap();
    assert!(!rpc.deadlocked(), "replacing a timer long before it fires is safe");
    let rpc2 = MockRpc::new(Arc::new(AtomicU64::new(1_790_000_000)));
    rpc2.encrypt(PASS);
    rpc2.call("walletpassphrase", vec![json!(PASS), json!(1)]).await.unwrap();
    rpc2.call("walletlock", vec![]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    rpc2.call("walletpassphrase", vec![json!(PASS), json!(10)]).await.unwrap();
    assert!(rpc2.deadlocked(), "an unlock as the relock fires");
}

/// Turning phone sends on unlocks for a second; a send just before that second is up waits for
/// the node's relock instead of deadlocking it (this hung the real node once, 2026-09-29).
#[tokio::test]
async fn unlocks_keep_clear_of_the_nodes_relock() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(900)).await;
    let t0 = std::time::Instant::now();
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert!(!h.rpc.deadlocked(), "the unlock ran into the relock");
    assert!(t0.elapsed() >= Duration::from_millis(1500), "it waited for the relock: {:?}", t0.elapsed());
    // The next relock is 10 s away: an unlock now replaces it long before it fires, at once.
    let t1 = std::time::Instant::now();
    let r = ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 0.02})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert!(t1.elapsed() < Duration::from_millis(1500), "no wait for a far relock: {:?}", t1.elapsed());
    assert!(!h.rpc.deadlocked());
    // A confirm on the desktop as that relock comes due waits too.
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.0).unwrap();
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.02})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_secs(SEND_UNLOCK_SECS) - t1.elapsed() - Duration::from_millis(100)).await;
    assert!(h.phone.confirm_send(&confirm, true, None).await.unwrap().txid.is_some());
    assert!(!h.rpc.deadlocked(), "the confirm's unlock ran into the relock");
    assert!(!h.rpc.unlocked());
}

#[tokio::test]
async fn turning_phone_sends_off_forgets_the_passphrase() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    let changed = h.ev.named(EV_CHANGED).len();
    h.phone.forget_passphrase();
    assert!(!h.phone.phone_send_is_on());
    assert!(h.ev.named(EV_CHANGED).len() > changed);
    h.rpc.forget_calls();
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.01})).await;
    assert_eq!(r["ok"]["why"], "locked");
    assert!(h.rpc.params_of("walletpassphrase").is_empty());
}

/// Seen end to end on beta (2026-09-29): with the only phone revoked, Settings hid the switch, but the
/// passphrase stayed in memory and the next phone paired could send at once without it.
#[tokio::test]
async fn revoking_the_last_phone_turns_phone_sends_off() {
    let mut h = harness(None);
    let (mut first, mut second) = (Sim::new(), Sim::new());
    paired(&mut h, &mut first, 1).await;
    paired(&mut h, &mut second, 2).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    let ids: Vec<String> = h.phone.devices().iter().map(|d| d.id.clone()).collect();
    assert_eq!(ids.len(), 2);
    for (i, id) in ids.iter().enumerate() {
        h.phone.revoke(id).unwrap();
        assert_eq!(h.next().await["d"]["t"], "denied");
        assert_eq!(h.next().await["t"], "close");
        if i == 0 {
            assert!(h.phone.phone_send_is_on(), "another phone is still paired");
        }
    }
    assert!(!h.phone.phone_send_is_on());
    assert_eq!(h.phone.wallet_view().await, WalletView { encrypted: Some(true), locked: true, phone_send: false });

    // A phone paired afterwards waits for the desktop until phone sends are turned on again.
    let mut third = Sim::new();
    paired(&mut h, &mut third, 3).await;
    h.rpc.forget_calls();
    let r = ask(&mut h, &mut third, 3, 1, "send", json!({"address": TO, "amount": 0.01})).await;
    assert_eq!(r["ok"]["why"], "locked");
    assert!(h.rpc.params_of("walletpassphrase").is_empty());
}

#[tokio::test]
async fn a_changed_passphrase_turns_phone_sends_off() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    h.rpc.encrypt("a new passphrase");
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.01})).await;
    assert_eq!(r["ok"]["why"], "locked", "{r}");
    assert!(!h.phone.phone_send_is_on());
    assert!(h.rpc.sends().is_empty());
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.1);
}

#[tokio::test]
async fn an_empty_wallet_fails_clearly_and_is_locked_again() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    h.rpc.fail_send.store(true, Ordering::SeqCst);
    let r = ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.01})).await;
    assert_eq!(r, json!({"id": 1, "err": NOT_ENOUGH}));
    assert!(!h.rpc.unlocked());
    assert_eq!(h.rpc.methods().last().unwrap(), "walletlock");
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.1, "the allowance comes back");
    assert_eq!(h.phone.store.recent_sends(1)[0]["result"], "failed");
}

#[tokio::test]
async fn confirming_on_a_locked_wallet_asks_for_the_passphrase_for_that_send() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    let r = ask(&mut h, &mut sim, 1, 8, "send", json!({"address": TO, "amount": 0.03})).await;
    assert_eq!(r["ok"]["why"], "locked");
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();

    // Without a passphrase: nothing happens, and the desktop is asked for one.
    let c = h.phone.confirm_send(&confirm, true, None).await.unwrap();
    assert_eq!(c, Confirmed { txid: None, need_passphrase: true });
    let c = h.phone.confirm_send(&confirm, true, pass("")).await.unwrap();
    assert!(c.need_passphrase, "an empty one counts as none");
    // A wrong one: a clear message, and the send keeps waiting.
    let e = h.phone.confirm_send(&confirm, true, pass("wrong")).await.unwrap_err();
    assert_eq!(e, "That isn't the wallet's passphrase. The payment is still waiting.");
    assert_eq!(h.phone.held().len(), 1);
    assert_eq!(h.phone.store.load_held().held.len(), 1, "still saved");
    assert!(h.rpc.sends().is_empty());
    assert!(h.quiet(), "the phone hears nothing yet");

    // The right one: unlocked for this send, sent, locked.
    h.rpc.forget_calls();
    let c = h.phone.confirm_send(&confirm, true, pass(PASS)).await.unwrap();
    let txid = c.txid.clone().unwrap();
    assert_eq!(h.rpc.methods(), ["getwalletinfo", "walletpassphrase", "sendtoaddress", "walletlock"]);
    assert!(!h.rpc.unlocked());
    assert_eq!(sim.open(&h.next().await["d"]), json!({"id": 8, "pending": confirm, "ok": {"txid": txid}}));
    assert!(h.phone.held().is_empty());
    assert!(!h.phone.phone_send_is_on(), "a passphrase for one send isn't kept");
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.1, "a confirmed send doesn't count against the limit");
}

#[tokio::test]
async fn confirming_uses_the_phone_send_passphrase_when_it_is_on() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.0).unwrap();
    h.rpc.encrypt(PASS);
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    let r = ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 0.5})).await;
    assert_eq!(r["ok"]["why"], "limit");
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    let c = h.phone.confirm_send(&confirm, true, None).await.unwrap();
    assert!(c.txid.is_some());
    assert!(!h.rpc.unlocked());
    assert_eq!(sim.open(&h.next().await["d"])["pending"], confirm.as_str());
}

#[tokio::test]
async fn an_empty_wallet_on_confirm_tells_the_phone() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    h.rpc.fail_send.store(true, Ordering::SeqCst);
    let r = ask(&mut h, &mut sim, 1, 4, "send", json!({"address": TO, "amount": 0.03})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    let e = h.phone.confirm_send(&confirm, true, pass(PASS)).await.unwrap_err();
    assert_eq!(e, NOT_ENOUGH);
    assert!(!h.rpc.unlocked());
    assert!(h.phone.held().is_empty(), "the node refused it: it is answered");
    assert_eq!(sim.open(&h.next().await["d"]), json!({"id": 4, "pending": confirm, "err": NOT_ENOUGH}));
}

/// The passphrase never reaches a file, the send log, the screen's events or the phone.
#[tokio::test]
async fn the_passphrase_stays_in_memory() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    h.rpc.encrypt(PASS);
    let mut replies = vec![];
    h.phone.phone_send_on(Zeroizing::new(PASS.into())).await.unwrap();
    replies.push(ask(&mut h, &mut sim, 1, 1, "send", json!({"address": TO, "amount": 0.01})).await);
    replies.push(ask(&mut h, &mut sim, 1, 2, "send", json!({"address": TO, "amount": 5})).await);
    let confirm = replies[1]["ok"]["pending"].as_str().unwrap().to_string();
    h.phone.forget_passphrase();
    replies.push(ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.01})).await);
    let _ = h.phone.confirm_send(&confirm, true, pass("wrong")).await;
    h.phone.confirm_send(&confirm, true, pass(PASS)).await.unwrap();
    replies.push(sim.open(&h.next().await["d"]));
    h.restart();

    for e in std::fs::read_dir(h.dir.join("phone")).unwrap() {
        let p = e.unwrap().path();
        let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).to_string();
        assert!(!text.contains(PASS), "{} holds the passphrase", p.display());
    }
    for (name, payload) in h.ev.0.lock().unwrap().iter() {
        assert!(!payload.to_string().contains(PASS), "event {name}");
    }
    for r in &replies {
        assert!(!r.to_string().contains(PASS));
    }
    assert!(!h.phone.phone_send_is_on(), "and a restart starts with phone sends off");
}

/// Revoking the last phone leaves the link nothing to do, but the phone hears `denied` and the
/// relay the channel's close before the link goes. (On the real relay, 2026-09-29, the link once
/// left first, and the phone showed "Desktop offline" instead of "This phone was removed".) The
/// link's select picks at random, so several rounds.
#[tokio::test]
async fn revoking_the_last_phone_tells_it_before_the_link_goes() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    // The relay, played here: each connection does the host handshake, then passes frames on.
    let (link_said, mut heard) = mpsc::unbounded_channel::<Value>();
    let (say, to_link) = mpsc::unbounded_channel::<Value>();
    let to_link = Arc::new(tokio::sync::Mutex::new(to_link));
    async fn next_json<S>(rx: &mut S) -> Option<Value>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            match rx.next().await {
                Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(t.as_str()).unwrap()),
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return None,
                _ => {}
            }
        }
    }
    let relay = tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let (mut tx, mut rx) = tokio_tungstenite::accept_async(sock).await.unwrap().split();
            let host = next_json(&mut rx).await.unwrap();
            assert_eq!(host["t"], "host");
            let n: [u8; 32] = rand::random();
            tx.send(Message::text(json!({"t": "challenge", "n": crypto::b64u(&n)}).to_string())).await.unwrap();
            let proof = next_json(&mut rx).await.unwrap();
            assert_eq!(proof["t"], "proof");
            // As the relay checks it (P4): over "fb-relay-proof-v1" || n, never the bare n.
            {
                use p256::ecdsa::signature::Verifier;
                let d = crypto::parse_pub(host["d"].as_str().unwrap()).unwrap();
                let vk = p256::ecdsa::VerifyingKey::from(&d);
                let sig = crypto::unb64u(proof["sig"].as_str().unwrap()).unwrap();
                let sig = p256::ecdsa::Signature::from_slice(&sig).unwrap();
                vk.verify(&[b"fb-relay-proof-v1".as_slice(), &n].concat(), &sig).expect("the P4 proof");
                assert!(vk.verify(&n, &sig).is_err());
            }
            tx.send(Message::text(json!({"t": "ready"}).to_string())).await.unwrap();
            let mut to_link = to_link.lock().await;
            loop {
                tokio::select! {
                    f = next_json(&mut rx) => match f {
                        Some(f) => { let _ = link_said.send(f); }
                        None => { let _ = link_said.send(json!({"gone": true})); break; }
                    },
                    Some(f) = to_link.recv() => { tx.send(Message::text(f.to_string())).await.unwrap(); }
                }
            }
        }
    });
    let mut h = harness(None);
    h.phone.set_relay(&url).unwrap();
    let rx = std::mem::replace(&mut h.rx, mpsc::unbounded_channel().1);
    tokio::spawn(link::run(h.phone.clone(), rx));
    for round in 0..8u64 {
        let (pair_url, _) = h.phone.pair_start().unwrap();
        assert!(link::wait_for(&h.phone, "online", Duration::from_secs(5)).await, "round {round}");
        let mut sim = Sim::new();
        let (pch, sch) = (100 + 2 * round, 101 + 2 * round);
        say.send(json!({"ch": pch, "d": sim.pair_frame(&h.d_pub(), &code_of(&pair_url), "Only phone")})).unwrap();
        for _ in 0..250 {
            if !h.phone.pair_pending().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        allow_the_one(&h);
        let f = tokio::time::timeout(Duration::from_secs(5), heard.recv()).await.unwrap().unwrap();
        assert_eq!(f, json!({"ch": pch, "d": {"t": "paired"}}));
        say.send(json!({"ch": sch, "d": sim.hello()})).unwrap();
        let f = tokio::time::timeout(Duration::from_secs(5), heard.recv()).await.unwrap().unwrap();
        assert_eq!(f["d"]["t"], "hello-ok", "round {round}");
        let id = h.phone.devices()[0].id.clone();
        h.phone.revoke(&id).unwrap();
        let mut got = vec![];
        loop {
            let f = tokio::time::timeout(Duration::from_secs(5), heard.recv()).await.unwrap().unwrap();
            if f["gone"] == true {
                break;
            }
            got.push(f);
        }
        assert!(got.contains(&json!({"ch": sch, "d": {"t": "denied"}})), "round {round}: {got:?}");
        assert!(got.contains(&json!({"t": "close", "ch": sch})), "round {round}: {got:?}");
        assert!(link::wait_for(&h.phone, "off", Duration::from_secs(5)).await);
        assert!(heard.try_recv().is_err());
    }
    relay.abort();
}

// ----- end to end, against the real relay ----------------------------------------------------

/// Run with a relay listening, e.g. `fb-relay --listen 127.0.0.1:18480` and
/// `FB_RELAY_URL=ws://127.0.0.1:18480/ws cargo test -j2 relay_e2e -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn relay_e2e() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let url = std::env::var("FB_RELAY_URL").expect("FB_RELAY_URL");
    let mut h = harness(None);
    h.phone.set_relay(&url).unwrap();
    let rx = std::mem::replace(&mut h.rx, mpsc::unbounded_channel().1);
    let phone = h.phone.clone();
    tokio::spawn(link::run(phone.clone(), rx));

    // Nothing paired and no pairing open: the link stays off.
    assert!(link::wait_for(&phone, "off", Duration::from_secs(3)).await);
    let (pair_url, _) = phone.pair_start().unwrap();
    assert!(link::wait_for(&phone, "online", Duration::from_secs(10)).await, "{}", phone.status().detail);
    eprintln!("link online, pairing open");

    let d_pub = crypto::parse_pub(&phone.d_pub).unwrap();
    let mut sim = Sim::new();
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut tx, mut wrx) = ws.split();
    async fn recv<S>(r: &mut S) -> Value
    where
        S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), r.next()).await.expect("frame").unwrap().unwrap();
            if let Message::Text(t) = m {
                return serde_json::from_str(t.as_str()).unwrap();
            }
        }
    }
    let text = |v: Value| Message::text(v.to_string());
    tx.send(text(json!({"t": "join", "room": phone.room}))).await.unwrap();
    assert_eq!(recv(&mut wrx).await["t"], "joined");

    // Pair.
    tx.send(text(sim.pair_frame(&d_pub, &code_of(&pair_url), "e2e phone"))).await.unwrap();
    for _ in 0..250 {
        if !phone.pair_pending().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.ev.named(EV_PAIR)[0]["name"], "e2e phone");
    let (_, code) = sim.pair_frame_and_code(&d_pub, &code_of(&pair_url), "unused");
    assert_eq!(code.len(), 7);
    allow_the_one(&h);
    assert_eq!(recv(&mut wrx).await, json!({"t": "paired"}));

    // Session and requests.
    tx.send(text(sim.hello())).await.unwrap();
    let ok = recv(&mut wrx).await;
    sim.hello_ok(&d_pub, &ok);
    tx.send(text(sim.req(1, "balance", json!({})))).await.unwrap();
    let r = sim.open(&recv(&mut wrx).await);
    assert_eq!(r["ok"]["confirmed"], 1.5);
    eprintln!("balance through the relay: {r}");
    tx.send(text(sim.req(2, "send", json!({"address": TO, "amount": 0.5})))).await.unwrap();
    let r = sim.open(&recv(&mut wrx).await);
    let confirm = r["ok"]["pending"].as_str().expect("held").to_string();
    assert_eq!(r["ok"]["why"], "limit");
    phone.confirm_send(&confirm, true, None).await.unwrap();
    let r = sim.open(&recv(&mut wrx).await);
    assert_eq!(r["id"], 2);
    assert_eq!(r["pending"], confirm.as_str());
    assert!(r["ok"]["txid"].is_string());
    eprintln!("held send confirmed through the relay: {r}");

    // Revoke: denied, then the relay drops the phone.
    phone.revoke(&phone.devices()[0].id).unwrap();
    assert_eq!(recv(&mut wrx).await, json!({"t": "denied"}));
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match wrx.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(Message::Text(t))) => eprintln!("after revoke: {t}"),
                _ => {}
            }
        }
    })
    .await;
    assert!(end.unwrap_or(false), "the relay closed the revoked phone");
    // No phone left: the link goes off.
    assert!(link::wait_for(&phone, "off", Duration::from_secs(5)).await);
}

// ----- a host for the real phone page --------------------------------------------------------

/// The wallet passphrase from FB_PASS_FILE (a 0600 file). Never printed.
fn pass_from_file(path: Option<&str>) -> Option<Zeroizing<String>> {
    use zeroize::Zeroize;
    let mut s = std::fs::read_to_string(path?).ok()?;
    let p = Zeroizing::new(s.trim_end_matches(['\r', '\n']).to_string());
    s.zeroize();
    Some(p)
}

/// Write `text` to `dir/name` whole (temp file, then rename), so a reader never sees half of it.
fn put(dir: &Path, name: &str, text: &str) {
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, dir.join(name)).unwrap();
}

/// One control command for `page_host`: `<seq> <verb> [args]`. Returns the ack and whether to stop.
async fn host_command(phone: &Arc<Phone>, line: &str, pass_file: Option<&str>) -> (Value, bool) {
    let mut w = line.split_whitespace();
    let seq = w.next().unwrap_or("0").to_string();
    let verb = w.next().unwrap_or("").to_string();
    let args: Vec<String> = w.map(String::from).collect();
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let r: Result<Value, String> = async {
        match verb.as_str() {
            "pair" => {
                let (url, expires) = phone.pair_start()?;
                Ok(json!({"url": url, "expires": expires}))
            }
            "send-on" => {
                let p = pass_from_file(pass_file).ok_or("no FB_PASS_FILE")?;
                phone.phone_send_on(p).await?;
                Ok(json!({}))
            }
            "send-off" => {
                phone.forget_passphrase();
                Ok(json!({}))
            }
            "allow" | "deny" => {
                phone.pair_answer(&arg(0), verb == "allow")?;
                Ok(json!({}))
            }
            "wallet" => Ok(serde_json::to_value(phone.wallet_view().await).unwrap()),
            "confirm" => {
                let p = match arg(1).as_str() {
                    "pass" => Some(pass_from_file(pass_file).ok_or("no FB_PASS_FILE")?),
                    "wrong" => Some(Zeroizing::new("not the wallet passphrase".to_string())),
                    _ => None,
                };
                let c = phone.confirm_send(&arg(0), true, p).await?;
                Ok(serde_json::to_value(c).unwrap())
            }
            "decline" => Ok(serde_json::to_value(phone.confirm_send(&arg(0), false, None).await?).unwrap()),
            "limit" => {
                let ecx: f64 = arg(0).parse().map_err(|_| "limit <ecx>")?;
                for d in phone.devices() {
                    phone.set_limit(&d.id, ecx)?;
                }
                Ok(json!({}))
            }
            "revoke" => {
                for d in phone.devices() {
                    phone.revoke(&d.id)?;
                }
                Ok(json!({}))
            }
            "quit" => Ok(json!({})),
            _ => Err(format!("unknown command {verb:?}")),
        }
    }
    .await;
    // The pairing URL carries a live pairing code: it goes to the script, not to the log.
    let shown = match &r {
        Ok(v) if verb == "pair" => format!("pairing open until {}", v["expires"]),
        Ok(v) => v.to_string(),
        Err(e) => format!("error: {e}"),
    };
    eprintln!("ctl {seq} {verb}: {shown}");
    let ack = match r {
        Ok(v) => json!({"seq": seq, "ok": v}),
        Err(e) => json!({"seq": seq, "err": e}),
    };
    (ack, verb == "quit")
}

/// The desktop side as a host for driving the real phone page, by hand or from a browser script
/// (scripts/phone-real-node.sh runs it against a real FreeBank node). It connects to the relay and
/// opens a pairing. Env:
/// - FB_RELAY_URL (required): the relay, e.g. ws://127.0.0.1:18480/ws.
/// - FB_NODE_URL and FB_NODE_COOKIE: a real freebankd's RPC URL and its `.cookie` file (read
///   again if the node restarts). Without them the node is the mock.
/// - FREEBANK_APP_DIR: the app folder the phone folder goes in (default: a fresh temp folder).
/// - FB_PAIR_FILE: where to write the first pairing URL.
/// - FB_CTL_DIR: a control folder. The host keeps `state.json` there current (link, phones, held
///   sends, phone sends on or off) and writes the first pairing URL to `pair-url`. A script puts
///   one command in `cmd` (a temp file, then rename), as `<seq> <verb> [args]`; the host runs it,
///   deletes `cmd` and answers `{"seq","ok"|"err"}` in `ack`. Verbs: `pair`, `allow <id>`,
///   `deny <id>` (a waiting pair request; `state.json` lists them with their comparison codes),
///   `send-on` (the passphrase from FB_PASS_FILE), `send-off`, `wallet`,
///   `confirm <id> pass|wrong|none`, `decline <id>`, `limit <ecx>`, `revoke`, `quit`.
/// - FB_PASS_FILE: the wallet passphrase, for `send-on` and `confirm <id> pass`. Never printed.
/// - FB_HELD_SECS: held sends expire after this many seconds (default 600, the most allowed).
/// - FB_AUTO_ALLOW: 1 (the default) allows every pair request as it comes; 0 leaves them to
///   `allow` and `deny`, after comparing codes.
/// - FB_AUTO_CONFIRM: 1 confirms every held send after a second (the default with the mock);
///   0 leaves them to `confirm`, `decline` or expiry (the default with a real node).
/// - FB_REVOKE_FILE: revoke every phone once this file exists.
/// - FB_HOST_SECS: stop after this long (default 120).
#[tokio::test]
#[ignore]
async fn page_host() {
    use std::path::PathBuf;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let url = env("FB_RELAY_URL").expect("FB_RELAY_URL");
    let secs: u64 = env("FB_HOST_SECS").and_then(|s| s.parse().ok()).unwrap_or(120);
    let real = env("FB_NODE_URL").zip(env("FB_NODE_COOKIE"));
    let auto_confirm = env("FB_AUTO_CONFIRM").map(|v| v == "1").unwrap_or(real.is_none());
    let auto_allow = env("FB_AUTO_ALLOW").map(|v| v == "1").unwrap_or(true);
    let pass_file = env("FB_PASS_FILE");
    let (dir, temp) = match env("FREEBANK_APP_DIR") {
        Some(d) => (PathBuf::from(d), false),
        None => (temp_dir("host"), true),
    };
    std::fs::create_dir_all(&dir).unwrap();
    let mock_now = Arc::new(AtomicU64::new(unix_now()));
    let rpc: Arc<dyn Rpc> = match &real {
        Some((node, cookie)) => {
            let datadir = Path::new(cookie).parent().expect("the cookie's folder").to_path_buf();
            let mut c = crate::rpc::FreeBankClient::default();
            assert!(c.configure_local(node, datadir), "can't read the cookie at {cookie}");
            Arc::new(commands::NodeRpc(Arc::new(tokio::sync::Mutex::new(c))))
        }
        None => Arc::new(MockRpc::new(mock_now.clone())),
    };
    let (phone, rx) = Phone::new(&dir, rpc, Arc::new(MockEvents::default()), Arc::new(unix_now)).unwrap();
    if let Some(s) = env("FB_HELD_SECS").and_then(|s| s.parse().ok()) {
        phone.set_held_ttl(s);
    }
    phone.set_relay(&url).unwrap();
    tokio::spawn(link::run(phone.clone(), rx));
    tokio::spawn(phone.clone().expire_forever());
    let (pair_url, _) = phone.pair_start().unwrap();
    assert!(link::wait_for(&phone, "online", Duration::from_secs(10)).await, "{}", phone.status().detail);
    if let Some(f) = env("FB_PAIR_FILE") {
        std::fs::write(&f, &pair_url).unwrap();
    }
    let ctl = env("FB_CTL_DIR").map(PathBuf::from);
    if let Some(c) = &ctl {
        std::fs::create_dir_all(c).unwrap();
        let _ = std::fs::remove_file(c.join("cmd"));
        let _ = std::fs::remove_file(c.join("ack"));
        put(c, "pair-url", &pair_url);
    }
    eprintln!(
        "host online: room {}, node {}, app folder {}, held sends expire after {} s{}",
        phone.room,
        if real.is_some() { "real" } else { "mock" },
        dir.display(),
        phone.held_ttl(),
        if auto_confirm { ", confirmed automatically" } else { "" }
    );
    let revoke_file = env("FB_REVOKE_FILE");
    let t0 = std::time::Instant::now();
    let mut revoked = false;
    let mut last_state = String::new();
    while t0.elapsed() < Duration::from_secs(secs) {
        mock_now.store(unix_now(), Ordering::SeqCst);
        if auto_allow {
            for a in phone.pair_pending() {
                eprintln!("allowing {:?}, code {}", a.name, a.code);
                let _ = phone.pair_answer(&a.id, true);
            }
        }
        if auto_confirm {
            for held in phone.held() {
                if unix_now() >= held.at + 1 {
                    let r = phone.confirm_send(&held.confirm, true, None).await;
                    eprintln!("confirmed held send {} ECX: {r:?}", held.amount);
                }
            }
        }
        if !revoked && revoke_file.as_deref().is_some_and(|f| Path::new(f).exists()) {
            for d in phone.devices() {
                eprintln!("revoking {}", d.name);
                phone.revoke(&d.id).unwrap();
            }
            revoked = true;
        }
        if let Some(c) = &ctl {
            let write_state = |last: &mut String| {
                let devices: Vec<Value> = phone
                    .devices()
                    .iter()
                    .map(|d| json!({"id": d.id, "name": d.name, "online": phone.online(&d.id), "limit": to_ecx(d.limit_sats)}))
                    .collect();
                let state = json!({
                    "link": phone.status().state, "devices": devices, "held": phone.held(),
                    "phone_send": phone.phone_send_is_on(), "asks": phone.pair_pending(),
                })
                .to_string();
                if state != *last {
                    put(c, "state.json", &state);
                    *last = state;
                }
            };
            write_state(&mut last_state);
            if let Ok(line) = std::fs::read_to_string(c.join("cmd")) {
                let _ = std::fs::remove_file(c.join("cmd"));
                let (ack, quit) = host_command(&phone, line.trim(), pass_file.as_deref()).await;
                // The state a command left is on disk before its answer.
                write_state(&mut last_state);
                put(c, "ack", &ack.to_string());
                if quit {
                    break;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    phone.forget_passphrase();
    for s in phone.store.recent_sends(30).iter().rev() {
        eprintln!("send log: {s}");
    }
    if temp {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ----- Face ID: passkeys (PROTOCOL.md, "Face ID: passkeys") -----------------------------------

/// A phone's platform authenticator: signs WebAuthn assertions for the page's origin.
struct Authenticator(p256::ecdsa::SigningKey);

impl Authenticator {
    fn new(seed: u8) -> Self {
        Authenticator(p256::ecdsa::SigningKey::from_bytes(&[seed; 32].into()).unwrap())
    }
    fn pk(&self) -> String {
        crypto::b64u(self.0.verifying_key().to_encoded_point(false).as_bytes())
    }
    fn assert_for(&self, challenge: &Value, origin: &str) -> Value {
        use p256::ecdsa::signature::Signer;
        use sha2::{Digest, Sha256};
        let mut ad = Sha256::digest(b"app.ecxfreebank.com").to_vec();
        ad.extend_from_slice(&[0x05, 0, 0, 0, 0]);
        let cdj = json!({"type": "webauthn.get", "challenge": challenge, "origin": origin}).to_string();
        let mut msg = ad.clone();
        msg.extend_from_slice(&Sha256::digest(cdj.as_bytes()));
        let sig: p256::ecdsa::Signature = self.0.sign(&msg);
        json!({"ad": crypto::b64u(&ad), "cdj": crypto::b64u(cdj.as_bytes()), "sig": crypto::b64u(sig.to_der().as_bytes())})
    }
    fn assert(&self, challenge: &Value) -> Value {
        self.assert_for(challenge, "https://app.ecxfreebank.com")
    }
}

/// Add `fid`'s passkey on the session on `ch` (ids `id` and `id + 1`).
async fn add_passkey(h: &mut H, sim: &mut Sim, ch: u64, id: u64, fid: &Authenticator, sends: bool) -> Value {
    let c = ask(h, sim, ch, id, "auth-start", json!({"for": "add"})).await;
    let mut add = fid.assert(&c["ok"]["challenge"]);
    add["pk"] = json!(fid.pk());
    add["cred"] = json!("Y3JlZC1pZA");
    add["sends"] = json!(sends);
    ask(h, sim, ch, id + 1, "passkey-add", add).await
}

#[tokio::test]
async fn face_id_gates_each_session() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let fid = Authenticator::new(7);
    assert_eq!(add_passkey(&mut h, &mut sim, 1, 1, &fid, false).await, json!({"id": 2, "ok": {}}));
    assert!(h.phone.devices()[0].passkey.is_some());
    // The session that added it goes on; a new one proves it first.
    assert!(ask(&mut h, &mut sim, 1, 3, "balance", json!({})).await["ok"].is_object());
    session(&mut h, &mut sim, 1).await;
    let r = ask(&mut h, &mut sim, 1, 4, "balance", json!({})).await;
    assert_eq!(r, json!({"id": 4, "err": ERR_AUTH_NEEDED, "auth": "open"}));
    // Another origin's assertion fails, and uses the challenge up.
    let c = ask(&mut h, &mut sim, 1, 5, "auth-start", json!({"for": "open"})).await;
    assert_eq!(c["ok"]["cred"], "Y3JlZC1pZA");
    let evil = fid.assert_for(&c["ok"]["challenge"], "https://evil.example");
    assert_eq!(ask(&mut h, &mut sim, 1, 6, "auth", evil).await["err"], ERR_AUTH_FAILED);
    let right = fid.assert(&c["ok"]["challenge"]);
    assert_eq!(ask(&mut h, &mut sim, 1, 7, "auth", right).await["err"], ERR_AUTH_FAILED, "the challenge was used up");
    // Another key fails; a fresh challenge and the phone's own key open the session.
    let c = ask(&mut h, &mut sim, 1, 8, "auth-start", json!({"for": "open"})).await;
    let other = Authenticator::new(9).assert(&c["ok"]["challenge"]);
    assert_eq!(ask(&mut h, &mut sim, 1, 9, "auth", other).await["err"], ERR_AUTH_FAILED);
    let c = ask(&mut h, &mut sim, 1, 10, "auth-start", json!({"for": "open"})).await;
    assert_eq!(ask(&mut h, &mut sim, 1, 11, "auth", fid.assert(&c["ok"]["challenge"])).await, json!({"id": 11, "ok": {}}));
    assert!(ask(&mut h, &mut sim, 1, 12, "balance", json!({})).await["ok"].is_object());
    // A new session can't swap the passkey without proving the old one.
    session(&mut h, &mut sim, 1).await;
    let r = add_passkey(&mut h, &mut sim, 1, 13, &Authenticator::new(9), false).await;
    assert_eq!(r["err"], ERR_AUTH_NEEDED);
    assert_eq!(h.phone.devices()[0].passkey.as_ref().unwrap().pk, fid.pk());
    // An expired challenge fails.
    let c = ask(&mut h, &mut sim, 1, 15, "auth-start", json!({"for": "open"})).await;
    h.later(CHALLENGE_SECS + 1);
    assert_eq!(ask(&mut h, &mut sim, 1, 16, "auth", fid.assert(&c["ok"]["challenge"])).await["err"], ERR_AUTH_FAILED);
    // The desktop's "Remove Face ID": the phone opens without it again.
    let id = h.phone.devices()[0].id.clone();
    h.phone.remove_passkey(&id).unwrap();
    assert!(ask(&mut h, &mut sim, 1, 17, "balance", json!({})).await["ok"].is_object());
    // A passkey that doesn't work is never kept.
    let c = ask(&mut h, &mut sim, 1, 18, "auth-start", json!({"for": "add"})).await;
    let mut add = Authenticator::new(9).assert(&c["ok"]["challenge"]);
    add["pk"] = json!(fid.pk());
    add["cred"] = json!("Y3JlZC1pZA");
    assert_eq!(ask(&mut h, &mut sim, 1, 19, "passkey-add", add).await["err"], ERR_AUTH_FAILED);
    assert!(h.phone.devices()[0].passkey.is_none());
}

#[tokio::test]
async fn face_id_before_each_send_when_chosen() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let fid = Authenticator::new(7);
    assert!(add_passkey(&mut h, &mut sim, 1, 1, &fid, true).await["ok"].is_object());
    // Without an assertion over a send challenge, nothing is reserved or sent.
    let r = ask(&mut h, &mut sim, 1, 3, "send", json!({"address": TO, "amount": 0.01})).await;
    assert_eq!(r["err"], ERR_AUTH_FAILED);
    assert!(h.rpc.sends().is_empty());
    assert_eq!(limit_left(&mut h, &mut sim, 1).await, 0.1);
    let c = ask(&mut h, &mut sim, 1, 5, "auth-start", json!({"for": "send"})).await;
    let r = ask(&mut h, &mut sim, 1, 6, "send", json!({"address": TO, "amount": 0.01, "auth": fid.assert(&c["ok"]["challenge"])})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    // Turning it off takes Face ID too (security review M1), then sends go without it.
    assert_eq!(ask(&mut h, &mut sim, 1, 7, "passkey-set", json!({"sends": false})).await["err"], ERR_AUTH_FAILED);
    let c = ask(&mut h, &mut sim, 1, 8, "auth-start", json!({"for": "change"})).await;
    let r = ask(&mut h, &mut sim, 1, 9, "passkey-set", json!({"sends": false, "auth": fid.assert(&c["ok"]["challenge"])})).await;
    assert_eq!(r, json!({"id": 9, "ok": {}}));
    let r = ask(&mut h, &mut sim, 1, 10, "send", json!({"address": TO, "amount": 0.01})).await;
    assert!(r["ok"]["txid"].is_string(), "{r}");
    assert_eq!(h.rpc.sends().len(), 2);
}

/// Security review M1: a phone left unlocked with FreeBank open. Removing or replacing the passkey
/// takes Face ID again, and a proved session lapses after 5 minutes idle or 30 in all.
#[tokio::test]
async fn face_id_changes_take_face_id_and_proofs_lapse() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let fid = Authenticator::new(7);
    assert!(add_passkey(&mut h, &mut sim, 1, 1, &fid, false).await["ok"].is_object());
    // Removing it on a proved session, without Face ID: refused.
    assert_eq!(ask(&mut h, &mut sim, 1, 3, "passkey-remove", json!({})).await["err"], ERR_AUTH_FAILED);
    assert!(h.phone.devices()[0].passkey.is_some());
    // Replacing it with another key: refused without the old one's assertion...
    let other = Authenticator::new(9);
    assert_eq!(add_passkey(&mut h, &mut sim, 1, 4, &other, false).await["err"], ERR_AUTH_FAILED);
    assert_eq!(h.phone.devices()[0].passkey.as_ref().unwrap().pk, fid.pk());
    // ...and kept with it.
    let change = ask(&mut h, &mut sim, 1, 6, "auth-start", json!({"for": "change"})).await;
    let add = ask(&mut h, &mut sim, 1, 7, "auth-start", json!({"for": "add"})).await;
    let mut x = other.assert(&add["ok"]["challenge"]);
    x["pk"] = json!(other.pk());
    x["cred"] = json!("b3RoZXI");
    x["auth"] = fid.assert(&change["ok"]["challenge"]);
    assert_eq!(ask(&mut h, &mut sim, 1, 8, "passkey-add", x).await, json!({"id": 8, "ok": {}}));
    assert_eq!(h.phone.devices()[0].passkey.as_ref().unwrap().pk, other.pk());
    // Idle for 5 minutes: asked again.
    h.later(VERIFIED_IDLE_SECS + 1);
    assert_eq!(ask(&mut h, &mut sim, 1, 9, "balance", json!({})).await["auth"], "open");
    let c = ask(&mut h, &mut sim, 1, 10, "auth-start", json!({"for": "open"})).await;
    assert!(ask(&mut h, &mut sim, 1, 11, "auth", other.assert(&c["ok"]["challenge"])).await["ok"].is_object());
    // Busy, but 30 minutes after the proof: asked again.
    for i in 0..8 {
        h.later(VERIFIED_IDLE_SECS - 60);
        let r = ask(&mut h, &mut sim, 1, 12 + i, "balance", json!({})).await;
        let lapsed = (i + 1) * (VERIFIED_IDLE_SECS - 60) > VERIFIED_MAX_SECS;
        assert_eq!(r["auth"] == "open", lapsed, "after {} s: {r}", (i + 1) * (VERIFIED_IDLE_SECS - 60));
        assert_eq!(lapsed, i == 7);
    }
    // Removing it with Face ID works.
    let c = ask(&mut h, &mut sim, 1, 30, "auth-start", json!({"for": "open"})).await;
    assert!(ask(&mut h, &mut sim, 1, 31, "auth", other.assert(&c["ok"]["challenge"])).await["ok"].is_object());
    let c = ask(&mut h, &mut sim, 1, 32, "auth-start", json!({"for": "change"})).await;
    let r = ask(&mut h, &mut sim, 1, 33, "passkey-remove", json!({"auth": other.assert(&c["ok"]["challenge"])})).await;
    assert_eq!(r, json!({"id": 33, "ok": {}}));
    assert!(h.phone.devices()[0].passkey.is_none());
}

/// Security review L2: a request that was on its way when the phone was removed is refused, and a
/// phone that had Face ID doesn't fall back to "no passkey, nothing to prove" once its record is gone.
#[tokio::test]
async fn a_removed_phone_gets_nothing() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let fid = Authenticator::new(7);
    assert!(add_passkey(&mut h, &mut sim, 1, 1, &fid, false).await["ok"].is_object());
    // The record goes while the session stays (as when revoke races a request).
    h.phone.devices.lock().unwrap().devices.clear();
    let r = ask(&mut h, &mut sim, 1, 3, "balance", json!({})).await;
    assert_eq!(r, json!({"id": 3, "err": "This phone was removed on the desktop."}));
    let r = ask(&mut h, &mut sim, 1, 4, "receive", json!({})).await;
    assert_eq!(r["err"], "This phone was removed on the desktop.");
}

/// Code review N1: a held send confirmed after the phone's Face ID proof lapsed still reaches the
/// session that asked, and a session that reconnected hears it once it proves Face ID.
#[tokio::test]
async fn a_held_outcome_survives_the_proofs_lapse() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let fid = Authenticator::new(7);
    assert!(add_passkey(&mut h, &mut sim, 1, 1, &fid, false).await["ok"].is_object());
    let id = h.phone.devices()[0].id.clone();
    h.phone.set_limit(&id, 0.05).unwrap();
    let r = ask(&mut h, &mut sim, 1, 11, "send", json!({"address": TO, "amount": 0.06})).await;
    let confirm = r["ok"]["pending"].as_str().unwrap().to_string();
    // The walk to the desktop takes over 5 minutes: the proof lapses meanwhile.
    h.later(VERIFIED_IDLE_SECS + 60);
    let txid = h.phone.confirm_send(&confirm, true, None).await.unwrap().txid.unwrap();
    let final_reply = json!({"id": 11, "pending": confirm, "ok": {"txid": txid}});
    assert_eq!(sim.open(&h.next().await["d"]), final_reply);
    // The phone reconnects: once it proves Face ID, it hears the outcome again.
    session(&mut h, &mut sim, 1).await;
    let c = ask(&mut h, &mut sim, 1, 12, "auth-start", json!({"for": "open"})).await;
    let f = sim.req(13, "auth", fid.assert(&c["ok"]["challenge"]));
    h.feed(1, f);
    assert_eq!(sim.open(&h.next().await["d"]), final_reply);
    assert_eq!(sim.open(&h.next().await["d"]), json!({"id": 13, "ok": {}}));
}

/// Daemon mode (v0.2.4, `background.rs` light): a paired phone's request wakes the node; while it starts, requests
/// that need it hear `ERR_STARTING` with `"starting": true`; a removed phone wakes nothing.
#[tokio::test]
async fn daemon_mode_wakes_on_a_paired_phones_request() {
    let mut h = harness(None);
    let mut sim = Sim::new();
    paired(&mut h, &mut sim, 1).await;
    let woken = Arc::new(AtomicU64::new(0));
    let w = woken.clone();
    h.phone.set_waker(Arc::new(move || {
        w.fetch_add(1, Ordering::SeqCst);
    }));
    h.phone.set_starting(true);
    let r = ask(&mut h, &mut sim, 1, 5, "balance", json!({})).await;
    assert_eq!(r, json!({"id": 5, "err": ERR_STARTING, "starting": true}));
    assert_eq!(woken.load(Ordering::SeqCst), 1);
    // Up again: the real answer.
    h.phone.set_starting(false);
    let r = ask(&mut h, &mut sim, 1, 6, "balance", json!({})).await;
    assert!(r["ok"]["confirmed"].is_number(), "{r}");
    assert_eq!(woken.load(Ordering::SeqCst), 2);
    // A phone removed on the desktop: refused before anything wakes.
    let id = h.phone.devices()[0].id.clone();
    h.phone.revoke(&id).unwrap();
    let f = sim.req(7, "balance", json!({}));
    h.feed(1, f);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(woken.load(Ordering::SeqCst), 2);
}
