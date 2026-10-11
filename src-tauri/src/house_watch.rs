//! Houses this wallet runs (v0.4.6): Home's reminder when one nears its attest deadline, and an opt-in "Attest
//! automatically while FreeBank is open". A house that misses its deadline turns stressed (no new notes) and, left
//! stressed for 1008 blocks on the beta, insolvent. The deadline counts blocks, and the beta's pace varies (3 to 12
//! minutes a block), so a person can't keep it by the clock.
//!
//! Attesting signs with the house's key, so the wallet must be unlocked, and the app never stores the passphrase. For
//! automatic attests it is kept in memory only, as for phone sends, until turned off or the app quits; it's checked
//! against the wallet when turned on, and dropped (with an event to say so) if the wallet stops taking it. Without it,
//! Home's card asks, and the screen's usual unlock prompt does the rest.

use crate::rpc::{FreeBankClient, RpcError};
use crate::wallet::{self, RelockState, RELOCK_MARGIN};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// Home's card shows from this many blocks before the deadline (and whenever the house is stressed).
pub const REMIND_WITHIN: u64 = 144;
/// Automatic attests start this many blocks before it: about half the beta's 288-block cadence.
pub const AUTO_WITHIN: u64 = 72;
/// An automatic attest not in a block after this many blocks, and gone from the mempool, may be tried again.
const RETRY_BLOCKS: u64 = 12;
const EVERY: Duration = Duration::from_secs(60);
/// Core's "the passphrase entered was incorrect".
const RPC_WALLET_PASSPHRASE_INCORRECT: i64 = -14;
const ERR_PASS_CHANGED: &str = "The wallet's passphrase changed: automatic attests are off.";
const ERR_OTHER_NODE: &str = "FreeBank is connected to another node or wallet setup: automatic attests are off.";

/// The passphrase, with the wallet and node it was checked against: a round runs on that wallet only.
struct Kept {
    pass: Zeroizing<String>,
    wallet: Option<String>,
    url: Option<String>,
}

/// An automatic attest sent: at what height, and its txid.
#[derive(Clone)]
struct Tried {
    height: u64,
    txid: Option<String>,
}

#[derive(Default)]
pub struct AutoAttest {
    kept: std::sync::Mutex<Option<Kept>>,
    /// (wallet, house id) → the last automatic attest.
    tried: std::sync::Mutex<HashMap<(String, u64), Tried>>,
    /// (wallet, address) → whether the wallet holds it: partner keys rarely change, so each is asked once.
    mine: std::sync::Mutex<HashMap<(String, String), bool>>,
}

pub type AutoAttestState = Arc<AutoAttest>;

fn wallet_key(w: Option<&str>) -> String {
    w.unwrap_or("").to_string()
}

/// A house is this wallet's to attest when the wallet holds `threshold` of its active partner keys.
pub fn runs(house: &Value, ismine: impl Fn(&str) -> bool) -> bool {
    let need = house["threshold"].as_u64().unwrap_or(1).max(1);
    let held = house["partners"]
        .as_array()
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|p| p["status"].as_str().map_or(true, |s| s == "active"))
        .filter_map(|p| p["pubkey"].as_str().and_then(|x| hex::decode(x).ok()))
        .filter(|pk| ismine(&crate::seed::p2pkh_address(pk)))
        .count() as u64;
    held >= need
}

/// What Home shows for one house. `cause` for a stressed house: "deadline" when it missed its attest deadline and no
/// demand stands protested (an attest reopens it), else "other" (reserves under the floor, or a protest: an attest
/// won't).
pub fn view(house: &Value, height: u64, protest_open: u64) -> Value {
    let deadline = house["attest_deadline"].as_u64().unwrap_or(0);
    let status = house["effective_status"].as_str().unwrap_or("");
    let cause = if status != "stressed" {
        ""
    } else if height > deadline && protest_open == 0 {
        "deadline"
    } else {
        "other"
    };
    json!({
        "id": house["id"],
        "name": house["classid"],
        "status": status,
        "cause": cause,
        "deadline": deadline,
        "left": deadline.saturating_sub(height),
        "last": house["lastattestheight"],
    })
}

