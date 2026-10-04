//! The app's two wallets in the eCash node, watch-only (v0.2.6 security review H1, H2): made from the accounts' public
//! keys, read, asked for addresses (each checked against the key derived here: review M3), and asked to fund payments
//! that `sign.rs` checks and signs here.

use super::conn::Conn;
use super::keys::{Account, AccountKey, AccountPub};
use super::sign::{self, Expect};
use super::{sats_of, to_coins};
use crate::rpc::RpcError;
use crate::seed::Chain;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Core's RPC_WALLET_NOT_FOUND and RPC_WALLET_ALREADY_LOADED.
const WALLET_NOT_FOUND: i64 = -18;
const WALLET_ALREADY_LOADED: i64 = -35;

/// What the app keeps about its eCash wallets (`<app data>/wallet/ecash.json`): only public things. The wallets' names
/// carry the eCash root's fingerprint, so other words make other wallets beside them (review L1); the key id says which
/// words (seed.rs's header); the accounts' xpubs are what every address and payment is checked against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub key_id: String,
    pub chain: String,
    pub fingerprint: String,
    pub main_name: String,
    pub bids_name: String,
    pub main_xpub: String,
    pub bids_xpub: String,
}

impl Record {
    pub fn chain(&self) -> Result<Chain, String> {
        Chain::from_name(&self.chain)
    }

    pub fn name(&self, a: Account) -> &str {
        match a {
            Account::Main => &self.main_name,
            Account::Bids => &self.bids_name,
        }
    }

    pub fn public(&self, a: Account) -> Result<AccountPub, String> {
        let x = match a {
            Account::Main => &self.main_xpub,
            Account::Bids => &self.bids_xpub,
        };
        AccountPub::from_record(x, &self.fingerprint, self.chain()?, a)
    }
}

/// The wallets' names for an eCash root.
pub fn names(fingerprint: &str) -> (String, String) {
    (format!("freebank-ecash-{}", fingerprint), format!("freebank-bids-{}", fingerprint))
}

pub fn record_path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("ecash.json")
}

/// The bidding account's private key: owner-only, kept so bids can be signed with nobody there.
pub fn bids_key_path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("ecash-bids.key")
}

pub fn read_record(app_dir: &Path) -> Option<Record> {
    serde_json::from_slice(&std::fs::read(record_path(app_dir)).ok()?).ok()
}

pub fn write_record(app_dir: &Path, r: &Record) -> Result<(), String> {
    let p = record_path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    // Owner-only (re-review L-F): it names every address of both wallets.
    write_private(&p, &serde_json::to_vec_pretty(r).map_err(|e| e.to_string())?)
}

/// Write `bytes` to `p` owner-only, through a temporary file renamed over it.
pub(crate) fn write_private(p: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = crate::node::install::private_file(&tmp).map_err(|e| format!("Couldn't write {}: {}", tmp.display(), e))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

pub fn write_bids_key(app_dir: &Path, key: &AccountKey) -> Result<(), String> {
    use std::io::Write;
    let p = bids_key_path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&p);
    let mut f = crate::node::install::private_file(&p).map_err(|e| format!("Couldn't write {}: {}", p.display(), e))?;
    f.write_all(key.to_file().as_bytes()).map_err(|e| e.to_string())
}

pub fn read_bids_key(app_dir: &Path, r: &Record) -> Result<AccountKey, String> {
    let p = bids_key_path(app_dir);
    let meta = std::fs::metadata(&p).map_err(|_| "The bidding wallet's key file is missing: set up again.")?;
    // Only this user's, and only theirs to read (re-review nit): a copy others can read may already be out.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::getuid() } {
            return Err(format!("The bidding wallet's key file ({}) belongs to another user: FreeBank won't use it.", p.display()));
        }
        if meta.mode() & 0o077 != 0 {
            // Made private again, and said once: the key comes from the words, so a new setup would give the same one.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
            return Err(format!(
                "The bidding wallet's key file ({}) could be read by other users of this computer; FreeBank has made it \
                 private again. If someone else uses this computer, move the bidding wallet's eCash back and keep bidding \
                 off: its key may have been copied.",
                p.display()
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(&p).map_err(|_| "The bidding wallet's key file can't be read.")?);
    let k = AccountKey::from_file(&text, r.chain()?, Account::Bids)?;
    if k.public != r.public(Account::Bids)? {
        return Err("The bidding wallet's key file isn't this wallet's.".into());
    }
    Ok(k)
}

pub fn chain_name(c: Chain) -> &'static str {
    match c {
        Chain::Main => "main",
        Chain::Regtest => "regtest",
    }
}

fn say(e: RpcError) -> String {
    match e {
        RpcError::Rpc { message, .. } => message,
        e => e.to_string(),
    }
}

