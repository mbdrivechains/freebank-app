//! Bidding for FreeBank blocks (BMM) from the bidding wallet (v0.2.6; operator, 2026-10-04: "adn bmm now").
//!
//! The simple loop of the 2026-09-29 design (`gateway/docs/distribution/APP_SELF_SUFFICIENT_SCOPE.md`, "Bid (M8)"),
//! on the FreeBank node's calls for an outside bidder (freebankd v0.2.16 and later, `src/rpc/misc.cpp`):
//! 1. On each new eCash tip T, the live rounds are settled first: `get_bmm_inclusions h*` names the eCash block that
//!    committed our FreeBank block, and `connect_block` connects it (won). Nothing named means "not yet": a round is
//!    lost only after three new tips with nothing (security review M2).
//! 2. Then, if bidding is on and the day's bids leave room under the cap, `get_block_template`. Its `prev_main_hash`
//!    must be T: a template for another tip is never bid on.
//! 3. The bid is built and signed here (review H1, L4), never by the eCash node: an M8, `OP_RETURN 00bf00 <130> <h*>
//!    <T in internal order>`, as output 0 (the enforcer reads only the first output), the whole bid as the fee, the
//!    change to the bidding wallet's change address (checked against its key), eCash's replay stamp. Its coin is a
//!    confirmed coin of the bidding wallet (checked to be its own by its key), or the coin of a lost bid the eCash
//!    node still holds: eCash nodes keep a stale bid (for up to two weeks), so the new bid replaces it (BIP125: a fee
//!    above the old by its size in sats), which frees the coin. It is sent with a fee-rate ceiling just above its own.
//!
//! What counts against the day's cap is decided here, never by what the eCash node says (re-review L-A): every bid of
//! the last 24 hours, except that each bid spends one coin, so of the bids on one coin at most one can confirm, and
//! only the largest of their fees counts. A round is recorded in bmm.json before its bid goes out, so neither a crash
//! nor an error from the node can leave a bid uncounted. Settings and the rounds are kept in
//! `<app data>/wallet/bmm.json`.

use super::conn::Conn;
use super::keys::AccountKey;
use super::sign::{self, Spend};
use super::wallet;
use super::{sats_of, to_coins, REPLAY_LOCKTIME};
use crate::rpc::{FreeBankClient, RpcError};
use bitcoin::absolute::LockTime;
use bitcoin::script::PushBytesBuf;
use bitcoin::transaction::Version;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// FreeBank's sidechain slot.
pub const SLOT: u8 = 130;
/// BIP301's M8 tag.
const M8_TAG: &str = "00bf00";
/// A bid coin keeps at least this much as change (well above the dust limit), else it isn't used.
pub const MIN_CHANGE: u64 = 1_000;
/// Rounds older than a day kept in bmm.json (every round of the last day is kept: the cap counts them).
const KEEP_ROUNDS: usize = 200;
const DAY: u64 = 86_400;
/// connect_block and get_bmm_inclusions are retried on this many new tips before a round counts as failed.
const MAX_TRIES: u32 = 5;
/// New tips with no inclusion before a round counts as lost.
const MISSES_FOR_LOST: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Bmm {
    pub on: bool,
    /// The bid per FreeBank block, sats (the eCash miner gets it as the fee).
    pub bid: u64,
    /// At most this much in bids in any 24 hours, sats.
    pub daily_cap: u64,
    pub rounds: Vec<Round>,
}

impl Default for Bmm {
    fn default() -> Self {
        // 0.0001 eCash a bid, as the ticker's refreshbmm in the docs; 0.005 eCash a day.
        Bmm { on: false, bid: 10_000, daily_cap: 500_000, rounds: Vec::new() }
    }
}