/// Due for Home's card (`within` REMIND_WITHIN) or an automatic attest (AUTO_WITHIN): near the deadline, or stressed
/// for missing it. A house stressed for another reason shows on the card, but an attest wouldn't help it.
pub fn due(v: &Value, within: u64) -> bool {
    match v["status"].as_str().unwrap_or("") {
        "stressed" => within == REMIND_WITHIN || v["cause"] == "deadline",
        "open" | "deferred" => v["left"].as_u64().unwrap_or(u64::MAX) <= within,
        _ => false,
    }
}

/// Whether to send an automatic attest now, given the last one: never twice while one waits, never again for a house
/// still stressed after one confirmed (it won't help: no fee loop), and again for an open house only when its next
/// deadline comes near.
fn try_now(v: &Value, height: u64, last_try: Option<&Tried>, pending: bool) -> bool {
    let Some(t) = last_try else { return true };
    let confirmed = v["last"].as_u64().unwrap_or(0) >= t.height;
    if confirmed {
        return v["status"] != "stressed";
    }
    !pending && height >= t.height + RETRY_BLOCKS
}

/// The chain's height and the houses the client's wallet runs. A house whose keys can't be checked is left out, not
/// the round.
async fn houses_run(c: &mut FreeBankClient, state: &AutoAttest) -> Result<(u64, Vec<Value>), String> {
    let height = c.call_ui("getblockcount", vec![]).await?.as_u64().unwrap_or(0);
    let list = c.call_ui("listhouses", vec![]).await?;
    let w = wallet_key(c.wallet());
    let mut out = Vec::new();
    for h in list.as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
        if !matches!(h["effective_status"].as_str(), Some("open" | "deferred" | "stressed")) {
            continue;
        }
        let mut known = HashMap::new();
        let mut unknown = false;
        for p in h["partners"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
            let Some(pk) = p["pubkey"].as_str().and_then(|x| hex::decode(x).ok()) else { continue };
            let a = crate::seed::p2pkh_address(&pk);
            let cached = state.mine.lock().unwrap().get(&(w.clone(), a.clone())).copied();
            let yes = match cached {
                Some(y) => y,
                None => match c.call_ui("getaddressinfo", vec![json!(a)]).await {
                    Ok(i) => {
                        let y = i["ismine"] == true;
                        state.mine.lock().unwrap().insert((w.clone(), a.clone()), y);
                        y
                    }
                    Err(_) => {
                        unknown = true;
                        false
                    }
                },
            };
            known.insert(a, yes);
        }
        if unknown || !runs(h, |a| known.get(a).copied().unwrap_or(false)) {
            continue;
        }
        let protest = if h["effective_status"] == "stressed" {
            match c.call_ui("gethouse", vec![h["id"].clone()]).await {
                Ok(full) => full["protest_open"].as_u64().unwrap_or(0),
                Err(_) => continue,
            }
        } else {
            0
        };
        out.push(view(h, height, protest));
    }
    Ok((height, out))
}

/// Home's card: the houses the screens' wallet runs that are due, and whether automatic attests are on for it.
#[tauri::command]
pub async fn houses_due(
    client: State<'_, Arc<Mutex<FreeBankClient>>>,
    state: State<'_, AutoAttestState>,
) -> Result<Value, String> {
    let mut c = client.lock().await;
    let (height, houses) = houses_run(&mut c, &state).await?;
    let due: Vec<Value> = houses.into_iter().filter(|v| due(v, REMIND_WITHIN)).collect();
    let here = state.kept.lock().unwrap().as_ref().is_some_and(|k| k.wallet.as_deref() == c.wallet() && k.url.as_deref() == c.url());
    Ok(json!({ "height": height, "houses": due, "auto_within": AUTO_WITHIN, "auto_here": here }))
}

