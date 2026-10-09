//! The money changer, from the app (v0.3.0 "In and out"; `gateway/docs/distribution/CASH_OUT_DESIGN.md` §4.8): sell
//! FreeBank sECX for ECX ("out", the Withdraw panel's fast card) or buy it with ECX ("in", the Deposit panel's),
//! below par, from a changer that keeps a float of both (`distribution/changer/`, on beta Michael's).
//!
//! Every quote is signed by the changer's key, which the app pins (Settings: the changer's address and key). The app
//! checks the signature, that the quote is for what it asked (side, amount, the payout and refund addresses it gave),
//! for this FreeBank chain (its genesis), that the pay-in address is the changer's and not ours, that the price is
//! within sense, and that it hasn't expired. The user then pays in; the order is recorded in `wallet/orders.json`
//! first; the changer pays out on the other chain. The user trusts the changer up to one order (its per-order
//! maximum), and an order not paid in time shows as overdue, with the signed quote as the proof. Calls go out from
//! here (the screens' CSP allows no other host). The changer's quote text must match `distribution/changer/src/quote.rs`
//! byte for byte (the tests pin it on both sides).

use crate::commands::ClientState;
use crate::ecash::keys::Account;
use crate::ecash::to_coins;
use crate::node::NodeManager;
use crate::phone::commands::PhoneState;
use crate::phone::Approve;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::{schnorr, Message, Secp256k1, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::State;
use zeroize::Zeroizing;

/// The most discount the app takes from any changer: CASH_OUT_DESIGN.md's d_max.
pub const MAX_DISCOUNT_BPS: u64 = 1_500;
const QUOTE_LIFE: Duration = Duration::from_secs(300);
const EXPIRED: &str = "That quote is too old. Ask the changer again.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Quote {
    pub id: String,
    /// "out" (sell FreeBank sECX for ECX) or "in" (buy it with ECX).
    pub side: String,
    pub amount: u64,
    pub payout: u64,
    pub discount_bps: u64,
    pub fee: u64,
    pub pay_in: String,
    pub payout_to: String,
    pub refund_to: String,
    pub expires: u64,
    pub genesis: String,
    pub key: String,
    pub sig: String,
}

/// The signed text (the changer's quote.rs `message`).
pub fn message(q: &Quote) -> String {
    format!(
        "fb-changer quote v1\nid={}\nside={}\namount={}\npayout={}\ndiscount_bps={}\nfee={}\npay_in={}\npayout_to={}\nrefund_to={}\nexpires={}\ngenesis={}\nkey={}\n",
        q.id, q.side, q.amount, q.payout, q.discount_bps, q.fee, q.pay_in, q.payout_to, q.refund_to, q.expires, q.genesis, q.key
    )
}

pub fn verify(q: &Quote, pinned: &str) -> bool {
    let (Ok(pk), Ok(sig)) = (XOnlyPublicKey::from_str(pinned), schnorr::Signature::from_str(&q.sig)) else {
        return false;
    };
    let digest = Message::from_digest(sha256::Hash::hash(message(q).as_bytes()).to_byte_array());
    q.key == pinned && Secp256k1::verification_only().verify_schnorr(&sig, &digest, &pk).is_ok()
}

/// What the app asked for, to check the quote against.
pub struct Asked<'a> {
    pub side: &'a str,
    pub amount: u64,
    pub payout_to: &'a str,
    pub refund_to: &'a str,
    pub genesis: &'a str,
    /// The pay-in chain's height now.
    pub height: u64,
}