/// A bid's coin, as the bid spent it (so a replacement doesn't take the node's word for its value).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Coin {
    pub txid: String,
    pub vout: u32,
    pub value: u64,
    pub branch: u32,
    pub index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Round {
    /// The eCash tip bid on (T), display order.
    pub main_tip: String,
    /// h*, as get_block_template gave it (internal order).
    pub critical: String,
    /// The FreeBank height of the block.
    pub height: u64,
    /// get_block_template's block object, for connect_block. Dropped once the round is final.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub block: Value,
    pub txid: String,
    pub fee: u64,
    pub at: u64,
    /// "live", "won", "lost", "replaced", "rejected" (the node refused the won block), "failed" (couldn't be settled).
    pub outcome: String,
    /// The eCash block that carried the bid, when won.
    pub main_block: Option<String>,
    #[serde(default)]
    pub tries: u32,
    /// New tips seen with no inclusion, and the last one counted.
    #[serde(default)]
    pub misses: u32,
    #[serde(default)]
    pub last_tip: String,
    /// The bid was seen confirmed: paid, whatever the outcome.
    #[serde(default)]
    pub paid: bool,
    /// Nothing more to watch: its coin is the wallet's again (abandoned, replaced, or confirmed).
    #[serde(default)]
    pub freed: bool,
    #[serde(default)]
    pub coin: Option<Coin>,
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("bmm.json")
}