/// Turn automatic attests on for the screens' wallet: `passphrase` is checked against it, then kept in memory with
/// the wallet's name and the node's address.
#[tauri::command]
pub async fn auto_attest_on(
    client: State<'_, Arc<Mutex<FreeBankClient>>>,
    guard: State<'_, RelockState>,
    state: State<'_, AutoAttestState>,
    passphrase: String,
) -> Result<bool, String> {
    let pass = Zeroizing::new(passphrase);
    if pass.is_empty() {
        return Err("Enter the wallet's passphrase.".into());
    }
    let mut c = client.lock().await;
    // Checked by unlocking: a locked wallet for a second, then locked again; an unlocked one until the same time,
    // after letting an unlock about to run out relock first (wallet::RelockGuard: freebankd deadlocks otherwise).
    let mut waited = false;
    let (secs, relock) = loop {
        let st = wallet::status(&mut c).await?;
        if !st.encrypted {
            return Err("This wallet has no passphrase: protect it first (Settings › Wallet).".into());
        }
        if st.unlocked_until == 0 {
            break (1, true);
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let left = st.unlocked_until.saturating_sub(now);
        if !waited && Duration::from_secs(left) <= RELOCK_MARGIN {
            waited = true;
            tokio::time::sleep(Duration::from_secs(left) + RELOCK_MARGIN).await;
            continue;
        }
        break (left.max(1) as i64, false);
    };
    wallet::unlock(&mut c, &guard, pass.to_string(), secs).await?;
    if relock {
        let _ = wallet::lock(&mut c).await;
    }
    *state.kept.lock().unwrap() = Some(Kept { pass, wallet: c.wallet().map(String::from), url: c.url().map(String::from) });
    crate::activity::note("automatic house attests on");
    Ok(true)
}

#[tauri::command]
pub fn auto_attest_off(state: State<'_, AutoAttestState>) -> bool {
    state.kept.lock().unwrap().take();
    crate::activity::note("automatic house attests off");
    false
}

/// What one automatic round did for one house.
#[derive(Debug)]
pub struct Attested {
    pub id: u64,
    pub name: Value,
    pub result: Result<Value, String>,
}

/// One automatic round, on the wallet and node the passphrase was checked against (the screens' choice is put back
/// after): each house that wallet runs that is due attests (try_now). The wallet is unlocked for it and locked again;
/// left as it was if someone had it unlocked. Err when the passphrase no longer opens it, or FreeBank now talks to
/// another node: it is dropped.
pub async fn round(c: &mut FreeBankClient, guard: &RelockState, state: &AutoAttest) -> Result<Vec<Attested>, String> {
    let (pass, wallet, url) = {
        let k = state.kept.lock().unwrap();
        let Some(k) = k.as_ref() else { return Ok(vec![]) };
        (k.pass.clone(), k.wallet.clone(), k.url.clone())
    };
    if c.url() != url.as_deref() {
        state.kept.lock().unwrap().take();
        return Err(ERR_OTHER_NODE.into());
    }
    let screens = c.wallet().map(String::from);
    c.set_wallet(wallet.clone());
    let r = round_on(c, guard, state, &pass, &wallet_key(wallet.as_deref())).await;
    c.set_wallet(screens);
    if r.as_ref().is_err_and(|e| e == ERR_PASS_CHANGED) {
        state.kept.lock().unwrap().take();
    }
    r
}

async fn round_on(c: &mut FreeBankClient, guard: &RelockState, state: &AutoAttest, pass: &str, w: &str) -> Result<Vec<Attested>, String> {
    let (height, houses) = houses_run(c, state).await?;
    let mut out = Vec::new();
    for h in houses.iter().filter(|v| due(v, AUTO_WITHIN)) {
        let id = h["id"].as_u64().unwrap_or(0);
        let last_try = state.tried.lock().unwrap().get(&(w.to_string(), id)).cloned();
        let pending = match last_try.as_ref().and_then(|t| t.txid.clone()) {
            Some(txid) => c.call_ui("getmempoolentry", vec![json!(txid)]).await.is_ok(),
            None => false,
        };
        if !try_now(h, height, last_try.as_ref(), pending) {
            continue;
        }
        let locked = wallet::status(c).await.map(|s| s.encrypted && s.unlocked_until == 0).unwrap_or(true);
        if locked {
            let r = guard
                .run(15, || c.call_fresh_typed("walletpassphrase", vec![json!(pass), json!(15)]), |e: &RpcError| {
                    !matches!(e, RpcError::Rpc { .. })
                })
                .await;
            match r {
                Ok(_) => {}
                Err(RpcError::Rpc { code: RPC_WALLET_PASSPHRASE_INCORRECT, .. }) => return Err(ERR_PASS_CHANGED.into()),
                Err(_) => break, // the node is busy: the next round tries again
            }
        }
        let r = c.call_ui("attesthouse", vec![json!(id)]).await;
        if locked {
            let _ = wallet::lock(c).await;
        }
        let r = r.map(|v| v.get("txid").cloned().unwrap_or(v));
        let txid = r.as_ref().ok().and_then(|t| t.as_str()).map(String::from);
        state.tried.lock().unwrap().insert((w.to_string(), id), Tried { height, txid });
        out.push(Attested { id, name: h["name"].clone(), result: r });
    }
    Ok(out)
}

/// Every minute while automatic attests are on, a round. Events: "house-attested" {id, name, txid} or {id, name,
/// error}; "auto-attest-off" {why} when the passphrase is dropped.
pub fn spawn(app: AppHandle, client: Arc<Mutex<FreeBankClient>>, guard: RelockState, state: AutoAttestState) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(EVERY).await;
            if state.kept.lock().unwrap().is_none() {
                continue;
            }
            let r = {
                let mut c = client.lock().await;
                round(&mut c, &guard, &state).await
            };
            match r {
                Ok(done) => {
                    for a in done {
                        match a.result {
                            Ok(txid) => {
                                crate::activity::note(&format!("house #{} attested automatically", a.id));
                                let _ = app.emit("house-attested", json!({ "id": a.id, "name": a.name, "txid": txid }));
                            }
                            Err(e) => {
                                crate::activity::note(&format!("house #{}: automatic attest failed", a.id));
                                let _ = app.emit("house-attested", json!({ "id": a.id, "name": a.name, "error": e }));
                            }
                        }
                    }
                }
                Err(why) if why == ERR_PASS_CHANGED || why == ERR_OTHER_NODE => {
                    crate::activity::note("automatic house attests off");
                    let _ = app.emit("auto-attest-off", json!({ "why": why }));
                }
                Err(_) => {} // the node is busy or restarting: the next minute tries again
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn house(threshold: u64, partners: &[(&str, &str)]) -> Value {
        json!({"threshold": threshold, "partners": partners.iter().map(|(pk, st)| json!({"pubkey": pk, "status": st})).collect::<Vec<_>>()})
    }

    #[test]
    fn a_house_is_ours_with_enough_of_its_active_keys() {
        let (a, b) = ("02".to_string() + &"11".repeat(32), "03".to_string() + &"22".repeat(32));
        let mine_a = crate::seed::p2pkh_address(&hex::decode(&a).unwrap());
        let ours = |x: &str| x == mine_a;
        assert!(runs(&house(1, &[(&a, "active")]), ours));
        assert!(!runs(&house(1, &[(&b, "active")]), ours));
        assert!(!runs(&house(2, &[(&a, "active"), (&b, "active")]), ours), "one key of a 2-of-2 house isn't enough");
        assert!(!runs(&house(1, &[(&a, "retired")]), ours), "a retired partner key doesn't count");
    }

    fn v(status: &str, deadline: u64, protest: u64, last: u64) -> Value {
        view(&json!({"id": 1, "classid": "mick1", "effective_status": status, "attest_deadline": deadline,
                     "lastattestheight": last}), 1424, protest)
    }

    #[test]
    fn due_when_near_the_deadline_or_stressed_for_missing_it() {
        assert_eq!(v("open", 1474, 0, 1186)["left"], 50);
        assert!(due(&v("open", 1474, 0, 1186), REMIND_WITHIN) && due(&v("open", 1474, 0, 1186), AUTO_WITHIN));
        assert!(due(&v("open", 1568, 0, 1280), REMIND_WITHIN) && !due(&v("open", 1568, 0, 1280), AUTO_WITHIN));
        assert!(!due(&v("open", 1700, 0, 1412), REMIND_WITHIN));
        let missed = v("stressed", 1153, 0, 865);
        assert_eq!(missed["cause"], "deadline");
        assert!(due(&missed, AUTO_WITHIN), "stressed for a missed deadline: an attest reopens it");
        // Stressed with the deadline still ahead (reserves under the floor), or with a demand protested: an attest
        // won't help, so the card says so and nothing attests automatically (security review of v0.4.6, M1).
        for other in [v("stressed", 1500, 0, 1300), v("stressed", 1153, 1, 865)] {
            assert_eq!(other["cause"], "other");
            assert!(due(&other, REMIND_WITHIN) && !due(&other, AUTO_WITHIN));
        }
        assert!(!due(&v("insolvent", 1153, 0, 865), REMIND_WITHIN) && !due(&v("wounddown", 1153, 0, 865), REMIND_WITHIN));
    }

    #[test]
    fn no_attest_loops() {
        let open = v("open", 1474, 0, 1186);
        assert!(try_now(&open, 1424, None, false));
        let sent = Tried { height: 1424, txid: Some("t".into()) };
        assert!(!try_now(&open, 1430, Some(&sent), true), "not while the last one waits in the mempool");
        assert!(!try_now(&open, 1430, Some(&sent), false), "nor before RETRY_BLOCKS");
        assert!(try_now(&open, 1436, Some(&sent), false), "gone from the mempool and still not in a block: again");
        // Confirmed (its last attest is at or after the try): a stressed house that stays stressed isn't tried again.
        assert!(!try_now(&v("stressed", 1153, 0, 1425), 1440, Some(&sent), false));
        // An open house's next deadline comes near a cadence later: tried again then.
        assert!(try_now(&v("open", 1713, 0, 1425), 1650, Some(&sent), false));
    }

    /// Against a regtest node, by hand: a wallet with a passphrase that runs a house near its deadline.
    /// FB_TEST_RPC=http://127.0.0.1:<port> FB_TEST_WALLET=<wallet> FB_TEST_PASS=<passphrase> FB_TEST_HOUSE=<id> \
    ///   cargo test --lib house_watch -- --ignored
    #[tokio::test]
    #[ignore]
    async fn a_round_attests_a_house_near_its_deadline() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k}"));
        let mut c = FreeBankClient::default();
        c.configure(&env("FB_TEST_RPC"), "t", "t");
        let wallet = env("FB_TEST_WALLET");
        let house: u64 = env("FB_TEST_HOUSE").parse().unwrap();
        let guard = RelockState::default();
        let state = AutoAttest::default();
        let keep = |pass: &str| Kept { pass: Zeroizing::new(pass.into()), wallet: Some(wallet.clone()), url: Some(env("FB_TEST_RPC")) };
        // The screens look at the node's default wallet: the round still runs on the kept one, and puts theirs back.
        c.set_wallet(None);
        assert!(round(&mut c, &guard, &state).await.unwrap().is_empty(), "nothing without the passphrase");
        *state.kept.lock().unwrap() = Some(keep("wrong"));
        assert_eq!(round(&mut c, &guard, &state).await.unwrap_err(), ERR_PASS_CHANGED);
        assert!(state.kept.lock().unwrap().is_none(), "a refused passphrase is dropped");
        *state.kept.lock().unwrap() = Some(keep(&env("FB_TEST_PASS")));
        let done = round(&mut c, &guard, &state).await.unwrap();
        assert_eq!(c.wallet(), None, "the screens' wallet is put back");
        assert_eq!(done.len(), 1, "one house due");
        assert_eq!(done[0].id, house);
        assert!(done[0].result.is_ok(), "{:?}", done[0].result);
        assert!(round(&mut c, &guard, &state).await.unwrap().is_empty(), "not again while it waits");
        c.set_wallet(Some(wallet.clone()));
        assert_eq!(wallet::status(&mut c).await.unwrap().unlocked_until, 0, "the wallet is locked again");
        // Another node: dropped.
        *state.kept.lock().unwrap() = Some(Kept { pass: Zeroizing::new("x".into()), wallet: Some(wallet), url: Some("http://elsewhere".into()) });
        assert_eq!(round(&mut c, &guard, &state).await.unwrap_err(), ERR_OTHER_NODE);
    }
}