/// The checks that need no node: the signature, the request, the chain, the price, the expiry.
pub fn check(q: &Quote, pinned: &str, a: &Asked) -> Result<(), String> {
    if !verify(q, pinned) {
        return Err("The changer's quote isn't signed with its key. Nothing was done.".into());
    }
    if q.side != a.side || q.amount != a.amount || q.payout_to != a.payout_to || q.refund_to != a.refund_to {
        return Err("The changer's quote isn't for what FreeBank asked. Nothing was done.".into());
    }
    if q.genesis != a.genesis {
        return Err("The changer's quote is for another FreeBank chain. Nothing was done.".into());
    }
    if q.payout == 0 || q.discount_bps > MAX_DISCOUNT_BPS || q.fee > q.amount / 10 {
        return Err("The changer's price is out of bounds. Nothing was done.".into());
    }
    // What the quoted discount gives, before the payout's fee: out, the amount less the discount (the changer buys
    // below par); in, the amount ÷ (1 − discount) (it sells below par). The payout plus its fee must be that, within
    // the sat each way the rounding allows.
    let gross = match q.side.as_str() {
        "out" => q.amount - (q.amount as u128 * q.discount_bps as u128).div_ceil(10_000) as u64,
        _ => (q.amount as u128 * 10_000 / (10_000 - q.discount_bps as u128)) as u64,
    };
    if (q.payout + q.fee).abs_diff(gross) > 1 {
        return Err("The changer's quote doesn't add up. Nothing was done.".into());
    }
    if q.expires <= a.height {
        return Err(EXPIRED.into());
    }
    Ok(())
}

/// An order, as kept in orders.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub quote: Quote,
    /// "paying" (recorded, the pay-in going out), "paid_in", "done", "refunded", "held", "failed".
    pub state: String,
    #[serde(default)]
    pub pay_txid: Option<String>,
    /// The signed eCash pay-in ("in"), sent again if it may not have gone out.
    #[serde(default)]
    pub pay_hex: Option<String>,
    #[serde(default)]
    pub payout_txid: Option<String>,
    pub time: u64,
    /// The FreeBank wallet used (None: the main one).
    #[serde(default)]
    pub fb_wallet: Option<String>,
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("orders.json")
}

/// The orders on record; none when there's no file. A file that can't be read is an error, never an empty list
/// written over (review L6).
pub fn load(app_dir: &Path) -> Result<Vec<Order>, String> {
    match std::fs::read(path(app_dir)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("FreeBank's record of changer orders can't be read ({e}); it is left as it is.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("FreeBank's record of changer orders can't be read: {e}")),
    }
}

fn save(app_dir: &Path, all: &[Order]) -> Result<(), String> {
    let p = path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::ecash::wallet::write_private(&p, &serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?)
}

static FILE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);
static BOOK: LazyLock<Mutex<HashMap<String, (Quote, Option<String>, Instant)>>> = LazyLock::new(Default::default);

async fn update(app_dir: &Path, id: &str, f: impl FnOnce(&mut Order)) -> Result<(), String> {
    let _g = FILE.lock().await;
    let mut all = load(app_dir)?;
    if let Some(o) = all.iter_mut().find(|o| o.quote.id == id) {
        f(o);
    }
    save(app_dir, &all)
}

/// The changer's address must be https, or http on this computer only (review L8).
pub fn changer_url_ok(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    match u.scheme() {
        "https" => u.host_str().is_some(),
        "http" => matches!(u.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]")),
        _ => false,
    }
}

