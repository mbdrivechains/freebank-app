//! Withdraw at par (v0.3.0 "In and out"; `gateway/docs/distribution/CASH_OUT_DESIGN.md` §3.1, §3.3): FreeBank sECX
//! back to eCash through the peg, with freebankd's own calls (no node change): `createwithdrawal`, `listmywithdrawals`
//! and `getwithdrawal`, `createwithdrawalrefundrequest`. It is trustless but slow: a withdrawal waits for a bundle, the
//! bundle for L1 miners' ACKs (13,150 on mainnet, and beta uses the same), so the screen puts a warning first (Michael,
//! 2026-09-29, walkthrough 8: "if they choose it they wait."). It can be cancelled only while it waits for a bundle.
//!
//! The eCash address is a fresh one of the app's own eCash wallet when it has one; a pasted address is checked by the
//! eCash node when there is one (and always by freebankd) and gets the lookalike warning on screen: beta uses Bitcoin's
//! mainnet address formats, so a Bitcoin exchange's address passes every check and the coins would be lost. Amounts are
//! integer sats here. Each withdrawal is kept in `<app data>/wallet/withdrawals.json` too, because freebankd's list is
//! a node-wide cache a restore or another node doesn't have.

use crate::commands::ClientState;
use crate::ecash::to_coins;
use crate::node::NodeManager;
use crate::phone::commands::PhoneState;
use crate::phone::Approve;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::State;

/// The least FreeBank fee offered for the withdrawal transaction itself: freebankd's wallet wants its minimum fee for
/// the transaction's size (Core's fallback rate, 0.0002 sECX a kB, on a few hundred bytes), and refuses less.
pub const FREEBANK_FEE: u64 = 20_000;
/// The size the FreeBank fee is reckoned on, kB.
const WITHDRAWAL_KB: f64 = 0.6;
/// The eCash fee offered for the payout when freebankd has no recent average to go by.
pub const DEFAULT_MAINCHAIN_FEE: u64 = 10_000;
/// The smallest withdrawal offered: 0.001 sECX (eCash dust is far below it).
pub const MIN_WITHDRAWAL: u64 = 100_000;
const QUOTE_LIFE: Duration = Duration::from_secs(300);
const EXPIRED: &str = "That withdrawal was prepared too long ago. Prepare it again.";

/// A withdrawal as kept in withdrawals.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    /// freebankd's withdrawal id (getwithdrawal, createwithdrawalrefundrequest).
    pub id: String,
    pub txid: String,
    /// What the eCash address receives.
    pub sats: u64,
    pub fee: u64,
    pub mainchain_fee: u64,
    /// The eCash address, as freebankd normalised it.
    pub destination: String,
    pub time: u64,
    /// The cancel's transaction, once asked for.
    #[serde(default)]
    pub refund_txid: Option<String>,
    /// The FreeBank wallet it was made from (None: the main one).
    #[serde(default)]
    pub fb_wallet: Option<String>,
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("withdrawals.json")
}