pub fn load(app_dir: &Path) -> Bmm {
    std::fs::read(path(app_dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(app_dir: &Path, b: &Bmm) -> Result<(), String> {
    let p = path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    wallet::write_private(&p, &serde_json::to_vec_pretty(b).map_err(|e| e.to_string())?)
}

impl Bmm {
    /// What counts against the cap now: the bids of the last 24 hours, the bids on one coin counting once, at the
    /// largest of their fees (only one of them can confirm). Nothing the eCash node said changes it.
    pub fn spent_today(&self, now: u64) -> u64 {
        let mut by_coin: std::collections::HashMap<(&str, u32), u64> = std::collections::HashMap::new();
        let mut total = 0u64;
        for r in self.rounds.iter().filter(|r| r.at + DAY > now) {
            match &r.coin {
                Some(c) => {
                    let most = by_coin.entry((c.txid.as_str(), c.vout)).or_insert(0);
                    *most = (*most).max(r.fee);
                }
                None => total = total.saturating_add(r.fee),
            }
        }
        by_coin.values().fold(total, |t, f| t.saturating_add(*f))
    }

    /// Drop the oldest rounds beyond KEEP_ROUNDS, but never one of the last 24 hours.
    pub fn prune(&mut self, now: u64) {
        let old = self.rounds.iter().take_while(|r| r.at + DAY <= now).count();
        let cut = old.min(self.rounds.len().saturating_sub(KEEP_ROUNDS));
        self.rounds.drain(..cut);
    }
}

/// The M8's data: tag, slot, h* as given (internal order), then T reversed from display order.
pub fn m8_data(critical: &str, main_tip: &str) -> Result<String, String> {
    let hex32 = |s: &str| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit());
    if !hex32(critical) || !hex32(main_tip) {
        return Err("The FreeBank node's block template had a malformed hash.".into());
    }
    let reversed: String = (0..32).rev().map(|i| &main_tip[2 * i..2 * i + 2]).collect();
    Ok(format!("{}{:02x}{}{}", M8_TAG, SLOT, critical.to_ascii_lowercase(), reversed.to_ascii_lowercase()))
}

fn say(e: RpcError) -> String {
    match e {
        RpcError::Rpc { message, .. } => message,
        e => e.to_string(),
    }
}

/// What a tick did, for the screen's last line.
#[derive(Debug, Clone, PartialEq)]
pub enum Did {
    Nothing,
    /// Waiting: why (the cap, no coin, the node behind, ...).
    Waiting(String),
    Bid { txid: String, height: u64 },
}

/// The bidding wallet as a tick uses it: its name in the node and its key (from the owner-only key file).
pub struct Bids<'a> {
    pub name: &'a str,
    pub key: &'a AccountKey,
}

/// Keeping the rounds (to bmm.json) before a bid goes out: done, or why not.
pub type Kept = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// Settle the live rounds against the new tip, and watch the decided ones' bids. Changes `b`; never fails as a whole.
async fn settle(b: &mut Bmm, conn: &Conn, fb: &FreeBankClient, bids: &Bids<'_>, tip: &str) {
    // The FreeBank chain's height: a round whose height it already has, with no inclusion of ours, went to another
    // bidder (the walk-through's second run: it used to wait three eCash blocks, half an hour on beta).
    let mut fb_height: Option<Option<u64>> = None;
    for r in b.rounds.iter_mut().filter(|r| r.outcome == "live" && r.main_tip != tip) {
        match fb.call_typed("get_bmm_inclusions", vec![json!(r.critical)]).await {
            Ok(list) => match list.as_array().and_then(|l| l.first()).and_then(Value::as_str) {
                Some(main) => match fb.call_typed("connect_block", vec![r.block.clone(), json!(main)]).await {
                    Ok(Value::Bool(true)) => {
                        r.outcome = "won".into();
                        r.main_block = Some(main.to_string());
                    }
                    Ok(_) => {
                        // Included, so paid; the node refused the block.
                        r.outcome = "rejected".into();
                        r.paid = true;
                        r.main_block = Some(main.to_string());
                    }
                    Err(_) => r.tries += 1,
                },
                // "No inclusion yet": lost at once if the FreeBank chain already has a block at its height, else only after
                // a few new tips (the FreeBank node's eCash view may lag).
                None => {
                    if fb_height.is_none() {
                        fb_height = Some(fb.call_typed("getblockcount", vec![]).await.ok().and_then(|v| v.as_u64()));
                    }
                    if r.last_tip != tip {
                        r.misses += 1;
                        r.last_tip = tip.to_string();
                    }
                    if r.misses >= MISSES_FOR_LOST || fb_height.flatten().is_some_and(|h| h >= r.height) {
                        r.outcome = "lost".into();
                    }
                }
            },
            Err(_) => r.tries += 1,
        }
        if r.outcome == "live" && r.tries >= MAX_TRIES {
            r.outcome = "failed".into();
        }
        if r.outcome == "won" || r.outcome == "rejected" {
            r.block = Value::Null;
        }
    }
    // Decided rounds whose bid may still move: confirmed (paid; a "lost" one connected after all if it can be), or out
    // of the mempool and unconfirmed (abandoned, so its coin is the wallet's again).
    let w = conn.wallet(bids.name);
    for r in b.rounds.iter_mut().filter(|r| !r.freed && matches!(r.outcome.as_str(), "lost" | "failed" | "rejected" | "won")) {
        let Ok(t) = w.call_typed("gettransaction", vec![json!(r.txid), json!(true)]).await else {
            // The wallet doesn't know it (never sent, or another wallet): nothing to watch.
            r.freed = true;
            continue;
        };
        let conf = t["confirmations"].as_i64().unwrap_or(0);
        if conf > 0 {
            r.paid = true;
            r.freed = true;
            if r.outcome != "won" && r.outcome != "rejected" && !r.block.is_null() {
                if let Some(main) = t["blockhash"].as_str() {
                    if let Ok(Value::Bool(true)) = fb.call_typed("connect_block", vec![r.block.clone(), json!(main)]).await {
                        r.outcome = "won".into();
                        r.main_block = Some(main.to_string());
                    }
                }
            }
            r.block = Value::Null;
            continue;
        }
        if conf < 0 {
            // Its coin was spent by another bid (a replacement): nothing more.
            r.freed = true;
            r.block = Value::Null;
            continue;
        }
        if r.outcome == "won" {
            continue;
        }
        if conn.node().call_typed("getmempoolentry", vec![json!(r.txid)]).await.is_ok() {
            continue;
        }
        let abandoned = t["details"].as_array().is_some_and(|d| d.iter().any(|x| x["abandoned"] == Value::Bool(true)));
        if abandoned || w.call_typed("abandontransaction", vec![json!(r.txid)]).await.is_ok() {
            r.freed = true;
            r.block = Value::Null;
        }
    }
}

/// The smallest confirmed coin of the bidding wallet that pays `bid` and keeps MIN_CHANGE, checked to be its own by
/// its key.
async fn bid_coin(conn: &Conn, bids: &Bids<'_>, bid: u64) -> Result<Option<Coin>, String> {
    let list = conn.wallet(bids.name).call_typed("listunspent", vec![json!(1)]).await.map_err(say)?;
    let mut coins: Vec<Coin> = Vec::new();
    for u in list.as_array().into_iter().flatten() {
        let (Some(txid), Some(vout), Some(value), Some(desc), Some(spk)) = (
            u["txid"].as_str(),
            u["vout"].as_u64(),
            sats_of(&u["amount"]),
            u["desc"].as_str(),
            u["scriptPubKey"].as_str(),
        ) else {
            continue;
        };
        let Some((branch, index)) = bids.key.public.place_of_desc(desc) else { continue };
        if bids.key.public.script(branch, index)?.to_hex_string() != spk || value < bid + MIN_CHANGE {
            continue;
        }
        coins.push(Coin { txid: txid.into(), vout: vout as u32, value, branch, index });
    }
    coins.sort_by_key(|c| c.value);
    Ok(coins.into_iter().next())
}

/// A lost bid the eCash node still holds, to replace: its recorded coin, the fee the new bid must pay (the bid, or the
/// old fee plus one sat a vbyte of the old bid, BIP125's least, whichever is more) and its txid. None when there is
/// none, or replacing it would cost more than twice the bid (a fresh coin then).
async fn stale_bid(b: &Bmm, conn: &Conn) -> Option<(Coin, u64, String)> {
    let r = b.rounds.iter().rev().find(|r| (r.outcome == "lost" || r.outcome == "failed") && !r.freed && r.coin.is_some())?;
    let entry = conn.node().call_typed("getmempoolentry", vec![json!(r.txid)]).await.ok()?;
    let vsize = entry["vsize"].as_u64()?.min(400);
    let fee = b.bid.max(r.fee.saturating_add(vsize + 1));
    let coin = r.coin.clone()?;
    if fee > 2 * b.bid || coin.value < fee + MIN_CHANGE {
        return None;
    }
    Some((coin, fee, r.txid.clone()))
}

/// The bid transaction, unsigned: the coin in, the M8 first, the change after.
fn bid_tx(coin: &Coin, data_hex: &str, change: &ScriptBuf, fee: u64) -> Result<Transaction, String> {
    let data = hex::decode(data_hex).map_err(|e| e.to_string())?;
    let push = PushBytesBuf::try_from(data).map_err(|e| e.to_string())?;
    Ok(Transaction {
        version: Version::TWO,
        lock_time: LockTime::from_consensus(REPLAY_LOCKTIME),
        input: vec![TxIn {
            previous_output: OutPoint { txid: Txid::from_str(&coin.txid).map_err(|e| e.to_string())?, vout: coin.vout },
            script_sig: ScriptBuf::new(),
            sequence: Sequence(0xffff_fffd),
            witness: Witness::new(),
        }],
        output: vec![
            TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(&push) },
            TxOut { value: Amount::from_sat(coin.value - fee), script_pubkey: change.clone() },
        ],
    })
}