async fn conf(mgr: &NodeManager) -> Result<(String, String), String> {
    let s = mgr.settings.lock().await;
    match (&s.changer_url, &s.changer_key) {
        (Some(u), Some(k)) if changer_url_ok(u.trim()) && !k.trim().is_empty() => {
            Ok((u.trim().trim_end_matches('/').to_string(), k.trim().to_string()))
        }
        _ => Err("No money changer is set up.".into()),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The changer's own words, shown on screen: at most 200 characters (review L10).
fn short(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
    if s.chars().count() > 200 { format!("{t}…") } else { t }
}

async fn get(mgr: &NodeManager, url: &str) -> Result<Value, String> {
    let r = mgr.http.get(url).timeout(Duration::from_secs(15)).send().await.map_err(|_| "FreeBank couldn't reach the changer.".to_string())?;
    let ok = r.status().is_success();
    let v: Value = r.json().await.map_err(|_| "The changer's answer can't be read.".to_string())?;
    if !ok {
        return Err(format!("The changer: {}", short(v["error"].as_str().unwrap_or("no"))));
    }
    Ok(v)
}

async fn post(mgr: &NodeManager, url: &str, body: Value) -> Result<Value, String> {
    let r = mgr
        .http
        .post(url)
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| "FreeBank couldn't reach the changer.".to_string())?;
    let ok = r.status().is_success();
    let v: Value = r.json().await.map_err(|_| "The changer's answer can't be read.".to_string())?;
    if !ok {
        return Err(format!("The changer: {}", short(v["error"].as_str().unwrap_or("no"))));
    }
    Ok(v)
}

/// What the changer can take now, for the cards: None when no changer is set up (the cards don't show).
#[tauri::command]
pub async fn changer_info(mgr: State<'_, Arc<NodeManager>>) -> Result<Option<Value>, String> {
    let Ok((url, key)) = conf(&mgr).await else { return Ok(None) };
    let v = get(&mgr, &format!("{url}/v1/info")).await?;
    if v["key"].as_str() != Some(key.as_str()) {
        return Err("The changer answers with another key than the one set up. Nothing is offered.".into());
    }
    Ok(Some(json!({
        "out_bps": v["out_bps"], "in_bps": v["in_bps"], "min_order": v["min_order"],
        "sides": v["sides"], "paused": v["paused"].as_str().map(short),
    })))
}

/// A quote as the screens show it.
#[derive(Debug, Serialize)]
pub struct QuoteView {
    pub id: String,
    pub side: String,
    pub amount: u64,
    pub payout: u64,
    pub discount_bps: u64,
    pub fee: u64,
    pub payout_to: String,
    pub expires: u64,
    /// Blocks of the pay-in chain left to pay in.
    pub blocks_left: u64,
}

/// The FreeBank fee reckoned for paying in on FreeBank ("out"), for the phone's count: freebankd's wallet sets it.
const FREEBANK_PAY_FEE: u64 = 20_000;

/// Ask the changer for a quote: "out" (sell `amount` FreeBank sECX; eCash to `address`, or to a fresh address of the
/// app's eCash wallet) or "in" (buy with `amount` eCash from the app's eCash wallet; FreeBank sECX to a fresh address of
/// the FreeBank wallet in use).
#[tauri::command]
pub async fn changer_quote(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    side: String,
    amount: String,
    address: Option<String>,
) -> Result<QuoteView, String> {
    let (url, key) = conf(&mgr).await?;
    let amount = crate::phone::store::decimal_to_sats(&amount).map_err(|_| "Enter the amount as a number, with at most 8 decimals.")?;
    let ecash = crate::ecash::commands::ready(&mgr).await;
    let fb_wallet;
    let (payout_to, refund_to) = {
        let c = client.lock().await;
        fb_wallet = c.wallet().map(str::to_string);
        let fb_address = c.call_typed("getnewaddress", vec![json!(""), json!("legacy")]).await.map_err(|e| e.for_ui())?;
        let fb_address = fb_address.as_str().ok_or("FreeBank gave no address.")?.to_string();
        let own = || async {
            let (ec, r) = ecash.as_ref().map_err(|_| "Set up your eCash wallet first.".to_string())?;
            crate::ecash::wallet::new_address(ec, &r.main_name, &r.public(Account::Main)?).await
        };
        match side.as_str() {
            "out" => match address.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                Some(a) => (a.to_string(), fb_address),
                None => (own().await?, fb_address),
            },
            "in" => (fb_address, own().await?),
            _ => return Err("Out or in.".into()),
        }
    };
    let (genesis, fb_height) = {
        let c = client.lock().await;
        let g = c.call_typed("getblockhash", vec![json!(0)]).await.map_err(|e| e.for_ui())?;
        let h = c.call_typed("getblockcount", vec![]).await.map_err(|e| e.for_ui())?;
        (g.as_str().unwrap_or_default().to_string(), h.as_u64().unwrap_or(0))
    };
    let v = post(&mgr, &format!("{url}/v1/quote"), json!({"side": side, "amount": amount, "payout_to": payout_to, "refund_to": refund_to})).await?;
    let q: Quote = serde_json::from_value(v).map_err(|_| "The changer's quote can't be read.".to_string())?;
    // A quote id we can file under: 16 hex, not one we have already (review L9).
    if q.id.len() != 16 || !q.id.chars().all(|c| c.is_ascii_hexdigit()) || load(&mgr.app_dir)?.iter().any(|o| o.quote.id == q.id) {
        return Err("The changer's quote has an id FreeBank can't use. Nothing was done.".into());
    }
    // The pay-in chain's height, and the pay-in address must be valid there and not ours.
    let height = if side == "out" {
        let c = client.lock().await;
        let a = c.call_typed("validateaddress", vec![json!(q.pay_in)]).await.map_err(|e| e.for_ui())?;
        if a["isvalid"].as_bool() != Some(true) || a["ismine"].as_bool() == Some(true) {
            return Err("The changer's pay-in address isn't one to pay. Nothing was done.".into());
        }
        fb_height
    } else {
        let (ec, r) = ecash.as_ref().map_err(|_| "Set up your eCash wallet first.".to_string())?;
        let a = ec.wallet(&r.main_name).call_typed("getaddressinfo", vec![json!(q.pay_in)]).await.map_err(|e| e.for_ui())?;
        if a["ismine"].as_bool() == Some(true) || ec.script_of(&q.pay_in).is_err() {
            return Err("The changer's pay-in address isn't one to pay. Nothing was done.".into());
        }
        ec.node().call_typed("getblockcount", vec![]).await.map_err(|e| e.for_ui())?.as_u64().unwrap_or(0)
    };
    check(&q, &key, &Asked { side: &side, amount, payout_to: &payout_to, refund_to: &refund_to, genesis: &genesis, height })?;
    let view = QuoteView {
        id: q.id.clone(),
        side: q.side.clone(),
        amount: q.amount,
        payout: q.payout,
        discount_bps: q.discount_bps,
        fee: q.fee,
        payout_to: q.payout_to.clone(),
        expires: q.expires,
        blocks_left: q.expires - height,
    };
    let mut book = BOOK.lock().unwrap();
    book.retain(|_, (_, _, at)| at.elapsed() < QUOTE_LIFE);
    book.insert(q.id.clone(), (q, fb_wallet, Instant::now()));
    Ok(view)
}