/// The withdrawals on record; none when there's no file. A file that can't be read is an error, never an empty list
/// written over (security review L4).
pub fn load(app_dir: &Path) -> Result<Vec<Record>, String> {
    match std::fs::read(path(app_dir)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("FreeBank's record of withdrawals can't be read ({e}); it is left as it is.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("FreeBank's record of withdrawals can't be read: {e}")),
    }
}

/// The label refund addresses get, so the payments list can say what a refund is.
pub const REFUND_LABEL: &str = "withdrawal refund";

fn save(app_dir: &Path, all: &[Record]) -> Result<(), String> {
    let p = path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::ecash::wallet::write_private(&p, &serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?)
}

static FILE: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(Default::default);

struct Pending {
    sats: u64,
    fee: u64,
    mainchain_fee: u64,
    address: String,
    /// The FreeBank wallet it was quoted from (None: the main one), so it is made from that one (review L7).
    fb_wallet: Option<String>,
    at: Instant,
}

static BOOK: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

/// What a withdrawal will do, shown before the warning and the passphrase.
#[derive(Debug, Serialize)]
pub struct Quote {
    pub id: String,
    /// What the eCash address receives.
    pub sats: u64,
    pub fee: u64,
    pub mainchain_fee: u64,
    /// What leaves the FreeBank wallet.
    pub total: u64,
    pub address: String,
    /// The address was pasted, not taken from the app's own eCash wallet: the screen warns.
    pub pasted: bool,
}

/// A node's BTC-style amount (a JSON number), in sats.
fn sats(v: &Value) -> Option<u64> {
    crate::ecash::sats_of(v)
}

/// vbytes a payout output and its share of the bundle cost, for its eCash fee.
const PAYOUT_VBYTES: u64 = 150;

/// The eCash fee for the payout, its share of the bundle's: the eCash node's fee estimate for that size, or the default
/// with no eCash node. (Bundles are filled in order of this fee. freebankd's getaveragemainchainfees has no enforcer
/// equivalent and always fails.)
async fn mainchain_fee(mgr: &NodeManager) -> u64 {
    match crate::ecash::commands::connect(mgr).await {
        Ok(c) => (crate::ecash::wallet::fee_rate(&c).await.ceil() as u64 * PAYOUT_VBYTES).clamp(1_000, 1_000_000),
        Err(_) => DEFAULT_MAINCHAIN_FEE,
    }
}

/// Prepare a withdrawal of `amount` sECX to `address`, or, with none, to a fresh address of the app's own eCash wallet.
#[tauri::command]
pub async fn withdraw_prepare(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    amount: String,
    address: Option<String>,
) -> Result<Quote, String> {
    let sats = crate::phone::store::decimal_to_sats(&amount).map_err(|_| "Enter the amount as a number of sECX, with at most 8 decimals.")?;
    if sats < MIN_WITHDRAWAL {
        return Err(format!("The smallest withdrawal is {} sECX.", to_coins(MIN_WITHDRAWAL)));
    }
    let pasted = address.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    let address = match &pasted {
        Some(a) => {
            // The eCash node checks it when there is one; freebankd checks it again when it's created.
            if let Ok(c) = crate::ecash::commands::connect(&mgr).await {
                let v = c.node().call_typed("validateaddress", vec![json!(a)]).await.map_err(|e| format!("The eCash node: {e}"))?;
                if v["isvalid"].as_bool() != Some(true) {
                    return Err("That isn't an eCash address.".into());
                }
            }
            a.clone()
        }
        None => {
            let (c, r) = crate::ecash::commands::ready(&mgr)
                .await
                .map_err(|_| "Set up your eCash wallet first, or paste an eCash address.".to_string())?;
            crate::ecash::wallet::new_address(&c, &r.main_name, &r.public(crate::ecash::keys::Account::Main)?).await?
        }
    };
    let mainchain_fee = mainchain_fee(&mgr).await;
    let fee = freebank_fee(&client).await;
    // Enough in the wallet, said before the warning and the passphrase (the UX run).
    let (have, fb_wallet) = {
        let c = client.lock().await;
        let b = c.call_typed("getbalance", vec![]).await.map_err(|e| e.for_ui())?;
        (crate::ecash::sats_of(&b).unwrap_or(0), c.wallet().map(str::to_string))
    };
    if have < sats + fee + mainchain_fee {
        return Err(format!(
            "You have {} sECX; this withdrawal needs {} with its fees.",
            to_coins(have),
            to_coins(sats + fee + mainchain_fee)
        ));
    }
    let id = format!("{:016x}", rand::random::<u64>());
    {
        let mut book = BOOK.lock().unwrap();
        book.retain(|_, p| p.at.elapsed() < QUOTE_LIFE);
        book.insert(id.clone(), Pending { sats, fee, mainchain_fee, address: address.clone(), fb_wallet, at: Instant::now() });
    }
    Ok(Quote {
        id,
        sats,
        fee,
        mainchain_fee,
        total: sats + fee + mainchain_fee,
        address,
        pasted: pasted.is_some(),
    })
}

/// "Withdraw anyway, I'll wait": create the prepared withdrawal. Wrap it in withUnlock. "Approve sends on my phone"
/// counts it, or asks a phone, first.
#[tauri::command]
pub async fn withdraw_confirm(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    phone: State<'_, PhoneState>,
    id: String,
) -> Result<Record, String> {
    if !BOOK.lock().unwrap().get(&id).is_some_and(|p| p.at.elapsed() < QUOTE_LIFE) {
        return Err(EXPIRED.into());
    }
    crate::send::locked_first(&client).await?;
    let p = BOOK.lock().unwrap().remove(&id).filter(|p| p.at.elapsed() < QUOTE_LIFE).ok_or(EXPIRED)?;
    let total = p.sats + p.fee + p.mainchain_fee;
    let guard = phone.guard(&mgr.app_dir)?.filter(|ph| ph.approve_over().is_some());
    let cleared = match guard {
        Some(ph) => Some(
            ph.clear_desktop(
                total,
                Approve::Action { text: format!("Withdraw {} sECX to the eCash address {}", to_coins(p.sats), p.address), sats: Some(total) },
            )
            .await?,
        ),
        None => None,
    };
    let made = async {
        let mut c = client.lock().await;
        let refund = in_wallet(&mut c, &p.fb_wallet, "getnewaddress", vec![json!(REFUND_LABEL), json!("legacy")]).await.map_err(|e| e.for_ui())?;
        let refund = refund.as_str().ok_or("FreeBank gave no refund address.")?.to_string();
        in_wallet(&mut c, &p.fb_wallet, "createwithdrawal", create_args(&p.address, &refund, p.sats, p.fee, p.mainchain_fee))
        .await
        .map_err(|e| (e.did_nothing(), e.for_ui()))
        .map_err(|(nothing, m)| if nothing { m } else { format!("{m} It may have gone through: look under Withdrawals.") })
    }
    .await;
    let v = match made {
        Ok(v) => v,
        Err(e) => {
            // Given back only if nothing can have happened.
            if !e.contains("may have gone") {
                if let (Some(cl), Some(ph)) = (cleared, guard) {
                    ph.uncount(cl);
                }
            }
            return Err(e);
        }
    };
    let rec = Record {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        txid: v["txid"].as_str().unwrap_or_default().to_string(),
        sats: p.sats,
        fee: p.fee,
        mainchain_fee: p.mainchain_fee,
        destination: v["destination"].as_str().unwrap_or(&p.address).to_string(),
        time: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        refund_txid: None,
        fb_wallet: p.fb_wallet.clone(),
    };
    // It was made: a record that can't be written doesn't undo it (freebankd lists it anyway).
    let _g = FILE.lock().await;
    let kept = load(&mgr.app_dir).and_then(|mut all| {
        all.push(rec.clone());
        save(&mgr.app_dir, &all)
    });
    if let Err(e) = kept {
        crate::activity::note(&format!("withdraw: created, not recorded: {}", crate::activity::mask_numbers(&e)));
    }
    crate::activity::note("withdraw: created");
    Ok(rec)
}

/// A call to the wallet a withdrawal belongs to.
async fn in_wallet(
    c: &mut crate::rpc::FreeBankClient,
    wallet: &Option<String>,
    method: &str,
    params: Vec<Value>,
) -> Result<Value, crate::rpc::RpcError> {
    match wallet {
        Some(w) => c.call_fresh_typed_in(w, method, params).await,
        None => c.call_fresh_typed_main(method, params).await,
    }
}

/// createwithdrawal's arguments: the eCash address, the refund address, then amount, FreeBank fee and eCash fee as
/// decimal text (exact, never a float).
pub fn create_args(address: &str, refund: &str, sats: u64, fee: u64, mainchain_fee: u64) -> Vec<Value> {
    vec![json!(address), json!(refund), json!(to_coins(sats)), json!(to_coins(fee)), json!(to_coins(mainchain_fee))]
}

/// The FreeBank fee: freebankd's own estimate for the transaction's size, never under FREEBANK_FEE.
async fn freebank_fee(client: &ClientState) -> u64 {
    let c = client.lock().await;
    let est = c.call_typed("estimatesmartfee", vec![json!(6)]).await.ok();
    let per_kb = est.and_then(|e| sats(&e["feerate"])).unwrap_or(0);
    ((per_kb as f64 * WITHDRAWAL_KB).ceil() as u64).clamp(FREEBANK_FEE, 1_000_000)
}

/// A withdrawal as the screens show it.
#[derive(Debug, Serialize)]
pub struct View {
    pub id: String,
    pub sats: u64,
    pub mainchain_fee: u64,
    pub destination: String,
    /// 0 when it came from freebankd's list only.
    pub time: u64,
    /// "pending" (made, waiting for a FreeBank block), "waiting" (for a bundle: can be cancelled), "bundled" (can't be
    /// cancelled), "paid", "cancelling", "refunded", "failed" (its transaction can't confirm), "unknown" (freebankd
    /// doesn't know it: another node or a restored wallet).
    pub state: &'static str,
}

/// What freebankd's status and our record together mean. `refund`: no cancel asked (None), asked but its request
/// not in a block (Some(false)), or in a block (Some(true)): only then is a "Spent" a refund (review L6).
/// `made_confirmations`: the withdrawal transaction's own, when freebankd doesn't know it yet (review M3).
pub fn state_of(status: Option<&str>, refund: Option<bool>, made_confirmations: Option<i64>) -> &'static str {
    match (status, refund) {
        (Some("Unspent"), Some(_)) => "cancelling",
        (Some("Unspent"), None) => "waiting",
        (Some("Pending - in WithdrawalBundle"), _) => "bundled",
        (Some("Spent"), Some(true)) => "refunded",
        (Some("Spent"), _) => "paid",
        _ => match made_confirmations {
            Some(c) if c < 0 => "failed",
            Some(_) => "pending",
            None => "unknown",
        },
    }
}

