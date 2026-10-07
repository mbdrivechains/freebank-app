//! Tauri command handlers - bridge between frontend and Rust backend

use crate::rpc::FreeBankClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use tokio::sync::Mutex;

pub(crate) type ClientState = Arc<Mutex<FreeBankClient>>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub txid: String,
    pub amount: f64,
    pub confirmations: i64,
    pub time: i64,
    pub address: Option<String>,
    pub category: String, // "send", "receive", or for a block this wallet won "generate" / "immature" / "orphan"
    /// The block it is in, when confirmed.
    pub blockheight: Option<i64>,
    /// The address's label (send.rs HistoryItem `label`).
    pub label: Option<String>,
    /// A send's fee, sats.
    pub fee: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: i64,
    pub headers: i64,
    pub bestblockhash: String,
    pub difficulty: f64,
    pub verification_progress: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

/// Connect to a freebankd node
#[tauri::command]
pub async fn connect_node(
    client: State<'_, ClientState>,
    config: ConnectionConfig,
) -> Result<bool, String> {
    let mut c = client.lock().await;
    c.configure(
        &format!("http://{}:{}", config.host, config.port),
        &config.user,
        &config.password,
    );

    // Test connection
    match c.call_fresh("getblockchaininfo", vec![]).await {
        Ok(_) => Ok(true),
        Err(e) => Err(format!("Connection failed: {}", e)),
    }
}

/// Check if connected to node
#[tauri::command]
pub async fn get_connection_status(client: State<'_, ClientState>) -> Result<bool, String> {
    let mut c = client.lock().await;
    if !c.is_configured() {
        return Ok(false);
    }

    match c.call_fresh("getblockchaininfo", vec![]).await {
        Ok(_) => Ok(true),
        Err(_) => Ok(false),
    }
}

/// Get blockchain info
#[tauri::command]
pub async fn get_blockchain_info(
    client: State<'_, ClientState>,
) -> Result<BlockchainInfo, String> {
    let mut c = client.lock().await;
    let result = c.call_ui("getblockchaininfo", vec![]).await?;

    Ok(BlockchainInfo {
        chain: result["chain"].as_str().unwrap_or("unknown").to_string(),
        blocks: result["blocks"].as_i64().unwrap_or(0),
        headers: result["headers"].as_i64().unwrap_or(0),
        bestblockhash: result["bestblockhash"].as_str().unwrap_or("").to_string(),
        difficulty: result["difficulty"].as_f64().unwrap_or(0.0),
        verification_progress: result["verificationprogress"].as_f64().unwrap_or(0.0),
    })
}

/// Get wallet balance
#[tauri::command]
pub async fn get_balance(client: State<'_, ClientState>) -> Result<f64, String> {
    let mut c = client.lock().await;
    let result = c.call_ui("getbalance", vec![]).await?;
    result.as_f64().ok_or_else(|| "Invalid balance response".to_string())
}

/// Generate a new receiving address
#[tauri::command]
pub async fn get_new_address(client: State<'_, ClientState>) -> Result<String, String> {
    let mut c = client.lock().await;
    let result = c
        .call_ui(
            "getnewaddress",
            vec![serde_json::json!(""), serde_json::json!("legacy")],
        )
        .await?;
    result
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Invalid address response".to_string())
}

/// The screens' wallet's member addresses (v0.2.8): one per members-only or redeem-only house where it is a member,
/// for being paid in that house's notes (a new address wouldn't be a member).
#[tauri::command]
pub async fn member_addresses(client: State<'_, ClientState>) -> Result<Vec<serde_json::Value>, String> {
    let c = client.inner().clone();
    Ok(crate::phone::hosted::member_addresses(move |m, p| {
        let c = c.clone();
        async move { c.lock().await.call_ui(m, p).await }
    })
    .await)
}

/// Get recent transactions
#[tauri::command]
pub async fn get_transactions(
    client: State<'_, ClientState>,
    count: Option<i32>,
) -> Result<Vec<Transaction>, String> {
    let mut c = client.lock().await;
    let count = count.unwrap_or(20);

    let result = c
        .call_ui("listtransactions", vec![serde_json::json!("*"), serde_json::json!(count)])
        .await?;

    let txs: Vec<Transaction> = result
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|tx| Transaction {
            txid: tx["txid"].as_str().unwrap_or("").to_string(),
            amount: tx["amount"].as_f64().unwrap_or(0.0),
            confirmations: tx["confirmations"].as_i64().unwrap_or(0),
            time: tx["time"].as_i64().unwrap_or(0),
            address: tx["address"].as_str().map(|s| s.to_string()),
            category: tx["category"].as_str().unwrap_or("unknown").to_string(),
            blockheight: tx["blockheight"].as_i64(),
            label: tx["label"].as_str().map(|s| s.to_string()),
            fee: tx["fee"].as_f64().map(|f| (f.abs() * 100_000_000.0).round() as i64),
        })
        .collect();

    Ok(txs)
}