/// A call to the FreeBank wallet an order belongs to.
async fn fb_in(c: &mut crate::rpc::FreeBankClient, wallet: &Option<String>, method: &str, params: Vec<Value>) -> Result<Value, crate::rpc::RpcError> {
    match wallet {
        Some(w) => c.call_fresh_typed_in(w, method, params).await,
        None => c.call_fresh_typed_main(method, params).await,
    }
}

/// Pay in for a quote: "out" pays FreeBank sECX from the wallet it was quoted from (wrap it in withUnlock); "in" pays
/// eCash from the app's eCash wallet (its passphrase), signed here. "Approve sends on my phone" counts it with its fee;
/// after that the quote must still stand; it is recorded in orders.json, then goes out.
#[tauri::command]
pub async fn changer_pay(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    phone: State<'_, PhoneState>,
    id: String,
    passphrase: Option<String>,
) -> Result<Order, String> {
    let (url, _) = conf(&mgr).await?;
    let (q, fb_wallet) = {
        let book = BOOK.lock().unwrap();
        let (q, w, at) = book.get(&id).ok_or(EXPIRED)?;
        if at.elapsed() >= QUOTE_LIFE {
            return Err(EXPIRED.into());
        }
        (q.clone(), w.clone())
    };
    // "In": the words open, and the eCash payment is built, checked and signed, before anything counts.
    let mut signed: Option<(String, String, u64, Vec<bitcoin::OutPoint>)> = None;
    if q.side == "in" {
        let pass = Zeroizing::new(passphrase.unwrap_or_default());
        let (c, r) = crate::ecash::commands::ready(&mgr).await?;
        let (entropy, _) = crate::recovery::ops::open_saved_blocking(&mgr.app_dir, &pass).await?;
        let key = crate::ecash::keys::Root::from_words(&entropy, c.chain)?.account(c.chain, Account::Main)?;
        if key.public != r.public(Account::Main)? {
            return Err("These words don't give this eCash wallet's key.".into());
        }
        if crate::ecash::commands::must_spend(&c, &r.main_name).await.is_some() {
            return Err("Your last payment from this eCash wallet may still go out. Wait until it shows under Payments, or half an hour.".into());
        }
        let ec_quote = crate::ecash::wallet::quote(&c, &r.main_name, &q.pay_in, Some(q.amount)).await?;
        let to = c.script_of(&ec_quote.address)?;
        let checked = crate::ecash::sign::check(&ec_quote.psbt, &key.public, &ec_quote.expect(to))?;
        let inputs = checked.tx.input.iter().map(|i| i.previous_output).collect();
        let txid = checked.tx.compute_txid().to_string();
        let hex = crate::ecash::sign::sign(checked.tx, &checked.spends, &key)?;
        signed = Some((txid, hex, checked.fee, inputs));
    } else {
        crate::send::locked_first(&client).await?;
    }
    BOOK.lock().unwrap().remove(&id).ok_or(EXPIRED)?;
    let (what, counted) = if q.side == "out" {
        (format!("Sell {} sECX to the changer for {} ECX", to_coins(q.amount), to_coins(q.payout)), q.amount + FREEBANK_PAY_FEE)
    } else {
        let fee = signed.as_ref().map(|s| s.2).unwrap_or(0);
        (format!("Buy {} sECX from the changer for {} ECX", to_coins(q.payout), to_coins(q.amount)), q.amount + fee)
    };
    let guard = phone.guard(&mgr.app_dir)?.filter(|ph| ph.approve_over().is_some());
    let cleared = match guard {
        Some(ph) => Some(ph.clear_desktop(counted, Approve::Action { text: what, sats: Some(counted) }).await?),
        None => None,
    };
    let give_back = || {
        if let (Some(cl), Some(ph)) = (cleared, guard) {
            ph.uncount(cl);
        }
    };
    // After the phone's approval, which can take minutes: the quote must still stand (review L7).
    let height = if q.side == "out" {
        let c = client.lock().await;
        c.call_typed("getblockcount", vec![]).await.map_err(|e| e.for_ui())?.as_u64().unwrap_or(u64::MAX)
    } else {
        let (c, _) = crate::ecash::commands::ready(&mgr).await?;
        c.node().call_typed("getblockcount", vec![]).await.map_err(|e| e.for_ui())?.as_u64().unwrap_or(u64::MAX)
    };
    if height >= q.expires {
        give_back();
        return Err(EXPIRED.into());
    }
    let mut order = Order {
        quote: q.clone(),
        state: "paying".into(),
        pay_txid: signed.as_ref().map(|s| s.0.clone()),
        pay_hex: signed.as_ref().map(|s| s.1.clone()),
        payout_txid: None,
        time: now(),
        fb_wallet: fb_wallet.clone(),
    };
    {
        let _g = FILE.lock().await;
        let saved = load(&mgr.app_dir).and_then(|mut all| {
            all.push(order.clone());
            save(&mgr.app_dir, &all)
        });
        if let Err(e) = saved {
            give_back();
            return Err(format!("FreeBank couldn't record the order, so it didn't pay: {e}"));
        }
    }
    let paid: Result<String, (bool, String)> = match &signed {
        None => {
            let mut c = client.lock().await;
            fb_in(&mut c, &fb_wallet, "sendtoaddress", vec![json!(q.pay_in), json!(to_coins(q.amount)), json!(format!("fb-changer {}", q.id))])
                .await
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .map_err(|e| (!e.did_nothing(), e.for_ui()))
        }
        Some((txid, hex, fee, inputs)) => {
            async {
                let (c, r) = crate::ecash::commands::ready(&mgr).await.map_err(|e| (true, e))?;
                let ceiling = crate::ecash::sign::max_fee_rate(hex, *fee).map_err(|e| (false, e))?;
                let r2 = c.node().call_typed("sendrawtransaction", vec![json!(hex), json!(ceiling)]).await;
                match r2 {
                    Ok(v) if v.as_str() == Some(txid.as_str()) => Ok(txid.clone()),
                    Err(e) if e.already_there() => Ok(txid.clone()),
                    Err(e) if e.did_nothing() => Err((false, format!("The eCash node refused the payment: {}", e.for_ui()))),
                    _ => {
                        let fail =
                            crate::ecash::wallet::SendFail { sent: true, message: String::new(), txid: Some(txid.clone()), inputs: inputs.clone() };
                        crate::ecash::commands::after_send(&r.main_name, &Err(fail), &None);
                        Err((true, "The eCash node didn't say it took the payment.".into()))
                    }
                }
            }
            .await
        }
    };
    match paid {
        Ok(txid) => {
            order.state = "paid_in".into();
            order.pay_txid = Some(txid.clone());
            // The money went: a record that can't be written is logged, not shown as a failure (re-review 8).
            if let Err(e) = update(&mgr.app_dir, &id, |o| {
                o.state = "paid_in".into();
                o.pay_txid = Some(txid.clone());
            })
            .await
            {
                crate::activity::note(&format!("changer: paid in, not recorded: {}", crate::activity::mask_numbers(&e)));
            }
            let _ = post(&mgr, &format!("{url}/v1/paid"), json!({"id": id, "txid": txid})).await;
            crate::activity::note("changer: paid in");
            Ok(order)
        }
        Err((maybe, msg)) => {
            if !maybe {
                give_back();
                let _ = update(&mgr.app_dir, &id, |o| o.state = "failed".into()).await;
                return Err(msg);
            }
            Err(format!("{msg} It may have gone out: it shows under Changer orders."))
        }
    }
}

