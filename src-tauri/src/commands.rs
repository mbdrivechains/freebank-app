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
    pub category: String, // "send" or "receive"
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

/// Send ECX to an address
#[tauri::command]
pub async fn send_transaction(
    client: State<'_, ClientState>,
    address: String,
    amount: f64,
) -> Result<String, String> {
    // Validate address format (basic check)
    if !address.starts_with('X') && !address.starts_with('x') {
        return Err("Invalid FreeBank address - must start with 'X'".to_string());
    }

    if amount <= 0.0 {
        return Err("Amount must be positive".to_string());
    }

    let mut c = client.lock().await;
    let result = c
        .call_ui(
            "sendtoaddress",
            vec![serde_json::json!(address), serde_json::json!(amount)],
        )
        .await?;

    result
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Failed to send transaction".to_string())
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
        })
        .collect();

    Ok(txs)
}

/// Generic JSON-RPC passthrough for FreeBank-specific methods (notes / houses /
/// pools / bills) so the frontend doesn't need a typed Rust command per RPC.
/// Only the calls on security.rs's allowlist go through: the screens' own calls and read-only ones.
/// The wallet-sensitive calls have their own commands, so a script injected into the page can't
/// reach them this way.
#[tauri::command]
pub async fn rpc_call(
    client: State<'_, ClientState>,
    method: String,
    params: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    crate::security::allow_rpc(&method)?;
    let mut c = client.lock().await;
    c.call_ui(&method, params).await
}