/// Generic JSON-RPC passthrough for FreeBank-specific methods (notes / houses /
/// pools / bills) so the frontend doesn't need a typed Rust command per RPC.
/// Only the calls on security.rs's allowlist go through: the screens' own calls and read-only ones.
/// The wallet-sensitive calls have their own commands, so a script injected into the page can't
/// reach them this way.
///
/// The credit tabs' payments go through "Approve sends on my phone" (security review M1): counted against the day's
/// amount, or over it a phone's approval first, asked without holding the node client. A locked wallet is said first,
/// so the unlock prompt comes before the phone is asked, and asked once.
#[tauri::command]
pub async fn rpc_call(
    client: State<'_, ClientState>,
    mgr: State<'_, std::sync::Arc<crate::node::NodeManager>>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    method: String,
    params: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    crate::security::allow_rpc(&method)?;
    let guard = if crate::phone::CREDIT_PAYMENTS.contains(&method.as_str()) {
        phone.guard(&mgr.app_dir)?.filter(|p| p.approve_over().is_some())
    } else {
        None
    };
    let cleared = match guard {
        Some(p) => {
            let bill = {
                let mut c = client.lock().await;
                if c.call_ui("getwalletinfo", vec![]).await?["unlocked_until"].as_u64() == Some(0) {
                    return Err(crate::send::LOCKED.into());
                }
                match (method.as_str(), params.first()) {
                    ("endorsebill" | "retirebill", Some(id)) => match c.call_ui("getbill", vec![id.clone()]).await {
                        Ok(b) => crate::phone::store::json_to_sats(&b["amount"]).ok(),
                        Err(_) => None,
                    },
                    _ => None,
                }
            };
            let (cost, what) = crate::phone::credit_payment(&method, &params, bill).expect("a credit payment");
            Some((p, p.clear_desktop(cost, what).await?))
        }
        None => None,
    };
    let mut c = client.lock().await;
    let r = screen_call(&mut c, &method, params).await;
    drop(c);
    r.map_err(|(e, give_back)| {
        if let (true, Some((p, cl))) = (give_back, cleared) {
            p.uncount(cl);
        }
        e
    })
}

/// A screen's call. On failure, also whether a payment counted for it is given back: only when the node surely did
/// nothing. A credit payment handed to the node with no answer of its own may have gone out, so it keeps its count and
/// the screen doesn't read "try again" (v0.2.7).
pub(crate) async fn screen_call(
    c: &mut FreeBankClient,
    method: &str,
    params: Vec<serde_json::Value>,
) -> Result<serde_json::Value, (String, bool)> {
    match c.call_fresh_typed(method, params).await {
        Ok(v) => Ok(v),
        Err(e) if e.did_nothing() => Err((e.for_ui(), true)),
        Err(_) if crate::phone::CREDIT_PAYMENTS.contains(&method) => Err((CREDIT_MAY_HAVE_GONE.into(), false)),
        Err(e) => Err((e.for_ui(), false)),
    }
}

/// A credit payment handed to the node with no answer of its own (a timeout, a dropped connection).
pub(crate) const CREDIT_MAY_HAVE_GONE: &str =
    "Your node didn't answer after FreeBank handed it this payment, so it may have gone out. Check your notes and History before trying again.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::stub;
    use serde_json::json;

    #[tokio::test]
    async fn a_credit_payment_the_node_took_without_answering_keeps_its_count() {
        let (mut c, calls) = stub::serve(|m, _| match m {
            "transfernote" => Err((stub::HANG_UP, String::new())),
            "redeemnote" => Err((-4, "no single holder's coins sum exactly to the amount".into())),
            "listhouses" => Err((stub::HANG_UP, String::new())),
            _ => Ok(json!({"txid": "t"})),
        });
        // The node took it and the line dropped: it may have gone out, and its count stays.
        let e = screen_call(&mut c, "transfernote", vec![json!(1), json!(5), json!(0.0001), json!("X")]).await.unwrap_err();
        assert_eq!(e, (CREDIT_MAY_HAVE_GONE.to_string(), false));
        // The node's own refusal: nothing went out, the count comes back, and the screen gets the code.
        let e = screen_call(&mut c, "redeemnote", vec![json!(1), json!(5), json!(0.0001)]).await.unwrap_err();
        assert_eq!(e, ("RPC error -4: no single holder's coins sum exactly to the amount".to_string(), true));
        // A read that drops says what happened, as before (nothing was counted for it).
        let e = screen_call(&mut c, "listhouses", vec![]).await.unwrap_err();
        assert!(!e.1 && e.0 != CREDIT_MAY_HAVE_GONE, "{e:?}");
        assert_eq!(screen_call(&mut c, "issuebill", vec![]).await.unwrap(), json!({"txid": "t"}));
        assert_eq!(calls.lock().unwrap().len(), 4);
    }
}