/// An order as the screens show it.
#[derive(Debug, Serialize)]
pub struct OrderView {
    pub id: String,
    pub side: String,
    pub amount: u64,
    pub payout: u64,
    pub time: u64,
    /// "paying", "waiting" (paid in; the changer to pay), "said_paid" (the changer says so; not seen yet), "done"
    /// (seen paid), "refunded", "held", "overdue", "failed".
    pub state: String,
    pub note: Option<String>,
}

/// Whether the payout (or refund) is seen where it should be: at least `sats` received at `address`, confirmed, in
/// the app's own wallet on that chain (review M5). None: it can't be seen from here (a pasted address).
async fn seen_paid(mgr: &NodeManager, client: &ClientState, fb_wallet: &Option<String>, on_freebank: bool, address: &str, sats: u64) -> Option<bool> {
    let got = if on_freebank {
        let mut c = client.lock().await;
        fb_in(&mut c, fb_wallet, "getreceivedbyaddress", vec![json!(address), json!(1)]).await.ok()
    } else {
        let (c, r) = crate::ecash::commands::ready(mgr).await.ok()?;
        let info = c.wallet(&r.main_name).call_typed("getaddressinfo", vec![json!(address)]).await.ok()?;
        if info["ismine"].as_bool() != Some(true) {
            return None;
        }
        c.wallet(&r.main_name).call_typed("getreceivedbyaddress", vec![json!(address), json!(1)]).await.ok()
    }?;
    Some(crate::ecash::sats_of(&got).is_some_and(|g| g >= sats))
}