/// One pass: settle, then bid on the tip if there is room. `now` is unix seconds; `still_on` is asked again just
/// before a bid goes out (it may have been switched off meanwhile), and `keep` is given the rounds, the new one
/// included, to record before it does: if they can't be kept, the bid isn't sent.
pub async fn tick(
    b: &mut Bmm,
    conn: &Conn,
    fb: &FreeBankClient,
    bids: &Bids<'_>,
    now: u64,
    still_on: &(dyn Fn() -> bool + Send + Sync),
    keep: &(dyn Fn(Vec<Round>) -> Kept + Send + Sync),
) -> Result<Did, String> {
    let tip = conn.node().call_typed("getbestblockhash", vec![]).await.map_err(say)?;
    let tip = tip.as_str().ok_or("The eCash node gave no tip.")?.to_string();
    settle(b, conn, fb, bids, &tip).await;
    if !b.on {
        return Ok(Did::Nothing);
    }
    if b.rounds.iter().any(|r| r.main_tip == tip) {
        return Ok(Did::Nothing);
    }
    if b.spent_today(now) + b.bid > b.daily_cap {
        return Ok(Did::Waiting(format!("Today's bids have reached your cap of {} eCash.", to_coins(b.daily_cap))));
    }
    let t = match fb.call_typed("get_block_template", vec![]).await {
        Ok(t) => t,
        // -40: retry (the node is catching up, or the round isn't ready); -10: still syncing.
        Err(RpcError::Rpc { code: -40, message }) | Err(RpcError::Rpc { code: -10, message }) => {
            return Ok(Did::Waiting(format!("The FreeBank node isn't ready to bid: {}", message)))
        }
        Err(e) => return Err(format!("The FreeBank node gave no block to bid on: {}", say(e))),
    };
    let block = &t["block"];
    if block["prev_main_hash"].as_str() != Some(tip.as_str()) {
        return Ok(Did::Waiting("The FreeBank node's block is for another eCash block; trying again.".into()));
    }
    let critical = t["critical_hash"].as_str().ok_or("The block template had no h*.")?.to_string();
    let data = m8_data(&critical, &tip)?;
    // The coin: a stale lost bid's (replacing it), else a confirmed one.
    let (coin, fee, replaces) = match stale_bid(b, conn).await {
        Some((c, f, old)) => (c, f, Some(old)),
        None => {
            let Some(c) = bid_coin(conn, bids, b.bid).await? else {
                // Its coin may be in a bid still being decided.
                if let Some(r) = b.rounds.iter().rev().find(|r| r.outcome == "live") {
                    return Ok(Did::Waiting(format!("Waiting for the bid on block {} to be decided.", r.height)));
                }
                return Ok(Did::Waiting(format!(
                    "The bidding wallet needs a confirmed coin of at least {} eCash.",
                    to_coins(b.bid + MIN_CHANGE)
                )));
            };
            (c, b.bid, None)
        }
    };
    if b.spent_today(now) + fee > b.daily_cap {
        return Ok(Did::Waiting(format!("Today's bids have reached your cap of {} eCash.", to_coins(b.daily_cap))));
    }
    let (_, change_place) = wallet::change_address(conn, bids.name, &bids.key.public).await?;
    let change = bids.key.public.script(change_place.0, change_place.1)?;
    let tx = bid_tx(&coin, &data, &change, fee)?;
    let spend = Spend { vin: 0, place: (coin.branch, coin.index), value: coin.value, script: bids.key.public.script(coin.branch, coin.index)? };
    let unsigned_txid = tx.compute_txid();
    let hex = sign::sign(tx, &[spend], bids.key)?;
    let ceiling = sign::max_fee_rate(&hex, fee)?;
    if !still_on() {
        return Ok(Did::Nothing);
    }
    // Recorded before it goes out (re-review L-A): from here on the bid counts, whatever the node answers.
    let bid_txid = unsigned_txid.to_string();
    let height = block["height"].as_u64().unwrap_or(0);
    let before = b.rounds.clone();
    if let Some(old) = &replaces {
        // The two spend one coin, so only one can confirm: the old one is replaced whether or not the node takes this.
        if let Some(r) = b.rounds.iter_mut().find(|r| &r.txid == old) {
            r.outcome = "replaced".into();
            r.freed = true;
            r.block = Value::Null;
        }
    }
    b.rounds.push(Round {
        main_tip: tip.clone(),
        critical,
        height,
        block: block.clone(),
        txid: bid_txid.clone(),
        fee,
        at: now,
        outcome: "live".into(),
        main_block: None,
        tries: 0,
        misses: 0,
        last_tip: tip,
        paid: false,
        freed: false,
        coin: Some(coin),
    });
    b.prune(now);
    if let Err(e) = keep(b.rounds.clone()).await {
        b.rounds = before;
        return Err(format!("The bid couldn't be recorded, so it wasn't sent: {e}"));
    }
    match conn.node().call_typed("sendrawtransaction", vec![json!(hex), json!(ceiling)]).await {
        Ok(Value::String(t)) if t == bid_txid => Ok(Did::Bid { txid: bid_txid, height }),
        // It may have gone out all the same: it stays recorded and counted, and is settled as any other.
        Ok(_) => Err("The eCash node answered the bid strangely. It may have gone out; it counts toward today's cap.".into()),
        Err(e) => Err(format!("The eCash node gave an error for the bid ({}). It may have gone out; it counts toward today's cap.", say(e))),
    }
}