/// freebankd's getwithdrawal amounts are integer sats.
fn int_sats(v: &Value) -> Option<u64> {
    v.as_u64()
}

/// This wallet's withdrawals, newest first: the ones kept here and any others freebankd lists, each with its status.
#[tauri::command]
pub async fn withdraw_list(mgr: State<'_, Arc<NodeManager>>, client: State<'_, ClientState>) -> Result<Vec<View>, String> {
    let mine = load(&mgr.app_dir)?;
    let mut c = client.lock().await;
    // Ours, each asked of the wallet it was made from (re-review 7); then any others the node lists.
    let mut ids: Vec<(String, Option<String>)> = mine.iter().map(|r| (r.id.clone(), r.fb_wallet.clone())).collect();
    if let Ok(list) = c.call_typed("listmywithdrawals", vec![]).await {
        for x in list.as_array().into_iter().flatten() {
            if let Some(id) = x["id"].as_str() {
                if !ids.iter().any(|(i, _)| i == id) {
                    ids.push((id.to_string(), c.wallet().map(str::to_string)));
                }
            }
        }
    }
    let mut out = Vec::new();
    for (id, wallet) in ids {
        let w = in_wallet(&mut c, &wallet, "getwithdrawal", vec![json!(id)]).await.ok();
        let rec = mine.iter().find(|r| r.id == id);
        let status = w.as_ref().and_then(|w| w["status"].as_str()).map(str::to_string);
        let refund = match rec.and_then(|r| r.refund_txid.clone()) {
            Some(t) => Some(
                in_wallet(&mut c, &wallet, "gettransaction", vec![json!(t)])
                    .await
                    .ok()
                    .and_then(|t| t["confirmations"].as_i64())
                    .is_some_and(|n| n > 0),
            ),
            None => None,
        };
        let made = match (&status, rec) {
            (None, Some(r)) if !r.txid.is_empty() => {
                in_wallet(&mut c, &wallet, "gettransaction", vec![json!(r.txid)]).await.ok().and_then(|t| t["confirmations"].as_i64())
            }
            _ => None,
        };
        out.push(View {
            id: id.clone(),
            sats: rec.filter(|r| r.sats > 0).map(|r| r.sats).or_else(|| w.as_ref().and_then(|w| int_sats(&w["amount"]))).unwrap_or(0),
            mainchain_fee: rec
                .filter(|r| r.sats > 0)
                .map(|r| r.mainchain_fee)
                .or_else(|| w.as_ref().and_then(|w| int_sats(&w["amountmainchainfee"])))
                .unwrap_or(0),
            destination: w
                .as_ref()
                .and_then(|w| w["destination"].as_str().map(str::to_string))
                .or_else(|| rec.map(|r| r.destination.clone()))
                .unwrap_or_default(),
            time: rec.map(|r| r.time).unwrap_or(0),
            state: state_of(status.as_deref(), refund, made),
        });
    }
    out.sort_by(|a, b| b.time.cmp(&a.time));
    Ok(out)
}