/// The changer orders, newest first: each unfinished one followed (a "paying" one found in the wallet or sent again,
/// then asked of the changer), and a payout or refund shown as done only once seen in the app's own wallet.
#[tauri::command]
pub async fn changer_orders(mgr: State<'_, Arc<NodeManager>>, client: State<'_, ClientState>) -> Result<Vec<OrderView>, String> {
    let all = load(&mgr.app_dir)?;
    let url = conf(&mgr).await.ok().map(|(u, _)| u);
    let mut out = Vec::new();
    for o in all.iter().rev().take(30) {
        let mut state = o.state.clone();
        let mut note = None;
        let id = o.quote.id.clone();
        if state == "paying" {
            if o.quote.side == "out" {
                // Found by its comment in the FreeBank wallet: it went out.
                let mut c = client.lock().await;
                let list = fb_in(&mut c, &o.fb_wallet, "listtransactions", vec![json!("*"), json!(1000)]).await.unwrap_or(Value::Null);
                drop(c);
                let comment = format!("fb-changer {id}");
                if let Some(t) = list.as_array().into_iter().flatten().find(|t| t["comment"].as_str() == Some(comment.as_str())) {
                    let txid = t["txid"].as_str().unwrap_or_default().to_string();
                    update(&mgr.app_dir, &id, |x| {
                        x.state = "paid_in".into();
                        x.pay_txid = Some(txid);
                    })
                    .await?;
                    state = "paid_in".into();
                }
            } else if let (Some(txid), Some(hex), Ok((c, r))) = (&o.pay_txid, &o.pay_hex, crate::ecash::commands::ready(&mgr).await) {
                // The signed eCash pay-in: in the wallet, or sent again (the same txid).
                let known = c.wallet(&r.main_name).call_typed("gettransaction", vec![json!(txid)]).await.ok().and_then(|t| t["confirmations"].as_i64());
                let tip = c.node().call_typed("getblockcount", vec![]).await.ok().and_then(|h| h.as_u64()).unwrap_or(0);
                let next = match known {
                    Some(n) if n >= 0 => Some("paid_in"),
                    Some(_) => Some("failed"),
                    // Not sent again once its quote has expired: it would only come back less a fee (re-review 6).
                    None if tip >= o.quote.expires => Some("failed"),
                    None => match c.node().call_typed("sendrawtransaction", vec![json!(hex)]).await {
                        Ok(_) => Some("paid_in"),
                        Err(e) if e.already_there() => Some("paid_in"),
                        Err(e) if e.did_nothing() => Some("failed"),
                        Err(_) => None,
                    },
                };
                if let Some(n) = next {
                    update(&mgr.app_dir, &id, |x| x.state = n.into()).await?;
                    state = n.into();
                    if n == "paid_in" {
                        if let Some(u) = &url {
                            let _ = post(&mgr, &format!("{u}/v1/paid"), json!({"id": id, "txid": txid})).await;
                        }
                    }
                }
            }
        }
        if state == "paid_in" || state == "said_paid" {
            let ask = match &url {
                Some(u) => get(&mgr, &format!("{u}/v1/order/{id}")).await.ok(),
                None => None,
            };
            let said = ask.as_ref().and_then(|v| v["state"].as_str()).unwrap_or("");
            let payout_on_freebank = o.quote.side == "in";
            match said {
                "paid" => {
                    let seen = seen_paid(&mgr, &client, &o.fb_wallet, payout_on_freebank, &o.quote.payout_to, o.quote.payout).await;
                    if seen == Some(true) {
                        let txid = ask.as_ref().and_then(|v| v["payout_txid"].as_str().map(str::to_string));
                        update(&mgr.app_dir, &id, |x| {
                            x.state = "done".into();
                            x.payout_txid = txid;
                        })
                        .await?;
                        state = "done".into();
                    } else {
                        state = "said_paid".into();
                    }
                }
                "refunded" => {
                    let seen = seen_paid(&mgr, &client, &o.fb_wallet, !payout_on_freebank, &o.quote.refund_to, 1).await;
                    if seen == Some(true) {
                        update(&mgr.app_dir, &id, |x| x.state = "refunded".into()).await?;
                        state = "refunded".into();
                    } else {
                        state = "said_paid".into();
                        note = Some("the changer says it refunded you; not seen yet".into());
                    }
                }
                "held" => {
                    state = "held".into();
                    note = ask.as_ref().and_then(|v| v["note"].as_str()).map(short);
                }
                _ => {
                    // An hour after paying in with nothing seen: overdue (the signed quote is the proof).
                    if state != "said_paid" {
                        state = if now().saturating_sub(o.time) > 3_600 { "overdue".into() } else { "waiting".into() };
                    }
                }
            }
        }
        out.push(OrderView { id, side: o.quote.side.clone(), amount: o.quote.amount, payout: o.quote.payout, time: o.time, state, note });
    }
    Ok(out)
}