// ---- The loop ---------------------------------------------------------------------------------------------------

/// The bidding loop's settings and rounds (bmm.json, read once), whether it is on (for the check just before a bid
/// goes out), and what its last pass said (unix time, words).
pub struct Runner {
    state: tokio::sync::Mutex<Option<Bmm>>,
    on: std::sync::atomic::AtomicBool,
    last: std::sync::Mutex<Option<(u64, String)>>,
}

pub static RUNNER: std::sync::LazyLock<Runner> = std::sync::LazyLock::new(|| Runner {
    state: tokio::sync::Mutex::new(None),
    on: std::sync::atomic::AtomicBool::new(false),
    last: std::sync::Mutex::new(None),
});

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Runner {
    /// The settings and rounds now.
    pub async fn get(&self, app_dir: &Path) -> Bmm {
        let mut g = self.state.lock().await;
        let b = g.get_or_insert_with(|| load(app_dir));
        self.on.store(b.on, std::sync::atomic::Ordering::SeqCst);
        b.clone()
    }

    /// Change the settings (not the rounds) and keep them.
    pub async fn set(&self, app_dir: &Path, on: bool, bid: u64, daily_cap: u64) -> Result<Bmm, String> {
        let mut g = self.state.lock().await;
        let b = g.get_or_insert_with(|| load(app_dir));
        b.on = on;
        b.bid = bid;
        b.daily_cap = daily_cap;
        save(app_dir, b)?;
        self.on.store(on, std::sync::atomic::Ordering::SeqCst);
        if !on {
            *self.last.lock().unwrap() = None;
        }
        Ok(b.clone())
    }

    pub fn last(&self) -> Option<(u64, String)> {
        self.last.lock().unwrap().clone()
    }

    fn say(&self, words: String) {
        *self.last.lock().unwrap() = Some((now(), words));
    }
}