/// Cancel a withdrawal that still waits for a bundle: its amount and eCash fee come back to the refund address in the
/// next FreeBank block, from the wallet it was made from. Wrap it in withUnlock.
#[tauri::command]
pub async fn withdraw_cancel(mgr: State<'_, Arc<NodeManager>>, client: State<'_, ClientState>, id: String) -> Result<(), String> {
    crate::send::locked_first(&client).await?;
    let wallet = load(&mgr.app_dir)?.iter().find(|r| r.id == id).and_then(|r| r.fb_wallet.clone());
    let txid = {
        let mut c = client.lock().await;
        let wallet = wallet.clone().or_else(|| c.wallet().map(str::to_string));
        let w = in_wallet(&mut c, &wallet, "getwithdrawal", vec![json!(id)]).await.map_err(|e| e.for_ui())?;
        if w["status"].as_str() != Some("Unspent") {
            return Err("This withdrawal is in a bundle already, so it can't be cancelled.".into());
        }
        let v = in_wallet(&mut c, &wallet, "createwithdrawalrefundrequest", vec![json!(id)]).await.map_err(|e| e.for_ui())?;
        v["txid"].as_str().unwrap_or_default().to_string()
    };
    // The cancel went: a record that can't be written is logged, not shown as a failure (re-review 8).
    let _g = FILE.lock().await;
    let kept = load(&mgr.app_dir).and_then(|mut all| {
        match all.iter_mut().find(|r| r.id == id) {
            Some(r) => r.refund_txid = Some(txid),
            None => all.push(Record {
                id,
                txid: String::new(),
                sats: 0,
                fee: 0,
                mainchain_fee: 0,
                destination: String::new(),
                time: 0,
                refund_txid: Some(txid),
                fb_wallet: wallet,
            }),
        }
        save(&mgr.app_dir, &all)
    });
    if let Err(e) = kept {
        crate::activity::note(&format!("withdraw: cancel asked, not recorded: {}", crate::activity::mask_numbers(&e)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_from_freebankds_status_and_our_cancel() {
        assert_eq!(state_of(Some("Unspent"), None, None), "waiting");
        assert_eq!(state_of(Some("Unspent"), Some(false), None), "cancelling");
        assert_eq!(state_of(Some("Pending - in WithdrawalBundle"), Some(false), None), "bundled");
        assert_eq!(state_of(Some("Spent"), None, None), "paid");
        assert_eq!(state_of(Some("Spent"), Some(true), None), "refunded");
        // A cancel whose request never made it into a block: the bundle paid it.
        assert_eq!(state_of(Some("Spent"), Some(false), None), "paid");
        // Not yet in a FreeBank block: ours, and waiting for one; or its transaction conflicted.
        assert_eq!(state_of(None, None, Some(0)), "pending");
        assert_eq!(state_of(None, None, Some(-1)), "failed");
        assert_eq!(state_of(None, None, None), "unknown");
    }
}