/// Settings: the changer set up, if any: its address and key.
#[tauri::command]
pub async fn changer_get(mgr: State<'_, Arc<NodeManager>>) -> Result<Value, String> {
    let s = mgr.settings.lock().await;
    Ok(json!({"url": s.changer_url.clone().unwrap_or_default(), "key": s.changer_key.clone().unwrap_or_default()}))
}

/// Settings: the changer's address and key (both empty: none).
#[tauri::command]
pub async fn changer_set(mgr: State<'_, Arc<NodeManager>>, url: String, key: String) -> Result<(), String> {
    let (url, key) = (url.trim().to_string(), key.trim().to_string());
    if !url.is_empty() || !key.is_empty() {
        if !changer_url_ok(&url) {
            return Err("The changer's address must start with https:// (or be http on this computer).".into());
        }
        if XOnlyPublicKey::from_str(&key).is_err() {
            return Err("That isn't a changer key (64 hex characters).".into());
        }
    }
    let mut s = mgr.settings.lock().await.clone();
    s.changer_url = (!url.is_empty()).then_some(url);
    s.changer_key = (!key.is_empty()).then_some(key);
    mgr.save_settings(s).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::Keypair;

    fn signed(key: &Keypair) -> Quote {
        let mut q = Quote {
            id: "00112233aabbccdd".into(),
            side: "out".into(),
            amount: 100_000_000,
            payout: 98_985_000,
            discount_bps: 100,
            fee: 15_000,
            pay_in: "XpayIn".into(),
            payout_to: "bcrt1qpayout".into(),
            refund_to: "Xrefund".into(),
            expires: 812,
            genesis: "ab".repeat(32),
            key: key.x_only_public_key().0.to_string(),
            sig: String::new(),
        };
        q.sig = sign(&q, key);
        q
    }

    fn sign(q: &Quote, key: &Keypair) -> String {
        let digest = Message::from_digest(sha256::Hash::hash(message(q).as_bytes()).to_byte_array());
        Secp256k1::new().sign_schnorr_no_aux_rand(&digest, key).to_string()
    }

    fn keypair(n: u8) -> Keypair {
        Keypair::from_seckey_slice(&Secp256k1::new(), &[n; 32]).unwrap()
    }

    #[test]
    fn the_signed_text_matches_the_changers() {
        // The same text distribution/changer/src/quote.rs pins.
        let mut q = signed(&keypair(1));
        q.key = "k".into();
        assert_eq!(
            message(&q),
            "fb-changer quote v1\nid=00112233aabbccdd\nside=out\namount=100000000\npayout=98985000\ndiscount_bps=100\nfee=15000\n\
             pay_in=XpayIn\npayout_to=bcrt1qpayout\nrefund_to=Xrefund\nexpires=812\ngenesis=abababababababababababababababababababababababababababababababab\nkey=k\n"
        );
    }

    #[test]
    fn the_changers_address() {
        assert!(changer_url_ok("https://changer.ecxfreebank.com"));
        assert!(changer_url_ok("http://127.0.0.1:8490"));
        assert!(changer_url_ok("http://localhost:8490/"));
        assert!(!changer_url_ok("http://127.0.0.1.evil.com"), "a lookalike host");
        assert!(!changer_url_ok("http://changer.ecxfreebank.com"), "plain http elsewhere");
        assert!(!changer_url_ok("ftp://127.0.0.1"));
        assert!(!changer_url_ok("changer.ecxfreebank.com"));
    }

    #[test]
    fn a_quote_is_taken_only_as_asked_and_signed() {
        let key = keypair(2);
        let pinned = key.x_only_public_key().0.to_string();
        let q = signed(&key);
        let genesis = "ab".repeat(32);
        let asked = Asked { side: "out", amount: 100_000_000, payout_to: "bcrt1qpayout", refund_to: "Xrefund", genesis: &genesis, height: 800 };
        assert!(check(&q, &pinned, &asked).is_ok());
        let other = keypair(3).x_only_public_key().0.to_string();
        assert!(check(&q, &other, &asked).unwrap_err().contains("signed"));
        assert!(check(&q, &pinned, &Asked { payout_to: "bcrt1qelse", ..asked }).unwrap_err().contains("asked"));
        let other_genesis = "cd".repeat(32);
        assert!(check(&q, &pinned, &Asked { genesis: &other_genesis, ..asked }).unwrap_err().contains("another FreeBank"));
        assert!(check(&q, &pinned, &Asked { height: 812, ..asked }).unwrap_err().contains("too old"));
        // A greedy changer, even one that signs it: a 20% discount, or a payout below the discount less the fee.
        let mut greedy = q.clone();
        greedy.discount_bps = 2_000;
        greedy.payout = 80_000_000;
        greedy.sig = sign(&greedy, &key);
        assert!(check(&greedy, &pinned, &asked).unwrap_err().contains("bounds"));
        let mut short = q.clone();
        short.payout = 90_000_000;
        short.sig = sign(&short, &key);
        assert!(check(&short, &pinned, &asked).unwrap_err().contains("add up"));
        // In: 0.5 ECX buys 0.5 / 0.995 sECX, less the fee; one that gives less is refused.
        let mut buy = q.clone();
        buy.side = "in".into();
        buy.amount = 50_000_000;
        buy.discount_bps = 50;
        buy.fee = 20_000;
        buy.payout = 50_231_256;
        buy.sig = sign(&buy, &key);
        let asked_in = Asked { side: "in", amount: 50_000_000, ..asked };
        assert!(check(&buy, &pinned, &asked_in).is_ok());
        let mut less = buy.clone();
        less.payout = 49_730_000;
        less.sig = sign(&less, &key);
        assert!(check(&less, &pinned, &asked_in).unwrap_err().contains("add up"));
    }
}