/// Open a wallet if the node has it on disk. False: the node has no such wallet.
pub async fn open(conn: &Conn, name: &str) -> Result<bool, String> {
    let loaded = conn.node().call_typed("listwallets", vec![]).await.map_err(say)?;
    if loaded.as_array().is_some_and(|l| l.iter().any(|w| w == name)) {
        return Ok(true);
    }
    // load_on_startup: the node opens it again after a restart.
    match conn.node().call_typed("loadwallet", vec![json!(name), json!(true)]).await {
        Ok(_) | Err(RpcError::Rpc { code: WALLET_ALREADY_LOADED, .. }) => Ok(true),
        Err(RpcError::Rpc { code: WALLET_NOT_FOUND, .. }) => Ok(false),
        Err(e) => Err(format!("The eCash node couldn't open the wallet {}: {}", name, say(e))),
    }
}

/// Whether the wallet watches `acct`: watch-only, and its first receive address is the account's.
pub async fn holds(conn: &Conn, name: &str, acct: &AccountPub) -> Result<bool, String> {
    let w = conn.wallet(name);
    let info = w.call_typed("getwalletinfo", vec![]).await.map_err(say)?;
    if info["private_keys_enabled"] != Value::Bool(false) {
        return Ok(false);
    }
    let first = acct.address(0, 0)?;
    let a = w.call_typed("getaddressinfo", vec![json!(first)]).await.map_err(say)?;
    Ok(a["ismine"] == Value::Bool(true) || a["iswatchonly"] == Value::Bool(true))
}

/// When to rescan from on import: a little before the eCash fork's pinned block (nothing of this wallet's can be
/// older), or the start on regtest.
async fn rescan_from(conn: &Conn) -> Value {
    if conn.chain == Chain::Regtest {
        return json!(0);
    }
    match conn.node().call_typed("getblockheader", vec![json!(crate::node::PIN_HASH)]).await {
        Ok(h) => json!(h["time"].as_u64().unwrap_or(0).saturating_sub(7200)),
        Err(_) => json!(0),
    }
}