/// Start the loop: a pass every 10 seconds for the app's life.
pub fn spawn(mgr: std::sync::Arc<crate::node::NodeManager>) {
    tauri::async_runtime::spawn(async move {
        loop {
            pass(&mgr).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });
}

async fn pass(mgr: &crate::node::NodeManager) {
    if mgr.obliterated.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    // Work on a copy, so changing the settings never waits on a slow connect_block.
    let mut work = RUNNER.get(&mgr.app_dir).await;
    let busy = work.rounds.iter().any(|r| r.outcome == "live" || !r.freed);
    if !work.on && !busy {
        return;
    }
    let (conn, record) = match super::commands::ready(mgr).await {
        Ok(c) => c,
        Err(e) => return RUNNER.say(e),
    };
    let key = match wallet::read_bids_key(&mgr.app_dir, &record) {
        Ok(k) => k,
        Err(e) => return RUNNER.say(e),
    };
    let s = mgr.settings.lock().await.clone();
    // connect_block may take the node up to 3 minutes.
    let fb = crate::node::detect::local_client(&mgr.http, &s).with_timeout(std::time::Duration::from_secs(200));
    let before = work.rounds.clone();
    let bids = Bids { name: &record.bids_name, key: &key };
    let still_on = || RUNNER.on.load(std::sync::atomic::Ordering::SeqCst);
    let dir = mgr.app_dir.clone();
    let keep = move |rounds: Vec<Round>| -> Kept {
        let dir = dir.clone();
        Box::pin(async move {
            let mut g = RUNNER.state.lock().await;
            let cur = g.get_or_insert_with(|| load(&dir));
            cur.rounds = rounds;
            save(&dir, cur)
        })
    };
    match tick(&mut work, &conn, &fb, &bids, now(), &still_on, &keep).await {
        Ok(Did::Bid { height, .. }) => {
            // The round's own fee: a replacement pays more than the bid set.
            let r = work.rounds.last();
            let fee = r.map_or(work.bid, |r| r.fee);
            let replaces = r.is_some_and(|r| work.rounds.iter().any(|x| x.outcome == "replaced" && x.coin == r.coin));
            let note = if replaces { " (in place of the last bid, on the same coin)" } else { "" };
            RUNNER.say(format!("Bid {} eCash for FreeBank block {}{}.", to_coins(fee), height, note));
            crate::activity::note("bmm: bid sent");
        }
        Ok(Did::Waiting(w)) => RUNNER.say(w),
        Ok(Did::Nothing) => {}
        Err(e) => RUNNER.say(e),
    }
    if work.rounds != before {
        let mut g = RUNNER.state.lock().await;
        let cur = g.get_or_insert_with(|| load(&mgr.app_dir));
        cur.rounds = work.rounds;
        let _ = save(&mgr.app_dir, cur);
    }
}