/// Make (or reuse) the watch-only wallet `name` for `acct`. A wallet of that name that isn't the account's, watch-only,
/// is refused: FreeBank never changes a wallet it didn't make.
pub async fn create(conn: &Conn, name: &str, acct: &AccountPub) -> Result<(), String> {
    if open(conn, name).await? {
        if holds(conn, name, acct).await? {
            return Ok(());
        }
        return Err(format!("Your eCash node already has a wallet called {} that isn't this one. FreeBank won't change it.", name));
    }
    // Watch-only (no private keys), blank, descriptors, no passphrase (it holds nothing secret), opened again after the
    // node restarts.
    conn.node()
        .call_typed(
            "createwallet",
            vec![json!(name), json!(true), json!(true), json!(""), json!(false), json!(true), json!(true)],
        )
        .await
        .map_err(|e| format!("The eCash node couldn't make the wallet {}: {}", name, say(e)))?;
    let ts = rescan_from(conn).await;
    let req = json!([
        {"desc": acct.descriptor(0)?, "active": true, "internal": false, "timestamp": ts},
        {"desc": acct.descriptor(1)?, "active": true, "internal": true, "timestamp": ts},
    ]);
    let r = conn
        .wallet(name)
        .without_timeout()
        .call_typed("importdescriptors", vec![req])
        .await
        .map_err(|e| format!("The eCash node couldn't take the wallet's public keys: {}", say(e)))?;
    if let Some(bad) = r.as_array().and_then(|v| v.iter().find(|x| x["success"] != Value::Bool(true))) {
        return Err(format!(
            "The eCash node couldn't take the wallet's public keys: {}",
            bad["error"]["message"].as_str().unwrap_or("no reason given")
        ));
    }
    if !holds(conn, name, acct).await? {
        return Err(format!("The wallet {} doesn't show the keys it was given.", name));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Balance {
    /// Spendable: confirmed, and the wallet's own unconfirmed change.
    pub trusted: u64,
    /// Coming in, not confirmed yet.
    pub pending: u64,
}

pub async fn balance(conn: &Conn, name: &str) -> Result<Balance, String> {
    let b = conn.wallet(name).call_typed("getbalances", vec![]).await.map_err(say)?;
    // Watch-only wallets report under "mine" in descriptor wallets (and "watchonly" in legacy ones).
    let part = if b["mine"].is_object() { &b["mine"] } else { &b["watchonly"] };
    Ok(Balance {
        trusted: sats_of(&part["trusted"]).unwrap_or(0),
        pending: sats_of(&part["untrusted_pending"]).unwrap_or(0),
    })
}

/// The place (branch, index) of `address` in `acct`, by what the node says of it, checked against the key derived
/// here. Refused if the node's description doesn't match (review M3: someone may have given the node other keys).
pub async fn place_of(conn: &Conn, name: &str, acct: &AccountPub, address: &str) -> Result<(u32, u32), String> {
    let a = conn.wallet(name).call_typed("getaddressinfo", vec![json!(address)]).await.map_err(say)?;
    let place = a["desc"].as_str().and_then(|d| acct.place_of_desc(d));
    match place {
        Some((b, i)) if acct.address(b, i)? == address => Ok((b, i)),
        _ => Err("The eCash node gave an address that isn't this wallet's. Nothing was done.".into()),
    }
}

/// A new receive address (branch 0) of the wallet, checked.
pub async fn new_address(conn: &Conn, name: &str, acct: &AccountPub) -> Result<String, String> {
    let v = conn.wallet(name).call_typed("getnewaddress", vec![json!(""), json!("bech32")]).await.map_err(say)?;
    let a = v.as_str().ok_or("The eCash wallet gave no address.")?.to_string();
    match place_of(conn, name, acct, &a).await? {
        (0, _) => Ok(a),
        _ => Err("The eCash node gave an address that isn't this wallet's. Nothing was done.".into()),
    }
}

/// A new change address (branch 1) of the wallet, checked.
pub async fn change_address(conn: &Conn, name: &str, acct: &AccountPub) -> Result<(String, (u32, u32)), String> {
    let v = conn.wallet(name).call_typed("getrawchangeaddress", vec![json!("bech32")]).await.map_err(say)?;
    let a = v.as_str().ok_or("The eCash wallet gave no change address.")?.to_string();
    match place_of(conn, name, acct, &a).await? {
        (1, i) => Ok((a, (1, i))),
        _ => Err("The eCash node gave a change address that isn't this wallet's. Nothing was done.".into()),
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Tx {
    /// "main" or "bids".
    pub wallet: &'static str,
    pub txid: String,
    /// Core's category: "send", "receive", "generate", "immature".
    pub category: String,
    /// Signed: out is negative. Fees not included.
    pub sats: i64,
    /// The fee this wallet paid (sends only), sats.
    pub fee: u64,
    pub confirmations: i64,
    pub time: u64,
    pub address: Option<String>,
}

fn signed_sats(v: &Value) -> i64 {
    let f = v.as_f64().unwrap_or(0.0);
    (f * 100_000_000.0).round() as i64
}

/// Both wallets' newest payments, newest first.
pub async fn history(conn: &Conn, r: &Record) -> Result<Vec<Tx>, String> {
    let mut all = Vec::new();
    for (a, tag) in [(Account::Main, "main"), (Account::Bids, "bids")] {
        let list = conn
            .wallet(r.name(a))
            .call_typed("listtransactions", vec![json!("*"), json!(100), json!(0), json!(true)])
            .await
            .map_err(say)?;
        for t in list.as_array().into_iter().flatten() {
            all.push(Tx {
                wallet: tag,
                txid: t["txid"].as_str().unwrap_or("").to_string(),
                category: t["category"].as_str().unwrap_or("").to_string(),
                sats: signed_sats(&t["amount"]),
                fee: signed_sats(&t["fee"]).unsigned_abs(),
                confirmations: t["confirmations"].as_i64().unwrap_or(0),
                time: t["time"].as_u64().unwrap_or(0),
                address: t["address"].as_str().map(String::from),
            });
        }
    }
    // Newest first; rows of one time by confirmations, the newest first too.
    all.sort_by(|x, y| y.time.cmp(&x.time).then(x.confirmations.cmp(&y.confirmations)));
    Ok(all)
}

/// The fee rate for a payment, sat/vB: the node's estimate for 6 blocks, else 2 (eCash betanet's blocks are rarely
/// full), between 1 and 500.
pub async fn fee_rate(conn: &Conn) -> f64 {
    let est = conn.node().call_typed("estimatesmartfee", vec![json!(6)]).await.ok();
    let r = est.and_then(|e| e["feerate"].as_f64()).map(|btc_per_kvb| btc_per_kvb * 100_000.0).unwrap_or(2.0);
    r.clamp(1.0, 500.0)
}

/// The most any payment's fee may be (0.01 eCash), whatever its size; its rate is bounded too (Expect::max_rate).
pub const MAX_FEE: u64 = 1_000_000;

/// A payment ready to sign: what arrives where, the fee, and the node's unsigned PSBT.
#[derive(Debug, Clone)]
pub struct Quote {
    pub psbt: String,
    pub address: String,
    pub sats: u64,
    pub fee: u64,
    /// The fee rate asked for, sat/vB, rounded up: the fee may be no higher than this on the payment's largest size.
    pub rate: u64,
}

/// Fund a payment of `sats` to `address` from wallet `name`, or of everything it can spend (None), the fee then taken
/// from the amount.
pub async fn quote(conn: &Conn, name: &str, address: &str, sats: Option<u64>) -> Result<Quote, String> {
    let v = conn.node().call_typed("validateaddress", vec![json!(address)]).await.map_err(say)?;
    if v["isvalid"] != Value::Bool(true) {
        return Err("That isn't an eCash address.".into());
    }
    let (amount, max) = match sats {
        Some(s) if s > 0 => (s, false),
        Some(_) => return Err("Enter an amount above zero.".into()),
        None => {
            let b = balance(conn, name).await?;
            if b.trusted == 0 {
                return Err("There is nothing to send yet.".into());
            }
            (b.trusted, true)
        }
    };
    let rate = fee_rate(conn).await;
    let mut opts = json!({"fee_rate": rate, "replaceable": true});
    if max {
        opts["subtractFeeFromOutputs"] = json!([0]);
    }
    let r = conn
        .wallet(name)
        .call_typed(
            "walletcreatefundedpsbt",
            vec![json!([]), json!([{ address: to_coins(amount) }]), json!(super::REPLAY_LOCKTIME), opts, json!(true)],
        )
        .await
        .map_err(|e| match e {
            RpcError::Rpc { code: -4, message } if message.contains("Insufficient funds") => {
                "There isn't enough eCash for that and its fee.".to_string()
            }
            e => say(e),
        })?;
    let fee = sats_of(&r["fee"]).ok_or("The eCash node didn't say the fee.")?;
    let psbt = r["psbt"].as_str().ok_or("The eCash node gave no transaction.")?.to_string();
    let sats = if max { amount.saturating_sub(fee) } else { amount };
    Ok(Quote { psbt, address: address.to_string(), sats, fee, rate: rate.ceil() as u64 })
}

impl Quote {
    /// What the payment must be, to `to` (the address's script).
    pub fn expect(&self, to: bitcoin::ScriptBuf) -> Expect {
        Expect { to, sats: self.sats, fee: self.fee, max_fee: MAX_FEE, max_rate: self.rate + 1 }
    }
}

/// Why a payment didn't go through: before it left FreeBank (nothing went out), or after the signed transaction went
/// to the node, which then didn't say it took it (it may have gone out: re-review M-B).
#[derive(Debug, Clone, PartialEq)]
pub struct SendFail {
    pub sent: bool,
    pub message: String,
    pub txid: Option<String>,
    pub inputs: Vec<bitcoin::OutPoint>,
}

impl SendFail {
    fn before(message: String) -> SendFail {
        SendFail { sent: false, message, txid: None, inputs: Vec::new() }
    }
}

/// Check the quote's PSBT, sign it here with `key`, and send it with a fee-rate ceiling just above its own.
/// `must_spend_one_of`: the coins of an earlier payment that may have gone out; this one must spend one of them, so
/// only one of the two can confirm.
pub async fn sign_and_send(
    conn: &Conn,
    q: &Quote,
    key: &AccountKey,
    must_spend_one_of: Option<&[bitcoin::OutPoint]>,
) -> Result<String, SendFail> {
    let to = conn.script_of(&q.address).map_err(SendFail::before)?;
    let checked =
        sign::check(&q.psbt, &key.public, &q.expect(to)).map_err(SendFail::before)?;
    let inputs: Vec<bitcoin::OutPoint> = checked.tx.input.iter().map(|i| i.previous_output).collect();
    if let Some(earlier) = must_spend_one_of {
        if !inputs.iter().any(|i| earlier.contains(i)) {
            return Err(SendFail::before(
                "Your last payment from this wallet may still go out (the eCash node didn't say it took it). Wait until \
                 it shows under Payments, or half an hour."
                    .into(),
            ));
        }
    }
    let txid = checked.tx.compute_txid().to_string();
    let hex = sign::sign(checked.tx, &checked.spends, key).map_err(SendFail::before)?;
    let ceiling = sign::max_fee_rate(&hex, checked.fee).map_err(SendFail::before)?;
    match conn.node().call_typed("sendrawtransaction", vec![json!(hex), json!(ceiling)]).await {
        Ok(v) if v.as_str() == Some(txid.as_str()) => Ok(txid),
        // From here the signed payment is the node's: whatever it answers, it may have gone out.
        Ok(_) | Err(_) => Err(SendFail {
            sent: true,
            message: "The eCash node didn't say it took the payment, so it may have gone out. Look under Payments before \
                      sending again."
                .into(),
            txid: Some(txid),
            inputs,
        }),
    }
}
