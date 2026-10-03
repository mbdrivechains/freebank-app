//! Send, Speed up and History (v0.2.0). The screens call the commands at the bottom of this file;
//! a send is built, signed and broadcast here, never through `rpc_call`.
//!
//! Send is ECX only (operator's decisions D-2026-09-29-5 and -7). It goes in two steps, so the fee
//! the screen shows is the fee paid:
//! - `send_prepare`: validateaddress, the fee rate of the chosen speed, then createrawtransaction and
//!   fundrawtransaction {feeRate, replaceable}. The funded transaction waits here under a random id
//!   for five minutes, while the screen shows its amount, fee and total.
//! - `send_confirm`: signrawtransactionwithwallet and sendrawtransaction, then a line in the send
//!   log. A locked wallet answers -13, and the screen's withUnlock (src/lib/wallet.ts) unlocks and
//!   asks again: the id stays good until the send has gone out.
//!
//! **Max** spends every safe, spendable coin that `listunspent 0 9999999 [] false` lists into the one
//! output, and takes the fee out of that output, so there is no change. freebankd's AvailableCoins,
//! behind both listunspent and fundrawtransaction, leaves note, bill, term-deposit and pool coins
//! out (wallet.cpp:2288-2351), so Max never sweeps credit. With no change, a Max send can't be sped
//! up.
//!
//! **Speeds** (Next block, Within an hour, Cheapest) are worked out when the Send tab opens and kept
//! for a minute. getmempoolinfo comes first: if the mempool fits in one block, every speed is the
//! minimum. Only when it doesn't does estimatesmartfee answer for 1, 6 and 144 blocks. Every rate is
//! at least 1 sat/vB (the wallet's DEFAULT_TRANSACTION_MINFEE) and the node's mempool minimum.
//!
//! **Speed up** is bumpfee with totalFee, since freebankd's bumpfee takes no fee rate. The total is
//! the chosen rate over the transaction's largest signed size, and never less than Core's floor for
//! a replacement: the old rate plus the incremental relay fee over that size (feebumper.cpp). The
//! extra comes out of the change. Only sends from this app's Send tab are sped up, because a credit
//! transaction's own outputs can look like change to bumpfee.
//!
//! **The send log** is `<app data>/sends.json` (0600, rewritten whole through a temp file). It keeps
//! what the wallet doesn't: the speed chosen, Max, the change, and which send replaced which.
//!
//! **History** is listtransactions, newest first, 25 to a page, with the log's speed and
//! replacements. `history_csv` writes all of it to a CSV file in Documents.

use crate::commands::ClientState;
use crate::node::NodeManager;
use crate::rpc::FreeBankClient;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, State};

// ---- Amounts ---------------------------------------------------------------------------------

/// Sats in one ECX.
pub const COIN: i64 = 100_000_000;
/// All the ECX there can be, in sats. Amounts past it are refused.
pub const MAX_SATS: i64 = 21_000_000 * COIN;

/// Sats as the exact 8-place decimal every amount goes to the node in: 150000000 → "1.50000000".
pub fn ecx(sats: i64) -> String {
    let sign = if sats < 0 { "-" } else { "" };
    let a = sats.unsigned_abs();
    let coin = COIN as u64;
    format!("{}{}.{:08}", sign, a / coin, a % coin)
}

/// An ECX amount from the node (a JSON number, or a string) in sats. The node writes 8 decimals, and
/// an f64 holds every such amount up to 21 million ECX closely enough for rounding to get it back.
pub fn sats(v: &Value) -> Option<i64> {
    let f = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !f.is_finite() || f.abs() > 2.0 * (MAX_SATS / COIN) as f64 {
        return None;
    }
    Some((f * COIN as f64).round() as i64)
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A txid shortened for a sentence: "3f2a9c1e…".
fn short(txid: &str) -> String {
    format!("{}…", txid.get(..8).unwrap_or(txid))
}

// ---- Fee rates -------------------------------------------------------------------------------

/// A fee rate in sats per 1,000 vbytes, Core's CFeeRate unit: 1,000 is 1 sat/vB.
pub type Rate = i64;

/// The wallet's minimum fee rate, 1 sat/vB (DEFAULT_TRANSACTION_MINFEE).
pub const MIN_RATE: Rate = 1_000;
/// What freebankd's miner fills a block with, in vbytes: DEFAULT_BLOCK_MAX_WEIGHT, which is
/// MAX_BLOCK_WEIGHT (4,000,000) less 4,000 for the coinbase.
pub const BLOCK_VBYTES: i64 = (4_000_000 - 4_000) / 4;
/// Core's incremental relay fee, when the node doesn't say (DEFAULT_INCREMENTAL_RELAY_FEE).
const INCREMENTAL_RATE: Rate = 1_000;
/// The most fee Core lets a wallet transaction pay (DEFAULT_TRANSACTION_MAXFEE, 0.1 ECX).
const MAX_TX_FEE: i64 = COIN / 10;
/// How long the fee choices are kept.
const FEES_FOR: Duration = Duration::from_secs(60);
/// How long a prepared send, or a Speed up quote, holds.
const HOLD: Duration = Duration::from_secs(5 * 60);

/// CFeeRate::GetFee: `rate` over `vsize`, rounded down, but at least 1 sat for a rate above zero.
fn fee_at(rate: Rate, vsize: i64) -> i64 {
    let f = rate * vsize / 1000;
    if f == 0 && rate > 0 && vsize > 0 {
        1
    } else {
        f
    }
}

/// `rate` over `vsize`, rounded up: a fee that pays at least that rate.
fn fee_for(rate: Rate, vsize: i64) -> i64 {
    (rate * vsize + 999) / 1000
}

fn per_vb(rate: Rate) -> f64 {
    rate as f64 / 1000.0
}

/// How soon a send should confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Next,
    Hour,
    Cheap,
}

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Next, Speed::Hour, Speed::Cheap];

    pub fn label(self) -> &'static str {
        match self {
            Speed::Next => "Next block",
            Speed::Hour => "Within an hour",
            Speed::Cheap => "Cheapest",
        }
    }

    /// estimatesmartfee's target. FreeBank blocks follow eCash blocks, about ten minutes apart, so
    /// six is about an hour. (Core reads a target of 1 as 2.)
    pub fn blocks(self) -> u32 {
        match self {
            Speed::Next => 1,
            Speed::Hour => 6,
            Speed::Cheap => 144,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeeChoice {
    pub speed: Speed,
    pub label: &'static str,
    pub sat_per_kvb: Rate,
    pub sat_per_vb: f64,
}

/// The three speeds, as the Send tab shows them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeeChoices {
    /// Next block, Within an hour, Cheapest.
    pub choices: Vec<FeeChoice>,
    /// All three are the same rate: the screen shows one line instead of a choice.
    pub same: bool,
    /// Where the rates came from. "quiet": the mempool fits in one block, so the minimum gets into
    /// the next one. "estimates": estimatesmartfee for all three. "partial": some estimates were
    /// missing and the minimum stands in for them. "none": the mempool is over a block and the node
    /// has no fee data yet (a young chain), so all three are the minimum.
    pub basis: &'static str,
    /// The mempool's size in vbytes.
    pub mempool_vbytes: i64,
    /// The lowest rate any choice can have: 1 sat/vB, or the node's mempool minimum if higher.
    pub floor_sat_per_kvb: Rate,
    /// When they were worked out, unix seconds.
    pub at: u64,
}

impl FeeChoices {
    pub fn rate(&self, speed: Speed) -> Rate {
        self.choices.iter().find(|c| c.speed == speed).map_or(self.floor_sat_per_kvb, |c| c.sat_per_kvb)
    }
}

/// The choices from the estimates for 1, 6 and 144 blocks (None: missing). Each is at least the
/// floor, and a faster speed never costs less than a slower one.
pub fn choices_from(est: [Option<Rate>; 3], floor: Rate, basis: &'static str, mempool_vbytes: i64) -> FeeChoices {
    let cheap = est[2].unwrap_or(floor).max(floor);
    let hour = est[1].unwrap_or(floor).max(cheap);
    let next = est[0].unwrap_or(floor).max(hour);
    let choices = Speed::ALL
        .iter()
        .zip([next, hour, cheap])
        .map(|(s, r)| FeeChoice { speed: *s, label: s.label(), sat_per_kvb: r, sat_per_vb: per_vb(r) })
        .collect();
    FeeChoices { choices, same: next == cheap, basis, mempool_vbytes, floor_sat_per_kvb: floor, at: now_unix() }
}

/// getmempoolinfo, and estimatesmartfee only when the mempool is bigger than a block.
pub async fn work_out_fees(c: &mut FreeBankClient) -> Result<FeeChoices, String> {
    let mp = c.call_ui("getmempoolinfo", vec![]).await?;
    let floor = [sats(&mp["mempoolminfee"]), sats(&mp["minrelaytxfee"])].into_iter().flatten().fold(MIN_RATE, i64::max);
    let vbytes = mp["bytes"].as_i64().unwrap_or(0);
    if vbytes <= BLOCK_VBYTES {
        return Ok(choices_from([None; 3], floor, "quiet", vbytes));
    }
    let mut est = [None; 3];
    for (i, s) in Speed::ALL.iter().enumerate() {
        // No data yet answers {"errors": [...]}: the floor stands in.
        if let Ok(v) = c.call_ui("estimatesmartfee", vec![json!(s.blocks())]).await {
            est[i] = sats(&v["feerate"]).filter(|r| *r > 0);
        }
    }
    let basis = match est.iter().flatten().count() {
        3 => "estimates",
        0 => "none",
        _ => "partial",
    };
    Ok(choices_from(est, floor, basis, vbytes))
}

// ---- What waits between the steps -----------------------------------------------------------

/// Prepared sends, the fee choices and Speed up quotes, in memory only.
pub struct Book {
    held: Mutex<HashMap<String, Held>>,
    fees: Mutex<Option<(Instant, FeeChoices)>>,
    quotes: Mutex<HashMap<String, (Instant, BumpQuote)>>,
    hold: Duration,
    fees_for: Duration,
}

impl Default for Book {
    fn default() -> Self {
        Self::new(HOLD, FEES_FOR)
    }
}

/// The app's one book. (The commands use it; tests make their own.)
pub static BOOK: LazyLock<Book> = LazyLock::new(Book::default);

struct Held {
    quote: Prepared,
    hex: String,
    made: Instant,
}

impl Book {
    /// A book whose prepared sends and quotes hold for `hold`, and fee choices for `fees_for`.
    pub fn new(hold: Duration, fees_for: Duration) -> Self {
        Self {
            held: Mutex::default(),
            fees: Mutex::default(),
            quotes: Mutex::default(),
            hold,
            fees_for,
        }
    }

    /// The fee choices: the ones worked out in the last minute, or new ones.
    pub async fn fees(&self, c: &mut FreeBankClient, refresh: bool) -> Result<FeeChoices, String> {
        if !refresh {
            if let Some((at, f)) = self.fees.lock().unwrap().as_ref() {
                if at.elapsed() < self.fees_for {
                    return Ok(f.clone());
                }
            }
        }
        let f = work_out_fees(c).await?;
        *self.fees.lock().unwrap() = Some((Instant::now(), f.clone()));
        Ok(f)
    }

    fn keep(&self, id: &str, h: Held) {
        let mut m = self.held.lock().unwrap();
        m.retain(|_, h| h.made.elapsed() < self.hold);
        m.insert(id.to_string(), h);
    }

    /// Take a prepared send out while it is signed and sent, so it can't go twice.
    fn take(&self, id: &str) -> Result<Held, String> {
        match self.held.lock().unwrap().remove(id) {
            Some(h) if h.made.elapsed() < self.hold => Ok(h),
            _ => Err(EXPIRED.into()),
        }
    }

    /// Put it back after a failure (a locked wallet, say), while it still holds.
    fn put_back(&self, id: &str, h: Held) {
        if h.made.elapsed() < self.hold {
            self.held.lock().unwrap().insert(id.to_string(), h);
        }
    }

    fn quote(&self, txid: &str) -> Option<BumpQuote> {
        let m = self.quotes.lock().unwrap();
        m.get(txid).filter(|(at, _)| at.elapsed() < self.hold).map(|(_, q)| q.clone())
    }
}

// ---- Words for the screen ---------------------------------------------------------------------

const NO_ADDRESS: &str = "Enter the address to send to.";
const NOT_AN_ADDRESS: &str = "That isn't a FreeBank address. Check it and try again.";
const AMOUNT_PROBLEM: &str = "Enter an amount in ECX above zero, with at most 8 decimal places.";
const NOTHING_TO_SEND: &str =
    "There's nothing to send yet: this wallet has no spendable ECX. Coins still on their way can be sent once they confirm.";
const NOT_ENOUGH: &str =
    "You don't have enough spendable ECX for this amount and its fee. Max sends everything, with the fee taken out of it.";
const TOO_SMALL: &str = "That amount is too small to send: the network won't pass on a payment that small.";
const FEE_EATS_IT: &str = "That's too little to cover the fee.";
const EXPIRED: &str = "This send's quote has expired, so nothing was sent. Review it again to see the fee as it is now.";
const NOT_FROM_SEND_TAB: &str = "Only sends made in this app's Send tab can be sped up.";
const MAX_CANT: &str = "A Max send has no change to pay a higher fee from, so it can't be sped up.";
const NO_CHANGE: &str = "This send left no change to pay a higher fee from, so it can't be sped up.";
const NOT_REPLACEABLE: &str =
    "This send wasn't marked replaceable, so it can't be sped up. Sends made before FreeBank app v0.2.0 weren't.";
const QUOTE_EXPIRED: &str = "The new fee's quote has expired. Press Speed up again to see it as it is now.";
const CHANGE_TOO_SMALL: &str = "This send's change is too small to pay a higher fee, so it can't be sped up.";

/// fundrawtransaction's failures, in plain words. "Keypool ran out" becomes the node's -12, so the
/// screen's withUnlock unlocks the wallet (which refills its keys) and asks again.
fn funding_problem(e: String) -> String {
    if e.contains("Insufficient funds") {
        NOT_ENOUGH.into()
    } else if e.contains("Keypool ran out") {
        format!("RPC error -12: {}", e.split_once(": ").map_or(e.as_str(), |(_, m)| m))
    } else if e.contains("too small to pay the fee") || e.contains("too small to send after the fee") {
        FEE_EATS_IT.into()
    } else if e.contains("amount too small") || e.contains("amount is too small") {
        TOO_SMALL.into()
    } else {
        e
    }
}

/// sendrawtransaction's failures, in plain words.
fn broadcast_problem(e: String) -> String {
    if e.starts_with("RPC error -25:") {
        "Some of the coins this send used have been spent since you reviewed it. Review it again.".into()
    } else if let Some(m) = e.strip_prefix("RPC error -26: ") {
        format!("The network refused this send: {}", m)
    } else {
        e
    }
}

/// bumpfee's failures, in plain words. -13 keeps its code, for withUnlock.
fn bump_problem(e: String) -> String {
    if e.starts_with("RPC error -13:") {
        e
    } else if e.contains("does not have a change output") {
        NO_CHANGE.into()
    } else if e.contains("Change output is too small") {
        CHANGE_TOO_SMALL.into()
    } else if e.contains("has descendants") {
        "A later transaction spends this send's change, so it can't be sped up.".into()
    } else if e.contains("has been mined") {
        "This send has confirmed already.".into()
    } else if e.contains("not BIP 125 replaceable") {
        NOT_REPLACEABLE.into()
    } else if e.contains("already bumped") {
        "This send was sped up already. Speed up the newer one instead.".into()
    } else {
        e
    }
}

// ---- Prepare and confirm ----------------------------------------------------------------------

/// What the Send tab asks for.
#[derive(Debug, Clone, Deserialize)]
pub struct SendRequest {
    pub address: String,
    /// Sats to the address; unused with `max`.
    pub amount: Option<i64>,
    #[serde(default)]
    pub max: bool,
    pub speed: Speed,
}

/// A send ready to confirm, as the screen shows it. All amounts are sats.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Prepared {
    pub id: String,
    /// The address as the node writes it.
    pub address: String,
    /// What the address gets.
    pub amount: i64,
    pub fee: i64,
    /// What leaves the wallet: amount + fee.
    pub total: i64,
    pub sat_per_kvb: Rate,
    pub sat_per_vb: f64,
    /// The size once signed, as the fee was worked out for it.
    pub vsize: i64,
    /// What comes back to the wallet; 0 means no change, so the send can't be sped up later.
    pub change: i64,
    pub max: bool,
    pub speed: Speed,
    pub label: &'static str,
    /// Seconds until Confirm stops working.
    pub expires_in: u64,
}

/// A coin from listunspent.
#[derive(Debug, Clone)]
struct Coin {
    txid: String,
    vout: u32,
    sats: i64,
    script: String,
    redeem: Option<String>,
}

type Outpoint = (String, u32);

/// listunspent's spendable coins (watch-only ones can't be signed for). Zero-value ones are listed
/// too (freebankd's block rewards can be 0 ECX): Max leaves them out, but a funded transaction may
/// use one, and the fee check needs to know it.
fn coins_from(v: &Value) -> Vec<Coin> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter(|u| u["spendable"] == Value::Bool(true))
        .filter_map(|u| {
            Some(Coin {
                txid: u["txid"].as_str()?.to_string(),
                vout: u32::try_from(u["vout"].as_u64()?).ok()?,
                sats: sats(&u["amount"]).filter(|s| *s >= 0)?,
                script: u["scriptPubKey"].as_str().unwrap_or("").to_ascii_lowercase(),
                redeem: u["redeemScript"].as_str().map(|r| r.to_ascii_lowercase()),
            })
        })
        .collect()
}

/// decoderawtransaction's inputs.
fn tx_inputs(tx: &Value) -> Vec<Outpoint> {
    tx["vin"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| Some((i["txid"].as_str()?.to_string(), u32::try_from(i["vout"].as_u64()?).ok()?)))
        .collect()
}

/// decoderawtransaction's outputs: sats, and the addresses each pays.
fn tx_outputs(tx: &Value) -> Vec<(i64, Vec<String>)> {
    tx["vout"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| {
            let to = o["scriptPubKey"]["addresses"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            (sats(&o["value"]).unwrap_or(-1), to)
        })
        .collect()
}

/// The weight a coin's spend adds once signed, as Core's dummy signer reckons it (72-byte
/// signatures, 33-byte keys), and whether it adds a witness.
fn spend_weight(script: &str, redeem: Option<&str>) -> (i64, bool) {
    let s = script;
    let p2sh_p2wpkh = redeem.is_some_and(|r| r.len() == 44 && r.starts_with("0014"));
    if s.len() == 50 && s.starts_with("76a914") && s.ends_with("88ac") {
        (4 * 107, false) // P2PKH: a signature and a key in the script
    } else if s.len() == 44 && s.starts_with("0014") {
        (108, true) // P2WPKH: a signature and a key in the witness
    } else if s.len() == 46 && s.starts_with("a914") && s.ends_with("87") && p2sh_p2wpkh {
        (4 * 23 + 108, true) // P2SH-P2WPKH, freebankd's default change: the script push, then the witness
    } else if (s.len() == 70 || s.len() == 134) && s.ends_with("ac") {
        (4 * 73, false) // P2PK: a signature alone
    } else {
        (4 * 107, false)
    }
}

/// An unsigned transaction's size once signed: its weight, plus what each input's signature adds.
fn signed_vsize(tx: &Value, coins: &HashMap<Outpoint, &Coin>) -> i64 {
    let ins = tx_inputs(tx);
    let mut weight = tx["weight"].as_i64().unwrap_or_else(|| tx["size"].as_i64().unwrap_or(0) * 4);
    let mut witnesses = 0;
    for k in &ins {
        let (w, witness) = coins.get(k).map_or((4 * 107, false), |c| spend_weight(&c.script, c.redeem.as_deref()));
        weight += w;
        witnesses += witness as i64;
    }
    if witnesses > 0 {
        // The segwit marker and flag, and an empty witness for every other input.
        weight += 2 + (ins.len() as i64 - witnesses);
    }
    (weight + 3) / 4
}

/// Build and fund a send, and keep it under a new id until it is confirmed or five minutes pass.
pub async fn prepare(book: &Book, c: &mut FreeBankClient, req: SendRequest) -> Result<Prepared, String> {
    let typed = req.address.trim();
    if typed.is_empty() {
        return Err(NO_ADDRESS.into());
    }
    let amount = match (req.max, req.amount) {
        (true, _) => None,
        (false, Some(a)) if a > 0 && a <= MAX_SATS => Some(a),
        _ => return Err(AMOUNT_PROBLEM.into()),
    };
    let v = c.call_ui("validateaddress", vec![json!(typed)]).await?;
    if v["isvalid"] != Value::Bool(true) {
        return Err(NOT_AN_ADDRESS.into());
    }
    let address = v["address"].as_str().unwrap_or(typed).to_string();

    let fees = book.fees(c, false).await?;
    let rate = fees.rate(req.speed);
    // Safe coins only (not unconfirmed ones from others), as fundrawtransaction chooses from.
    let coins = coins_from(&c.call_ui("listunspent", vec![json!(0), json!(9_999_999), json!([]), json!(false)]).await?);
    let by_outpoint: HashMap<Outpoint, &Coin> = coins.iter().map(|c| ((c.txid.clone(), c.vout), c)).collect();

    // Max spends the coins worth something: a zero-value one would only add to the fee.
    let worth: Vec<&Coin> = coins.iter().filter(|c| c.sats > 0).collect();
    let mut to = Map::new();
    let (raw, options, sum) = match amount {
        None => {
            let sum: i64 = worth.iter().map(|c| c.sats).sum();
            if sum <= 0 {
                return Err(NOTHING_TO_SEND.into());
            }
            to.insert(address.clone(), json!(ecx(sum)));
            let inputs: Vec<Value> = worth.iter().map(|c| json!({"txid": c.txid, "vout": c.vout})).collect();
            // replaceable here: fundrawtransaction keeps the sequence of inputs it is given.
            let raw = c.call_ui("createrawtransaction", vec![json!(inputs), Value::Object(to), json!(0), json!(true)]).await?;
            (raw, json!({"feeRate": ecx(rate), "replaceable": true, "subtractFeeFromOutputs": [0]}), sum)
        }
        Some(a) => {
            to.insert(address.clone(), json!(ecx(a)));
            let raw = c.call_ui("createrawtransaction", vec![json!([]), Value::Object(to)]).await?;
            (raw, json!({"feeRate": ecx(rate), "replaceable": true}), 0)
        }
    };
    let funded = c.call_ui("fundrawtransaction", vec![raw, options]).await.map_err(funding_problem)?;
    let hex = funded["hex"].as_str().ok_or("The node's funded transaction came back empty.")?.to_string();
    let fee = sats(&funded["fee"]).filter(|f| *f >= 0).ok_or("The node didn't say what the fee is.")?;
    let changepos = funded["changepos"].as_i64().unwrap_or(-1);

    // Check the transaction is what the screen will show before keeping it.
    let tx = c.call_ui("decoderawtransaction", vec![json!(hex)]).await?;
    let ins = tx_inputs(&tx);
    let outs = tx_outputs(&tx);
    let change = usize::try_from(changepos).ok().and_then(|i| outs.get(i)).map_or(0, |o| o.0);
    let amount = match amount {
        None => {
            // Exactly the coins worth something, one output, and the fee taken out of it: no change.
            let want: HashSet<Outpoint> = worth.iter().map(|c| (c.txid.clone(), c.vout)).collect();
            let got: HashSet<Outpoint> = ins.iter().cloned().collect();
            if got != want || outs.len() != 1 || changepos != -1 || outs[0].0 != sum - fee || outs[0].0 <= 0 {
                return Err("Max couldn't be built from exactly your spendable coins. Try again.".into());
            }
            outs[0].0
        }
        Some(a) => {
            let pays = outs.iter().enumerate().any(|(i, o)| i as i64 != changepos && o.0 == a && o.1.contains(&address));
            if !pays {
                return Err("The node built a transaction that doesn't pay this address this amount. Nothing was sent.".into());
            }
            a
        }
    };
    // What goes in less what goes out is the fee shown (inputs the wallet listed, which is all of them).
    if let Some(inputs) = ins.iter().map(|k| by_outpoint.get(k).map(|c| c.sats)).sum::<Option<i64>>() {
        let out: i64 = outs.iter().map(|o| o.0).sum();
        if inputs - out != fee {
            return Err("The node's fee doesn't match its transaction. Nothing was sent.".into());
        }
    }

    let id = hex::encode(rand::random::<[u8; 16]>());
    let quote = Prepared {
        id: id.clone(),
        address,
        amount,
        fee,
        total: amount + fee,
        sat_per_kvb: rate,
        sat_per_vb: per_vb(rate),
        vsize: signed_vsize(&tx, &by_outpoint),
        change,
        max: req.max,
        speed: req.speed,
        label: req.speed.label(),
        expires_in: book.hold.as_secs(),
    };
    book.keep(&id, Held { quote: quote.clone(), hex, made: Instant::now() });
    Ok(quote)
}

/// A send that has gone out.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sent {
    pub txid: String,
    pub address: String,
    pub amount: i64,
    pub fee: i64,
    pub sat_per_vb: f64,
    pub speed: Speed,
    pub label: &'static str,
    pub max: bool,
    pub change: i64,
    /// unix seconds
    pub time: u64,
    /// The send went out, but the log couldn't be written (the send itself is fine).
    pub log_error: Option<String>,
}

/// Sign and send a prepared send, then log it.
pub async fn confirm(book: &Book, c: &mut FreeBankClient, id: &str, log: Option<&SendLog>) -> Result<Sent, String> {
    let held = book.take(id)?;
    let txid = match sign_and_send(c, &held.hex).await {
        Ok(t) => t,
        Err(e) => {
            book.put_back(id, held);
            return Err(e);
        }
    };
    let q = held.quote;
    let time = now_unix();
    let entry = LogEntry {
        txid: txid.clone(),
        time,
        address: q.address.clone(),
        amount: q.amount,
        fee: q.fee,
        feerate: q.sat_per_kvb,
        speed: q.speed,
        max: q.max,
        change: q.change,
        replaced_by: None,
        replaces: None,
    };
    let log_error = log.map_or(Ok(()), |l| l.append(entry)).err();
    Ok(Sent {
        txid,
        address: q.address,
        amount: q.amount,
        fee: q.fee,
        sat_per_vb: q.sat_per_vb,
        speed: q.speed,
        label: q.label,
        max: q.max,
        change: q.change,
        time,
        log_error,
    })
}

/// What the node says when a locked wallet is asked to sign (RPC_WALLET_UNLOCK_NEEDED).
const LOCKED: &str = "RPC error -13: Error: Please enter the wallet passphrase with walletpassphrase first.";

async fn sign_and_send(c: &mut FreeBankClient, hex: &str) -> Result<String, String> {
    let signed = c.call_ui("signrawtransactionwithwallet", vec![json!(hex)]).await?;
    if signed["complete"] != Value::Bool(true) {
        // freebankd's signrawtransactionwithwallet (Core 0.16) doesn't check the lock: a locked
        // wallet answers "complete": false, "Unable to sign input, invalid stack size (possibly
        // missing key)". Say it as the node's -13, so the screen's withUnlock unlocks and asks
        // again, as it does for sendtoaddress.
        if crate::wallet::status(c).await.is_ok_and(|s| s.encrypted && s.unlocked_until == 0) {
            return Err(LOCKED.into());
        }
        let why = signed["errors"][0]["error"].as_str().unwrap_or("not every coin could be signed for");
        return Err(format!("The wallet couldn't sign this send ({}). Nothing was sent.", why));
    }
    let hex = signed["hex"].as_str().ok_or("The wallet's signed transaction came back empty.")?;
    let txid = c.call_ui("sendrawtransaction", vec![json!(hex)]).await.map_err(broadcast_problem)?;
    txid.as_str().map(String::from).ok_or_else(|| "The node didn't say the send's transaction ID.".into())
}

// ---- Speed up ---------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BumpChoice {
    pub speed: Speed,
    pub label: &'static str,
    /// The new total fee, sats.
    pub fee: i64,
    /// What it adds to the old fee; it comes out of the change.
    pub extra: i64,
    /// The rate the new fee pays.
    pub sat_per_vb: f64,
    /// The change can pay it.
    pub ok: bool,
}

/// What Speed up would cost, per speed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BumpQuote {
    pub txid: String,
    pub old_fee: i64,
    pub old_sat_per_vb: f64,
    pub vsize: i64,
    pub choices: Vec<BumpChoice>,
    /// All three cost the same: the screen shows one line.
    pub same: bool,
}

/// The largest size Core's fee bumper reckons a signed transaction at (feebumper.cpp,
/// CalculateMaximumSignedTxSize, with every signature 72 bytes), and a vbyte to spare.
fn max_signed_vsize(tx: &Value) -> i64 {
    let mut weight = tx["weight"].as_i64().unwrap_or_else(|| tx["size"].as_i64().unwrap_or(0) * 4);
    for i in tx["vin"].as_array().into_iter().flatten() {
        if let Some(w) = i["txinwitness"].as_array().filter(|w| !w.is_empty()) {
            let sig = w[0].as_str().map_or(72, |h| h.len() as i64 / 2);
            weight += (72 - sig).max(0);
        } else {
            let script = i["scriptSig"]["hex"].as_str().unwrap_or("");
            let first = script.get(0..2).and_then(|b| i64::from_str_radix(b, 16).ok()).unwrap_or(0);
            // A signature push is 9 to 73 bytes; one starting the script is the one to measure.
            weight += if (60..=73).contains(&first) { 4 * (72 - first).max(0) } else { 8 };
        }
    }
    (weight + 3) / 4 + 1
}

/// What speeding up `txid` would cost at each speed. Only a send from this Send tab, still
/// unconfirmed, replaceable and with change, can be sped up. The quote is kept for Confirm.
pub async fn quote_speed_up(book: &Book, c: &mut FreeBankClient, log: &[LogEntry], txid: &str) -> Result<BumpQuote, String> {
    let entry = log.iter().find(|e| e.txid == txid).ok_or(NOT_FROM_SEND_TAB)?;
    if let Some(by) = &entry.replaced_by {
        return Err(format!("This send was sped up already: {} replaced it.", short(by)));
    }
    if entry.max {
        return Err(MAX_CANT.into());
    }
    if entry.change <= 0 {
        return Err(NO_CHANGE.into());
    }
    let t = c.call_ui("gettransaction", vec![json!(txid)]).await?;
    let confirmations = t["confirmations"].as_i64().unwrap_or(0);
    if confirmations > 0 {
        return Err("This send has confirmed already.".into());
    }
    if confirmations < 0 {
        return Err("A different transaction spending the same coins confirmed instead.".into());
    }
    if let Some(by) = t["replaced_by_txid"].as_str() {
        return Err(format!("This send was sped up already: {} replaced it.", short(by)));
    }
    if t["bip125-replaceable"].as_str() != Some("yes") {
        return Err(NOT_REPLACEABLE.into());
    }
    let old_fee = sats(&t["fee"]).map(|f| -f).filter(|f| *f > 0).ok_or("The wallet doesn't say this send's fee.")?;
    let hex = t["hex"].as_str().ok_or("The wallet doesn't have this send's transaction.")?;
    let tx = c.call_ui("decoderawtransaction", vec![json!(hex)]).await?;
    let vsize = tx["vsize"].as_i64().filter(|v| *v > 0).ok_or("The node couldn't read this send's transaction.")?;
    let max_vsize = max_signed_vsize(&tx);
    // The change is what doesn't go to the address sent to.
    let change: i64 = tx_outputs(&tx).iter().filter(|(_, to)| !to.contains(&entry.address)).map(|o| o.0).sum();
    if change <= 0 {
        return Err(NO_CHANGE.into());
    }

    // Core's floor for a replacement: the old rate and the incremental relay fee, both over the
    // largest size, and never under the wallet's or the relay minimum.
    let net = c.call_ui("getnetworkinfo", vec![]).await.ok();
    let incremental = net.as_ref().and_then(|n| sats(&n["incrementalfee"])).filter(|r| *r > 0).unwrap_or(INCREMENTAL_RATE);
    let relay = net.as_ref().and_then(|n| sats(&n["relayfee"])).unwrap_or(MIN_RATE).max(MIN_RATE);
    let old_rate = old_fee * 1000 / vsize;
    let least = (fee_at(old_rate, max_vsize) + fee_at(incremental, max_vsize)).max(fee_at(relay, max_vsize));
    let fees = book.fees(c, false).await?;
    let choices: Vec<BumpChoice> = Speed::ALL
        .iter()
        .map(|&s| {
            let fee = fee_for(fees.rate(s), max_vsize).max(least);
            BumpChoice {
                speed: s,
                label: s.label(),
                fee,
                extra: fee - old_fee,
                sat_per_vb: (fee as f64 / max_vsize as f64 * 1000.0).round() / 1000.0,
                ok: fee - old_fee <= change && fee <= MAX_TX_FEE,
            }
        })
        .collect();
    if !choices.iter().any(|c| c.ok) {
        return Err(CHANGE_TOO_SMALL.into());
    }
    let q = BumpQuote {
        txid: txid.to_string(),
        old_fee,
        old_sat_per_vb: per_vb(old_rate),
        vsize,
        same: choices.iter().all(|c| c.fee == choices[0].fee),
        choices,
    };
    book.quotes.lock().unwrap().insert(txid.to_string(), (Instant::now(), q.clone()));
    Ok(q)
}

/// A send sped up: the new transaction replaces the old.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bumped {
    pub txid: String,
    pub old_txid: String,
    pub fee: i64,
    pub old_fee: i64,
    pub speed: Speed,
    pub label: &'static str,
    pub sat_per_vb: f64,
    /// The new transaction went out, but the log couldn't be written.
    pub log_error: Option<String>,
}

/// Speed up `txid` at `speed`, for the fee its quote showed (bumpfee totalFee), and log the
/// replacement.
pub async fn speed_up(book: &Book, c: &mut FreeBankClient, log: Option<&SendLog>, txid: &str, speed: Speed) -> Result<Bumped, String> {
    let q = book.quote(txid).ok_or(QUOTE_EXPIRED)?;
    let choice = q.choices.iter().find(|c| c.speed == speed && c.ok).ok_or("That speed isn't available for this send.")?;
    let r = c.call_ui("bumpfee", vec![json!(txid), json!({"totalFee": choice.fee})]).await.map_err(bump_problem)?;
    if let Some(e) = r["errors"].as_array().and_then(|a| a.first()).and_then(Value::as_str) {
        return Err(format!("The node made the faster transaction but didn't accept it: {}", e));
    }
    let new = r["txid"].as_str().ok_or("The node didn't say the new transaction's ID.")?.to_string();
    // Core adds change it would leave as dust to the fee, so its figure can be a little higher.
    let fee = sats(&r["fee"]).unwrap_or(choice.fee);
    book.quotes.lock().unwrap().remove(txid);

    let log_error = match log {
        None => None,
        Some(l) => {
            let old = l.read().ok().and_then(|s| s.into_iter().find(|e| e.txid == txid));
            match old {
                None => Some("The send log has no record of the original send.".into()),
                Some(o) => {
                    let entry = LogEntry {
                        txid: new.clone(),
                        time: now_unix(),
                        fee,
                        feerate: fee * 1000 / q.vsize.max(1),
                        speed,
                        change: (o.change - (fee - q.old_fee)).max(0),
                        replaced_by: None,
                        replaces: Some(txid.to_string()),
                        ..o
                    };
                    l.replace(txid, entry).err()
                }
            }
        }
    };
    Ok(Bumped {
        txid: new,
        old_txid: txid.to_string(),
        fee,
        old_fee: q.old_fee,
        speed,
        label: speed.label(),
        sat_per_vb: (fee as f64 / q.vsize.max(1) as f64 * 1000.0).round() / 1000.0,
        log_error,
    })
}

// ---- The send log -----------------------------------------------------------------------------

/// One send from this app's Send tab. Amounts in sats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub txid: String,
    /// unix seconds
    pub time: u64,
    pub address: String,
    /// What the address got.
    pub amount: i64,
    pub fee: i64,
    /// sats per 1,000 vbytes
    #[serde(default)]
    pub feerate: Rate,
    pub speed: Speed,
    pub max: bool,
    /// The change it left; 0: none, so it can't be sped up.
    #[serde(default)]
    pub change: i64,
    /// The faster send that replaced this one.
    #[serde(default)]
    pub replaced_by: Option<String>,
    /// The send this one replaced.
    #[serde(default)]
    pub replaces: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LogFile {
    version: u32,
    sends: Vec<LogEntry>,
}

/// `<app data>/sends.json`.
pub struct SendLog {
    dir: PathBuf,
}

/// One change to the log at a time (it is read, changed and written whole).
static LOG_LOCK: Mutex<()> = Mutex::new(());

impl SendLog {
    pub const FILE: &'static str = "sends.json";

    pub fn new(app_dir: &Path) -> Self {
        Self { dir: app_dir.to_path_buf() }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(Self::FILE)
    }

    /// Every send on file, oldest first. No file yet: none.
    pub fn read(&self) -> Result<Vec<LogEntry>, String> {
        match std::fs::read(self.path()) {
            Ok(b) => serde_json::from_slice::<LogFile>(&b)
                .map(|f| f.sends)
                .map_err(|e| format!("{} is damaged ({})", self.path().display(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("FreeBank couldn't read {}: {}", self.path().display(), e)),
        }
    }

    /// Write the whole log through a temp file, readable by this user only.
    fn write(&self, sends: Vec<LogEntry>) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("FreeBank couldn't make {}: {}", self.dir.display(), e))?;
        let body = serde_json::to_vec_pretty(&LogFile { version: 1, sends }).map_err(|e| e.to_string())?;
        let tmp = self.dir.join(format!(".{}.tmp", Self::FILE));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).map_err(|e| format!("FreeBank couldn't write {}: {}", tmp.display(), e))?;
        f.write_all(&body).and_then(|_| f.sync_all()).map_err(|e| format!("FreeBank couldn't write {}: {}", tmp.display(), e))?;
        std::fs::rename(&tmp, self.path()).map_err(|e| format!("FreeBank couldn't write {}: {}", self.path().display(), e))
    }

    /// Read, change and write the log. A damaged log is moved aside, never written over.
    fn change(&self, f: impl FnOnce(&mut Vec<LogEntry>)) -> Result<(), String> {
        let _one = LOG_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut sends = match self.read() {
            Ok(s) => s,
            Err(_) if self.path().exists() => {
                let aside = self.dir.join(format!("{}.damaged-{}", Self::FILE, now_unix()));
                std::fs::rename(self.path(), &aside)
                    .map_err(|e| format!("The send log is damaged and FreeBank couldn't move it aside: {}", e))?;
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        f(&mut sends);
        self.write(sends)
    }

    pub fn append(&self, e: LogEntry) -> Result<(), String> {
        self.change(|s| s.push(e))
    }

    /// `new` replaced `old` (a Speed up).
    pub fn replace(&self, old: &str, new: LogEntry) -> Result<(), String> {
        self.change(|s| {
            for e in s.iter_mut().filter(|e| e.txid == old) {
                e.replaced_by = Some(new.txid.clone());
            }
            s.push(new);
        })
    }
}

// ---- History and the CSV ----------------------------------------------------------------------

/// History's page size.
pub const PER_PAGE: usize = 25;

/// One line of listtransactions, with what the send log knows about it. Amounts in sats, except
/// `amount`, the node's ECX number, which TransactionItem.svelte shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryItem {
    pub txid: String,
    /// unix seconds
    pub time: i64,
    /// send, receive, generate, immature or orphan
    pub category: String,
    pub amount: f64,
    /// The same, exactly; negative for a send.
    pub sats: i64,
    /// A send's fee.
    pub fee: Option<i64>,
    pub confirmations: i64,
    pub address: Option<String>,
    /// "yes", "no" or "unknown"
    pub replaceable: Option<String>,
    pub replaced_by: Option<String>,
    pub replaces: Option<String>,
    pub abandoned: bool,
    pub speed: Option<Speed>,
    pub max: Option<bool>,
    /// A send from this app's Send tab.
    pub logged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryPage {
    /// Newest first.
    pub items: Vec<HistoryItem>,
    pub page: u32,
    pub per_page: usize,
    /// There are older ones.
    pub more: bool,
}

fn by_txid(log: &[LogEntry]) -> HashMap<&str, &LogEntry> {
    log.iter().map(|e| (e.txid.as_str(), e)).collect()
}

fn history_item(e: &Value, log: &HashMap<&str, &LogEntry>) -> Option<HistoryItem> {
    let txid = e["txid"].as_str()?.to_string(); // an account "move" has none
    let l = log.get(txid.as_str()).copied();
    let text = |k: &str| e[k].as_str().map(String::from);
    Some(HistoryItem {
        time: e["time"].as_i64().unwrap_or(0),
        category: text("category").unwrap_or_else(|| "unknown".into()),
        amount: e["amount"].as_f64().unwrap_or(0.0),
        sats: sats(&e["amount"]).unwrap_or(0),
        fee: sats(&e["fee"]).map(|f| -f), // the node writes a send's fee as a negative amount
        confirmations: e["confirmations"].as_i64().unwrap_or(0),
        address: text("address"),
        replaceable: text("bip125-replaceable"),
        replaced_by: l.and_then(|l| l.replaced_by.clone()).or_else(|| text("replaced_by_txid")),
        replaces: l.and_then(|l| l.replaces.clone()).or_else(|| text("replaces_txid")),
        abandoned: e["abandoned"].as_bool().unwrap_or(false),
        speed: l.map(|l| l.speed),
        max: l.map(|l| l.max),
        logged: l.is_some(),
        txid,
    })
}

/// Page `page` of the wallet's transactions (0 = the newest), newest first.
pub async fn history_page(c: &mut FreeBankClient, log: &[LogEntry], page: u32) -> Result<HistoryPage, String> {
    let page = page.min(1_000_000);
    let skip = page as usize * PER_PAGE;
    // One more than a page: if it comes, there are older ones. listtransactions answers oldest first.
    let v = c.call_ui("listtransactions", vec![json!("*"), json!(PER_PAGE + 1), json!(skip), json!(false)]).await?;
    let mut rows = v.as_array().cloned().unwrap_or_default();
    let more = rows.len() > PER_PAGE;
    if more {
        rows.drain(..rows.len() - PER_PAGE);
    }
    let log = by_txid(log);
    let items = rows.iter().rev().filter_map(|e| history_item(e, &log)).collect();
    Ok(HistoryPage { items, page, per_page: PER_PAGE, more })
}

/// Every transaction, newest first.
pub async fn all_history(c: &mut FreeBankClient, log: &[LogEntry]) -> Result<Vec<HistoryItem>, String> {
    const BATCH: usize = 1000;
    let log = by_txid(log);
    let mut out = Vec::new();
    for k in 0..1000 {
        let v = c.call_ui("listtransactions", vec![json!("*"), json!(BATCH), json!(k * BATCH), json!(false)]).await?;
        let rows = v.as_array().cloned().unwrap_or_default();
        out.extend(rows.iter().rev().filter_map(|e| history_item(e, &log)));
        if rows.len() < BATCH {
            break;
        }
    }
    Ok(out)
}

/// Unix seconds as "2026-09-29T14:03:05Z".
pub fn iso_utc(unix: i64) -> String {
    let (days, secs) = (unix.div_euclid(86_400), unix.rem_euclid(86_400));
    // civil_from_days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, secs / 3600, secs % 3600 / 60, secs % 60)
}

/// A CSV field: quoted if it holds a comma, a quote or a line break. Text a spreadsheet would read
/// as a formula gets a leading apostrophe (amounts are numbers, and never pass through here).
fn csv_text(f: &str) -> String {
    let f = if f.starts_with(['=', '+', '-', '@']) { format!("'{}", f) } else { f.to_string() };
    if f.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", f.replace('"', "\"\""))
    } else {
        f
    }
}

/// The CSV for History's export: a header, then one line per transaction.
pub fn csv(items: &[HistoryItem]) -> String {
    let mut s = String::from("txid,time,category,amount,fee,confirmations,address,speed,replaced_by\n");
    for i in items {
        let fields = [
            csv_text(&i.txid),
            iso_utc(i.time),
            csv_text(&i.category),
            ecx(i.sats),
            i.fee.map(ecx).unwrap_or_default(),
            i.confirmations.to_string(),
            csv_text(i.address.as_deref().unwrap_or("")),
            csv_text(i.speed.map_or("", |s| s.label())),
            csv_text(i.replaced_by.as_deref().unwrap_or("")),
        ];
        s.push_str(&fields.join(","));
        s.push('\n');
    }
    s
}

/// Save `text` as freebank-history-<day>.csv in `folder` (then -2, -3, …: never over another file),
/// readable by this user only.
pub fn save_csv(folder: &Path, text: &str, day: &str) -> Result<PathBuf, String> {
    for n in 1..1000 {
        let name = if n == 1 { format!("freebank-history-{}.csv", day) } else { format!("freebank-history-{}-{}.csv", day, n) };
        let path = folder.join(name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&path) {
            Ok(mut f) => {
                f.write_all(text.as_bytes())
                    .and_then(|_| f.sync_all())
                    .map_err(|e| format!("FreeBank couldn't write {}: {}", path.display(), e))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("FreeBank couldn't write {}: {}", path.display(), e)),
        }
    }
    Err(format!("{} has too many history files for today already.", folder.display()))
}

#[derive(Debug, Clone, Serialize)]
pub struct CsvSaved {
    pub path: String,
    pub rows: usize,
}

// ---- Commands -----------------------------------------------------------------------------------

/// The send log, unless "Obliterate" has removed the app's folder: then nothing is written.
fn log_of(mgr: &NodeManager) -> Option<SendLog> {
    (!mgr.obliterated.load(Ordering::SeqCst)).then(|| SendLog::new(&mgr.app_dir))
}

fn entries(mgr: &NodeManager) -> Vec<LogEntry> {
    log_of(mgr).and_then(|l| l.read().ok()).unwrap_or_default()
}

/// The three speeds, from the last minute's answer unless `refresh`.
#[tauri::command]
pub async fn fee_choices(client: State<'_, ClientState>, refresh: Option<bool>) -> Result<FeeChoices, String> {
    let mut c = client.lock().await;
    BOOK.fees(&mut c, refresh.unwrap_or(false)).await
}

/// Build and fund a send; nothing is signed or sent. `amount` in sats, unless `max`.
#[tauri::command]
pub async fn send_prepare(
    client: State<'_, ClientState>,
    address: String,
    amount: Option<i64>,
    max: Option<bool>,
    speed: Speed,
) -> Result<Prepared, String> {
    let mut c = client.lock().await;
    prepare(&BOOK, &mut c, SendRequest { address, amount, max: max.unwrap_or(false), speed }).await
}

/// Sign and send what `send_prepare` built. Wrap it in withUnlock.
#[tauri::command]
pub async fn send_confirm(client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>, id: String) -> Result<Sent, String> {
    let log = log_of(&mgr);
    let mut c = client.lock().await;
    let r = confirm(&BOOK, &mut c, &id, log.as_ref()).await;
    // Only a send that failed: one that went through could be picked out on the explorer by its time.
    if let Err(e) = &r {
        crate::activity::note(&format!("send: not sent: {}", crate::activity::mask_numbers(e)));
    }
    r
}

/// This app's sends, newest first.
#[tauri::command]
pub async fn send_log(mgr: State<'_, Arc<NodeManager>>) -> Result<Vec<LogEntry>, String> {
    let Some(log) = log_of(&mgr) else { return Ok(Vec::new()) };
    let mut s = log.read()?;
    s.reverse();
    Ok(s)
}

/// What Speed up would cost for `txid`, per speed.
#[tauri::command]
pub async fn speed_up_quote(client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>, txid: String) -> Result<BumpQuote, String> {
    let log = entries(&mgr);
    let mut c = client.lock().await;
    quote_speed_up(&BOOK, &mut c, &log, &txid).await
}

/// Speed up `txid` at `speed`, for the fee its quote showed. Wrap it in withUnlock.
#[tauri::command]
pub async fn send_speed_up(
    client: State<'_, ClientState>,
    mgr: State<'_, Arc<NodeManager>>,
    txid: String,
    speed: Speed,
) -> Result<Bumped, String> {
    let log = log_of(&mgr);
    let mut c = client.lock().await;
    speed_up(&BOOK, &mut c, log.as_ref(), &txid, speed).await
}

/// A page of History (0 = the newest), newest first.
#[tauri::command]
pub async fn history(client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>, page: Option<u32>) -> Result<HistoryPage, String> {
    let log = entries(&mgr);
    let mut c = client.lock().await;
    history_page(&mut c, &log, page.unwrap_or(0)).await
}

/// All of History as a CSV file in Documents (or the home folder if there is no Documents folder).
/// A developer's run with FREEBANK_APP_DIR saves it in that folder instead, so a test run never
/// writes into the real Documents.
#[tauri::command]
pub async fn history_csv(app: AppHandle, client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>) -> Result<CsvSaved, String> {
    let log = entries(&mgr);
    let items = {
        let mut c = client.lock().await;
        all_history(&mut c, &log).await?
    };
    let path = app.path();
    let folder = match std::env::var_os("FREEBANK_APP_DIR") {
        Some(_) => {
            std::fs::create_dir_all(&mgr.app_dir).map_err(|e| format!("FreeBank couldn't make {}: {}", mgr.app_dir.display(), e))?;
            mgr.app_dir.clone()
        }
        None => path
            .document_dir()
            .ok()
            .filter(|d| d.is_dir())
            .or_else(|| path.home_dir().ok())
            .ok_or("FreeBank couldn't find your Documents folder or your home folder.")?,
    };
    let day = iso_utc(now_unix() as i64);
    let file = save_csv(&folder, &csv(&items), &day[..10])?;
    Ok(CsvSaved { path: file.to_string_lossy().into_owned(), rows: items.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::stub;

    const TO: &str = "XkPvjfFq8Hj9wy9Wq3pRr1sBdXJxZ5nT2m";
    const P2PKH: &str = "76a914000000000000000000000000000000000000000088ac";

    /// A small stand-in for freebankd's wallet. Transactions travel as hex(JSON), so that
    /// fundrawtransaction and decoderawtransaction can read what createrawtransaction made. Sizes
    /// follow P2PKH: 10 bytes, 34 per output, 41 per input unsigned, 147 signed (71-byte
    /// signatures), and 148 as Core's fee maths reckons it (72-byte signatures).
    #[derive(Default)]
    struct Node {
        coins: Vec<Value>,
        mempool_vbytes: i64,
        mempoolminfee: f64,
        estimates: HashMap<u64, f64>,
        /// An encrypted wallet that is locked.
        locked: bool,
        /// Signing refuses a locked wallet with -13 (Core 0.17 and later). freebankd (Core 0.16)
        /// doesn't: it answers an incomplete signature.
        sign_checks_lock: bool,
        /// Wallet transactions by txid: {fee (sats), hex, confirmations, replaceable, replaced_by}.
        txs: HashMap<String, Value>,
        /// listtransactions, oldest first.
        list: Vec<Value>,
        n: u64,
    }

    type Shared = Arc<Mutex<Node>>;

    fn enc(tx: &Value) -> String {
        hex::encode(tx.to_string())
    }
    fn dec(h: &str) -> Value {
        let h = h.strip_prefix("5349474e4544").unwrap_or(h); // "SIGNED"
        serde_json::from_slice(&hex::decode(h).unwrap()).unwrap()
    }
    fn size(ins: usize, outs: usize, per_in: i64) -> i64 {
        10 + per_in * ins as i64 + 34 * outs as i64
    }
    fn coin(txid: char, sats: i64, spendable: bool) -> Value {
        json!({"txid": txid.to_string().repeat(64), "vout": 0, "amount": sats as f64 / 1e8, "scriptPubKey": P2PKH,
               "spendable": spendable, "solvable": true, "safe": true, "confirmations": 3})
    }

    impl Node {
        fn answer(&mut self, m: &str, p: &Value) -> Result<Value, (i64, String)> {
            let unlock = || Err((-13, "Error: Please enter the wallet passphrase with walletpassphrase first.".to_string()));
            match m {
                "validateaddress" => {
                    let a = p[0].as_str().unwrap_or("");
                    let ok = a.starts_with('X') && a.len() >= 26;
                    Ok(if ok { json!({"isvalid": true, "address": a, "ismine": false}) } else { json!({"isvalid": false}) })
                }
                "getmempoolinfo" => Ok(json!({"size": 1, "bytes": self.mempool_vbytes, "usage": 1, "maxmempool": 300000000,
                    "mempoolminfee": self.mempoolminfee.max(0.00001), "minrelaytxfee": 0.00001})),
                "estimatesmartfee" => Ok(match self.estimates.get(&p[0].as_u64().unwrap()) {
                    Some(r) => json!({"feerate": r, "blocks": p[0]}),
                    None => json!({"errors": ["Insufficient data or no feerate found"], "blocks": 0}),
                }),
                "listunspent" => {
                    assert_eq!(p, &json!([0, 9_999_999, [], false]), "safe coins only");
                    Ok(json!(self.coins))
                }
                "createrawtransaction" => {
                    let vin: Vec<Value> = p[0].as_array().unwrap().clone();
                    let vout: Vec<Value> = p[1]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(a, v)| {
                            assert!(v.is_string(), "amounts go to the node as decimal strings, not numbers: {}", v);
                            json!({"address": a, "sats": sats(v).unwrap()})
                        })
                        .collect();
                    Ok(json!(enc(&json!({"vin": vin, "vout": vout, "rbf": p[3] == json!(true)}))))
                }
                "fundrawtransaction" => {
                    let mut tx = dec(p[0].as_str().unwrap());
                    let o = &p[1];
                    assert!(o["feeRate"].is_string(), "the fee rate goes as a decimal string");
                    assert_eq!(o["replaceable"], json!(true));
                    let rate = sats(&o["feeRate"]).unwrap();
                    let subtract = o["subtractFeeFromOutputs"] == json!([0]);
                    let out: i64 = tx["vout"].as_array().unwrap().iter().map(|v| v["sats"].as_i64().unwrap()).sum();
                    let mut vin = tx["vin"].as_array().unwrap().clone();
                    let value = |i: &Value| {
                        self.coins.iter().find(|c| c["txid"] == i["txid"] && c["vout"] == i["vout"]).and_then(|c| sats(&c["amount"])).unwrap()
                    };
                    let mut have: i64 = vin.iter().map(value).sum();
                    let outs = tx["vout"].as_array().unwrap().len();
                    if vin.is_empty() {
                        // largest first, until the amount and the fee (with change) are covered
                        let mut spendable: Vec<&Value> = self.coins.iter().filter(|c| c["spendable"] == json!(true)).collect();
                        spendable.sort_by_key(|c| -sats(&c["amount"]).unwrap());
                        for c in spendable {
                            if have >= out + fee_at(rate, size(vin.len(), outs + 1, 148)) {
                                break;
                            }
                            vin.push(json!({"txid": c["txid"], "vout": c["vout"]}));
                            have += sats(&c["amount"]).unwrap();
                        }
                    }
                    let mut fee = fee_at(rate, size(vin.len(), outs, 148));
                    let mut changepos = -1;
                    if subtract {
                        let v = tx["vout"][0]["sats"].as_i64().unwrap() - fee;
                        if v <= 546 {
                            return Err((-4, "The transaction amount is too small to pay the fee".into()));
                        }
                        tx["vout"][0]["sats"] = json!(v);
                    } else {
                        if have < out + fee {
                            return Err((-4, "Insufficient funds".into()));
                        }
                        let with_change = fee_at(rate, size(vin.len(), outs + 1, 148));
                        let change = have - out - with_change;
                        if change > 546 {
                            fee = with_change;
                            tx["vout"].as_array_mut().unwrap().push(json!({"address": "Xchange111111111111111111111111", "sats": change}));
                            changepos = outs as i64;
                        } else {
                            fee = have - out; // dust change goes to the fee
                        }
                    }
                    tx["vin"] = json!(vin);
                    Ok(json!({"hex": enc(&tx), "fee": fee as f64 / 1e8, "changepos": changepos}))
                }
                "decoderawtransaction" => {
                    let h = p[0].as_str().unwrap();
                    let signed = h.starts_with("5349474e4544");
                    let tx = dec(h);
                    let vin: Vec<Value> = tx["vin"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|i| {
                            let sig = if signed { format!("47{}21{}", "30".repeat(71), "02".repeat(33)) } else { String::new() };
                            json!({"txid": i["txid"], "vout": i["vout"], "scriptSig": {"hex": sig}, "sequence": 4294967293u32})
                        })
                        .collect();
                    let vout: Vec<Value> = tx["vout"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .enumerate()
                        .map(|(n, o)| json!({"value": o["sats"].as_i64().unwrap() as f64 / 1e8, "n": n,
                                             "scriptPubKey": {"type": "pubkeyhash", "addresses": [o["address"]]}}))
                        .collect();
                    let s = size(vin.len(), vout.len(), if signed { 147 } else { 41 });
                    Ok(json!({"txid": "t", "size": s, "vsize": s, "weight": 4 * s, "vin": vin, "vout": vout}))
                }
                "getwalletinfo" => Ok(json!({"walletname": "", "unlocked_until": if self.locked { 0 } else { 1_900_000_000 }})),
                "signrawtransactionwithwallet" => {
                    let h = p[0].as_str().unwrap();
                    if self.locked && self.sign_checks_lock {
                        return unlock();
                    }
                    if self.locked {
                        let e = "Unable to sign input, invalid stack size (possibly missing key)";
                        return Ok(json!({"hex": h, "complete": false, "errors": [{"txid": "a", "vout": 0, "error": e}]}));
                    }
                    let tx = dec(h);
                    let known = tx["vin"].as_array().unwrap().iter().all(|i| self.coins.iter().any(|c| c["txid"] == i["txid"]));
                    if !known {
                        return Ok(json!({"hex": h, "complete": false, "errors": [{"error": "Input not found or already spent"}]}));
                    }
                    Ok(json!({"hex": format!("5349474e4544{}", h), "complete": true}))
                }
                "sendrawtransaction" => {
                    let h = p[0].as_str().unwrap();
                    let tx = dec(h);
                    self.n += 1;
                    let txid = format!("{:064x}", self.n);
                    let ins: i64 = tx["vin"].as_array().unwrap().iter()
                        .map(|i| self.coins.iter().find(|c| c["txid"] == i["txid"] && c["vout"] == i["vout"]).and_then(|c| sats(&c["amount"])).unwrap())
                        .sum();
                    let outs: i64 = tx["vout"].as_array().unwrap().iter().map(|o| o["sats"].as_i64().unwrap()).sum();
                    let replaceable = if tx["rbf"] == json!(true) || tx["vin"].as_array().unwrap().len() > 0 { "yes" } else { "no" };
                    self.txs.insert(txid.clone(), json!({"fee": ins - outs, "hex": h, "confirmations": 0, "replaceable": replaceable}));
                    Ok(json!(txid))
                }
                "gettransaction" => {
                    let id = p[0].as_str().unwrap();
                    let t = self.txs.get(id).ok_or((-5, "Invalid or non-wallet transaction id".to_string()))?;
                    let mut r = json!({"txid": id, "fee": -(t["fee"].as_i64().unwrap() as f64) / 1e8, "confirmations": t["confirmations"],
                                       "bip125-replaceable": t["replaceable"], "hex": t["hex"], "time": 1_759_140_000});
                    if let Some(by) = t.get("replaced_by") {
                        r["replaced_by_txid"] = by.clone();
                    }
                    Ok(r)
                }
                "getnetworkinfo" => Ok(json!({"version": 160000, "relayfee": 0.00001, "incrementalfee": 0.00001})),
                "bumpfee" => {
                    if self.locked {
                        return unlock();
                    }
                    let id = p[0].as_str().unwrap().to_string();
                    let total = p[1]["totalFee"].as_i64().expect("totalFee in sats");
                    let t = self.txs.get(&id).ok_or((-5, "Invalid or non-wallet transaction id".to_string()))?.clone();
                    if t.get("replaced_by").is_some() {
                        return Err((-4, format!("Cannot bump transaction {} which was already bumped by transaction x", id)));
                    }
                    let tx = dec(t["hex"].as_str().unwrap());
                    let (ins, outs) = (tx["vin"].as_array().unwrap().len(), tx["vout"].as_array().unwrap().len());
                    let change = tx["vout"].as_array().unwrap().iter().position(|o| o["address"] == "Xchange111111111111111111111111");
                    let Some(ci) = change else { return Err((-4, "Transaction does not have a change output".into())) };
                    // feebumper.cpp: the old rate over the largest size, plus the incremental relay fee
                    let old = t["fee"].as_i64().unwrap();
                    let (vsize, max) = (size(ins, outs, 147), size(ins, outs, 148));
                    let least = fee_at(old * 1000 / vsize, max) + fee_at(1000, max);
                    if total < least {
                        return Err((-8, format!("Insufficient totalFee, must be at least {}", ecx(least))));
                    }
                    let mut tx = tx.clone();
                    let left = tx["vout"][ci]["sats"].as_i64().unwrap() - (total - old);
                    let mut fee = total;
                    if left <= 546 {
                        fee += left;
                        tx["vout"].as_array_mut().unwrap().remove(ci);
                    } else {
                        tx["vout"][ci]["sats"] = json!(left);
                    }
                    self.n += 1;
                    let new = format!("{:064x}", self.n);
                    self.txs.get_mut(&id).unwrap()["replaced_by"] = json!(new);
                    let h = format!("5349474e4544{}", enc(&tx));
                    self.txs.insert(new.clone(), json!({"fee": fee, "hex": h, "confirmations": 0, "replaceable": "yes"}));
                    Ok(json!({"txid": new, "origfee": old as f64 / 1e8, "fee": fee as f64 / 1e8, "errors": []}))
                }
                "listtransactions" => {
                    let (count, skip) = (p[1].as_u64().unwrap() as usize, p[2].as_u64().unwrap() as usize);
                    let end = self.list.len().saturating_sub(skip);
                    Ok(json!(self.list[end.saturating_sub(count)..end]))
                }
                _ => Err((-32601, "Method not found".into())),
            }
        }
    }

    fn node(n: Node) -> (FreeBankClient, stub::Calls, Shared) {
        let shared: Shared = Arc::new(Mutex::new(n));
        let s = shared.clone();
        let (c, calls) = stub::serve(move |m, p| s.lock().unwrap().answer(m, p));
        (c, calls, shared)
    }

    fn funded() -> Node {
        // A watch-only coin, and a zero-value block reward (freebankd's regtest wallet lists those).
        let coins = vec![coin('a', 150_000_000, true), coin('b', 20_000_000, true), coin('w', 900_000_000, false), coin('z', 0, true)];
        Node { coins, ..Default::default() }
    }

    fn methods(calls: &stub::Calls) -> Vec<String> {
        calls.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    fn params(calls: &stub::Calls, method: &str) -> Vec<Value> {
        calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }

    fn req(amount: Option<i64>, max: bool, speed: Speed) -> SendRequest {
        SendRequest { address: format!("  {}  ", TO), amount, max, speed }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fb-send-test-{}-{}-{}", name, std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // ---- amounts ----

    #[test]
    fn amounts_go_out_as_exact_decimals_and_come_back_as_sats() {
        assert_eq!(ecx(150_000_000), "1.50000000");
        assert_eq!(ecx(1), "0.00000001");
        assert_eq!(ecx(0), "0.00000000");
        assert_eq!(ecx(-226), "-0.00000226");
        assert_eq!(ecx(MAX_SATS), "21000000.00000000");
        assert_eq!(sats(&json!(0.1)), Some(10_000_000));
        assert_eq!(sats(&json!(0.00000226)), Some(226));
        assert_eq!(sats(&json!(-0.25)), Some(-25_000_000));
        assert_eq!(sats(&json!(20_999_999.99999999)), Some(MAX_SATS - 1));
        assert_eq!(sats(&json!("1.23456789")), Some(123_456_789));
        assert_eq!(sats(&json!(null)), None);
        assert_eq!(sats(&json!(1e300)), None);
    }

    // ---- fee choices ----

    #[tokio::test]
    async fn a_mempool_under_one_block_gives_the_minimum_for_every_speed() {
        let (mut c, calls, _) = node(Node { mempool_vbytes: 40_000, ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!(f.choices.iter().map(|c| c.sat_per_kvb).collect::<Vec<_>>(), [1000, 1000, 1000]);
        assert!(f.same);
        assert_eq!(f.basis, "quiet");
        assert_eq!(f.choices[0].sat_per_vb, 1.0);
        assert_eq!(methods(&calls), ["getmempoolinfo"], "no estimates asked for a quiet mempool");
        // Exactly one block's worth still fits.
        let (mut c, calls, _) = node(Node { mempool_vbytes: BLOCK_VBYTES, estimates: HashMap::from([(1, 0.001)]), ..Default::default() });
        assert!(work_out_fees(&mut c).await.unwrap().same);
        assert_eq!(methods(&calls).len(), 1);
    }

    #[tokio::test]
    async fn a_mempool_over_a_block_asks_the_estimates() {
        let est = HashMap::from([(1, 0.0002), (6, 0.00005), (144, 0.00001)]);
        let (mut c, calls, _) = node(Node { mempool_vbytes: 2_000_000, estimates: est, ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!(f.choices.iter().map(|c| (c.speed, c.sat_per_kvb)).collect::<Vec<_>>(), [(Speed::Next, 20_000), (Speed::Hour, 5_000), (Speed::Cheap, 1_000)]);
        assert_eq!(f.choices.iter().map(|c| c.label).collect::<Vec<_>>(), ["Next block", "Within an hour", "Cheapest"]);
        assert!(!f.same);
        assert_eq!(f.basis, "estimates");
        assert_eq!(params(&calls, "estimatesmartfee"), [json!([1]), json!([6]), json!([144])]);
    }

    #[tokio::test]
    async fn missing_estimates_fall_back_to_the_floor() {
        let (mut c, _, _) = node(Node { mempool_vbytes: 2_000_000, ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!((f.same, f.basis, f.rate(Speed::Next)), (true, "none", 1000));

        let (mut c, _, _) = node(Node { mempool_vbytes: 2_000_000, estimates: HashMap::from([(6, 0.00003)]), ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!(f.basis, "partial");
        assert_eq!([f.rate(Speed::Next), f.rate(Speed::Hour), f.rate(Speed::Cheap)], [3000, 3000, 1000], "a faster speed never costs less");
    }

    #[tokio::test]
    async fn every_choice_is_floored_at_1_sat_per_vbyte_and_the_mempool_minimum() {
        // 0.1 sat/vB estimates (the 2026-09-27 flood paid about that) come up to 1 sat/vB.
        let est = HashMap::from([(1, 0.000001), (6, 0.000001), (144, 0.000001)]);
        let (mut c, _, _) = node(Node { mempool_vbytes: 2_000_000, estimates: est, ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!((f.rate(Speed::Next), f.same, f.floor_sat_per_kvb), (1000, true, 1000));
        // A full mempool raises its minimum: nothing below it would get in.
        let (mut c, _, _) = node(Node { mempool_vbytes: 1_000, mempoolminfee: 0.000025, ..Default::default() });
        let f = work_out_fees(&mut c).await.unwrap();
        assert_eq!((f.rate(Speed::Cheap), f.floor_sat_per_kvb, f.choices[2].sat_per_vb), (2500, 2500, 2.5));
    }

    #[tokio::test]
    async fn the_choices_are_kept_for_a_minute() {
        let (mut c, calls, _) = node(Node::default());
        let book = Book::default();
        book.fees(&mut c, false).await.unwrap();
        book.fees(&mut c, false).await.unwrap();
        assert_eq!(methods(&calls), ["getmempoolinfo"]);
        book.fees(&mut c, true).await.unwrap();
        assert_eq!(methods(&calls).len(), 2, "refresh asks again");
        let stale = Book::new(HOLD, Duration::ZERO);
        stale.fees(&mut c, false).await.unwrap();
        stale.fees(&mut c, false).await.unwrap();
        assert_eq!(methods(&calls).len(), 4, "older than the limit: asked again");
    }

    // ---- prepare and confirm ----

    #[tokio::test]
    async fn a_send_shows_the_fee_it_will_pay_and_pays_it() {
        let (mut c, calls, _) = node(funded());
        let book = Book::default();
        let dir = temp("send");
        let log = SendLog::new(&dir);
        let q = prepare(&book, &mut c, req(Some(25_000_000), false, Speed::Next)).await.unwrap();
        // One P2PKH coin in, the payment and change out, at 1 sat/vB over Core's signed size.
        assert_eq!(q.fee, fee_at(1000, size(1, 2, 148)));
        assert_eq!((q.amount, q.total, q.max, q.speed, q.label), (25_000_000, 25_000_000 + q.fee, false, Speed::Next, "Next block"));
        assert_eq!(q.change, 150_000_000 - 25_000_000 - q.fee);
        assert_eq!(q.vsize, size(1, 2, 148), "the signed size, as the fee was worked out");
        assert_eq!(q.address, TO, "the address as the node writes it (the typed spaces are gone)");
        assert_eq!(params(&calls, "createrawtransaction")[0][1], json!({TO: "0.25000000"}));
        let opts = &params(&calls, "fundrawtransaction")[0][1];
        assert_eq!(opts, &json!({"feeRate": "0.00001000", "replaceable": true}));
        assert!(!methods(&calls).iter().any(|m| m == "signrawtransactionwithwallet" || m == "sendrawtransaction"), "nothing signed or sent yet");

        let funded_hex = calls.lock().unwrap().iter().rev().find(|(m, _)| m == "decoderawtransaction").unwrap().1[0].clone();
        let sent = confirm(&book, &mut c, &q.id, Some(&log)).await.unwrap();
        assert_eq!(params(&calls, "signrawtransactionwithwallet")[0][0], funded_hex, "the transaction kept is the one signed");
        assert_eq!((sent.amount, sent.fee, sent.log_error.clone()), (q.amount, q.fee, None));
        let entries = log.read().unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!((e.txid.as_str(), e.address.as_str(), e.amount, e.fee, e.speed, e.max, e.change), (sent.txid.as_str(), TO, q.amount, q.fee, Speed::Next, false, q.change));
        assert_eq!(e.feerate, 1000);

        // Sent once: the id is used up.
        assert_eq!(confirm(&book, &mut c, &q.id, Some(&log)).await.unwrap_err(), EXPIRED);
        assert_eq!(params(&calls, "sendrawtransaction").len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn max_sends_every_spendable_coin_with_the_fee_taken_out_and_no_change() {
        let (mut c, calls, _) = node(funded());
        let book = Book::default();
        let q = prepare(&book, &mut c, req(None, true, Speed::Cheap)).await.unwrap();
        let sum = 150_000_000 + 20_000_000; // the watch-only coin is not ours to spend, the zero-value one is left out
        assert_eq!(q.fee, fee_at(1000, size(2, 1, 148)));
        assert_eq!((q.amount, q.total, q.change, q.max), (sum - q.fee, sum, 0, true));
        assert_eq!(q.vsize, size(2, 1, 148));
        let raw = &params(&calls, "createrawtransaction")[0];
        let ins: HashSet<String> = raw[0].as_array().unwrap().iter().map(|i| i["txid"].as_str().unwrap()[..1].to_string()).collect();
        assert_eq!(ins, HashSet::from(["a".to_string(), "b".to_string()]));
        assert_eq!((&raw[1], &raw[3]), (&json!({TO: "1.70000000"}), &json!(true)), "the exact sum, and replaceable inputs");
        assert_eq!(params(&calls, "fundrawtransaction")[0][1]["subtractFeeFromOutputs"], json!([0]));

        let sent = confirm(&book, &mut c, &q.id, None).await.unwrap();
        assert_eq!((sent.amount, sent.max, sent.change), (sum - q.fee, true, 0));
    }

    #[tokio::test]
    async fn max_with_nothing_spendable_says_so() {
        let (mut c, calls, _) = node(Node { coins: vec![coin('w', 5_000_000, false)], ..Default::default() });
        assert_eq!(prepare(&Book::default(), &mut c, req(None, true, Speed::Next)).await.unwrap_err(), NOTHING_TO_SEND);
        assert!(!methods(&calls).iter().any(|m| m == "createrawtransaction"));
    }

    #[tokio::test]
    async fn a_bad_address_or_amount_is_refused_before_anything_is_built() {
        let (mut c, calls, _) = node(funded());
        let book = Book::default();
        let bad = SendRequest { address: "1BoatSLRHtKNngkdXEeobR76b53LETtpyT".into(), ..req(Some(1000), false, Speed::Next) };
        assert_eq!(prepare(&book, &mut c, bad).await.unwrap_err(), NOT_AN_ADDRESS);
        assert_eq!(prepare(&book, &mut c, SendRequest { address: " ".into(), ..req(Some(1), false, Speed::Next) }).await.unwrap_err(), NO_ADDRESS);
        for amount in [None, Some(0), Some(-5), Some(MAX_SATS + 1)] {
            assert_eq!(prepare(&book, &mut c, req(amount, false, Speed::Next)).await.unwrap_err(), AMOUNT_PROBLEM);
        }
        assert_eq!(methods(&calls), ["validateaddress"]);
    }

    #[tokio::test]
    async fn too_much_says_so_and_points_at_max() {
        let (mut c, _, _) = node(funded());
        let e = prepare(&Book::default(), &mut c, req(Some(170_000_000), false, Speed::Next)).await.unwrap_err();
        assert_eq!(e, NOT_ENOUGH, "the full balance leaves nothing for the fee");
    }

    #[test]
    fn funding_errors_in_plain_words() {
        assert_eq!(funding_problem("RPC error -4: Insufficient funds".into()), NOT_ENOUGH);
        assert_eq!(
            funding_problem("RPC error -4: Keypool ran out, please call keypoolrefill first".into()),
            "RPC error -12: Keypool ran out, please call keypoolrefill first",
            "withUnlock unlocks for -12, which refills the keys"
        );
        assert_eq!(funding_problem("RPC error -4: The transaction amount is too small to pay the fee".into()), FEE_EATS_IT);
        assert_eq!(funding_problem("RPC error -4: Transaction amount too small".into()), TOO_SMALL);
        assert_eq!(funding_problem("RPC error -28: Loading wallet...".into()), "RPC error -28: Loading wallet...");
        assert!(broadcast_problem("RPC error -25: Missing inputs".into()).contains("Review it again"));
        assert_eq!(broadcast_problem("RPC error -26: 66: insufficient fee".into()), "The network refused this send: 66: insufficient fee");
    }

    #[tokio::test]
    async fn a_locked_wallet_keeps_the_send_for_the_retry() {
        // freebankd answers a locked wallet's signing with an incomplete signature, a newer node
        // with -13: either way the screen gets -13, and the prepared send waits for the retry.
        for checks in [false, true] {
            let (mut c, calls, n) = node(Node { locked: true, sign_checks_lock: checks, ..funded() });
            let book = Book::default();
            let q = prepare(&book, &mut c, req(Some(10_000_000), false, Speed::Next)).await.unwrap();
            let e = confirm(&book, &mut c, &q.id, None).await.unwrap_err();
            assert_eq!(e, LOCKED, "withUnlock needs the node's code");
            assert!(params(&calls, "sendrawtransaction").is_empty());
            n.lock().unwrap().locked = false; // what withUnlock's walletpassphrase does
            let sent = confirm(&book, &mut c, &q.id, None).await.unwrap();
            assert_eq!(sent.fee, q.fee);
            assert_eq!(params(&calls, "sendrawtransaction").len(), 1);
        }
    }

    #[tokio::test]
    async fn an_expired_or_unknown_send_is_refused() {
        let (mut c, calls, _) = node(funded());
        let book = Book::new(Duration::ZERO, FEES_FOR);
        let q = prepare(&book, &mut c, req(Some(10_000_000), false, Speed::Next)).await.unwrap();
        assert_eq!(confirm(&book, &mut c, &q.id, None).await.unwrap_err(), EXPIRED);
        assert_eq!(confirm(&Book::default(), &mut c, "nope", None).await.unwrap_err(), EXPIRED);
        assert!(!methods(&calls).iter().any(|m| m == "signrawtransactionwithwallet"), "nothing signed");
    }

    #[tokio::test]
    async fn a_send_the_wallet_cant_fully_sign_is_not_sent() {
        let (mut c, calls, n) = node(funded());
        let book = Book::default();
        let q = prepare(&book, &mut c, req(Some(10_000_000), false, Speed::Next)).await.unwrap();
        n.lock().unwrap().coins.clear(); // spent elsewhere in the meantime
        let e = confirm(&book, &mut c, &q.id, None).await.unwrap_err();
        assert!(e.contains("couldn't sign") && e.contains("Input not found"), "{}", e);
        assert!(params(&calls, "sendrawtransaction").is_empty());
    }

    #[test]
    fn the_signed_size_follows_the_kind_of_coin() {
        let coin = |script: &str, redeem: Option<&str>| Coin { txid: "a".into(), vout: 0, sats: 1, script: script.into(), redeem: redeem.map(String::from) };
        let unsigned = |n: usize| json!({"weight": 4 * size(n, 2, 41), "vin": (0..n).map(|i| json!({"txid": "a", "vout": i})).collect::<Vec<_>>()});
        let p2pkh = coin(P2PKH, None);
        let m: HashMap<Outpoint, &Coin> = HashMap::from([(("a".to_string(), 0), &p2pkh), (("a".to_string(), 1), &p2pkh)]);
        assert_eq!(signed_vsize(&unsigned(2), &m), size(2, 2, 148));
        // freebankd's default change is P2SH-P2WPKH: 23 bytes of script, then a 108-unit witness.
        let wrapped = coin(&format!("a914{}87", "00".repeat(20)), Some(&format!("0014{}", "00".repeat(20))));
        let m: HashMap<Outpoint, &Coin> = HashMap::from([(("a".to_string(), 0), &wrapped)]);
        assert_eq!(signed_vsize(&unsigned(1), &m), (4 * size(1, 2, 41) + 4 * 23 + 108 + 2 + 3) / 4);
        let native = coin(&format!("0014{}", "00".repeat(20)), None);
        let m: HashMap<Outpoint, &Coin> = HashMap::from([(("a".to_string(), 0), &native), (("a".to_string(), 1), &p2pkh)]);
        assert_eq!(signed_vsize(&unsigned(2), &m), (4 * size(2, 2, 41) + 108 + 4 * 107 + 2 + 1 + 3) / 4);
    }

    // ---- Speed up ----

    /// A send from the Send tab, logged, with change.
    async fn one_send(n: Node) -> (FreeBankClient, stub::Calls, Shared, Book, SendLog, PathBuf, Sent) {
        let (mut c, calls, shared) = node(n);
        let book = Book::default();
        let dir = temp("bump");
        let log = SendLog::new(&dir);
        let q = prepare(&book, &mut c, req(Some(25_000_000), false, Speed::Cheap)).await.unwrap();
        let sent = confirm(&book, &mut c, &q.id, Some(&log)).await.unwrap();
        (c, calls, shared, book, log, dir, sent)
    }

    #[test]
    fn the_largest_signed_size_counts_every_signature_at_72_bytes() {
        let vin = |sig: usize| json!({"txid": "a", "vout": 0, "scriptSig": {"hex": format!("{:02x}{}21{}", sig, "30".repeat(sig), "02".repeat(33))}});
        let tx = |ins: Vec<Value>, weight: i64| json!({"weight": weight, "vin": ins});
        assert_eq!(max_signed_vsize(&tx(vec![vin(71), vin(72)], 4 * 400)), 400 + 1 + 1, "one byte for the 71-byte one, one to spare");
        assert_eq!(max_signed_vsize(&tx(vec![vin(72)], 4 * 300)), 301);
        let witness = json!({"txid": "a", "vout": 0, "scriptSig": {"hex": format!("16{}", "00".repeat(22))}, "txinwitness": ["30".repeat(71), "02".repeat(33)]});
        assert_eq!(max_signed_vsize(&tx(vec![witness], 4 * 200 + 3)), (4 * 200 + 3 + 1 + 3) / 4 + 1);
    }

    #[tokio::test]
    async fn speed_up_pays_the_chosen_rate_over_the_largest_size_and_never_less_than_core_allows() {
        let (mut c, calls, n, book, log, dir, sent) = one_send(funded()).await;
        let entries = log.read().unwrap();
        let q = quote_speed_up(&book, &mut c, &entries, &sent.txid).await.unwrap();
        let (vsize, max) = (size(1, 2, 147), size(1, 2, 148) + 1);
        assert_eq!((q.old_fee, q.vsize), (sent.fee, vsize));
        // A quiet mempool: every speed is 1 sat/vB, which the old fee already pays, so Core's floor
        // rules: the old rate plus the 1 sat/vB increment, over the largest size.
        let least = fee_at(sent.fee * 1000 / vsize, max) + fee_at(1000, max);
        assert!(q.same);
        assert!(q.choices.iter().all(|c| c.fee == least && c.extra == least - sent.fee && c.ok));
        assert!(least > sent.fee);

        let b = speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Next).await.unwrap();
        assert_eq!(params(&calls, "bumpfee")[0], json!([sent.txid, {"totalFee": least}]));
        assert_eq!((b.old_txid.as_str(), b.fee, b.old_fee, b.speed, b.log_error.clone()), (sent.txid.as_str(), least, sent.fee, Speed::Next, None));
        assert_ne!(b.txid, sent.txid);
        // The log: the old one replaced, the new one after it.
        let entries = log.read().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].replaced_by.as_deref(), Some(b.txid.as_str()));
        let e = &entries[1];
        assert_eq!((e.txid.as_str(), e.replaces.as_deref(), e.fee, e.speed, e.amount, e.address.as_str()), (b.txid.as_str(), Some(sent.txid.as_str()), least, Speed::Next, 25_000_000, TO));
        assert_eq!(e.change, sent.change - (least - sent.fee));

        // The new one can be sped up again; the old one can't.
        n.lock().unwrap().mempool_vbytes = 2_000_000;
        n.lock().unwrap().estimates = HashMap::from([(1, 0.0002), (6, 0.00005), (144, 0.00001)]);
        book.fees(&mut c, true).await.unwrap();
        let q2 = quote_speed_up(&book, &mut c, &entries, &b.txid).await.unwrap();
        assert!(!q2.same);
        assert_eq!(q2.choices[0].fee, fee_for(20_000, max), "20 sat/vB over the largest size");
        assert!(q2.choices[2].fee >= fee_at(least * 1000 / vsize, max) + fee_at(1000, max));
        let e = quote_speed_up(&book, &mut c, &entries, &sent.txid).await.unwrap_err();
        assert!(e.starts_with("This send was sped up already"), "{}", e);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn speed_up_is_refused_for_max_sends_old_sends_and_settled_ones() {
        // Max: no change.
        let (mut c, _, _) = node(funded());
        let book = Book::default();
        let dir = temp("refuse");
        let log = SendLog::new(&dir);
        let q = prepare(&book, &mut c, req(None, true, Speed::Next)).await.unwrap();
        let max = confirm(&book, &mut c, &q.id, Some(&log)).await.unwrap();
        let entries = log.read().unwrap();
        assert_eq!(quote_speed_up(&book, &mut c, &entries, &max.txid).await.unwrap_err(), MAX_CANT);

        // Not from the Send tab (an older send, a phone send, a note transfer).
        assert_eq!(quote_speed_up(&book, &mut c, &entries, &"f".repeat(64)).await.unwrap_err(), NOT_FROM_SEND_TAB);

        // Logged, but the wallet says it isn't replaceable, or it has confirmed, or lost to another.
        let (mut c, _, n, book, log, dir2, sent) = one_send(funded()).await;
        let entries = log.read().unwrap();
        let set = |k: &str, v: Value| n.lock().unwrap().txs.get_mut(&sent.txid).unwrap()[k] = v;
        set("replaceable", json!("no"));
        assert_eq!(quote_speed_up(&book, &mut c, &entries, &sent.txid).await.unwrap_err(), NOT_REPLACEABLE);
        set("replaceable", json!("yes"));
        set("confirmations", json!(1));
        assert_eq!(quote_speed_up(&book, &mut c, &entries, &sent.txid).await.unwrap_err(), "This send has confirmed already.");
        set("confirmations", json!(-2));
        assert!(quote_speed_up(&book, &mut c, &entries, &sent.txid).await.unwrap_err().contains("confirmed instead"));

        // A send that left no change (it went to the fee as dust).
        let mut no_change = entries[0].clone();
        no_change.change = 0;
        assert_eq!(quote_speed_up(&book, &mut c, &[no_change], &sent.txid).await.unwrap_err(), NO_CHANGE);
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(dir2).unwrap();
    }

    #[tokio::test]
    async fn speed_up_needs_a_fresh_quote_and_keeps_it_through_an_unlock() {
        let (mut c, calls, n, book, log, dir, sent) = one_send(funded()).await;
        // No quote yet.
        assert_eq!(speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Next).await.unwrap_err(), QUOTE_EXPIRED);
        // An expired one.
        let brief = Book::new(Duration::ZERO, FEES_FOR);
        quote_speed_up(&brief, &mut c, &log.read().unwrap(), &sent.txid).await.unwrap();
        assert_eq!(speed_up(&brief, &mut c, Some(&log), &sent.txid, Speed::Next).await.unwrap_err(), QUOTE_EXPIRED);
        assert!(params(&calls, "bumpfee").is_empty());
        // A locked wallet: -13 for withUnlock, and the quote is still there for the retry.
        let q = quote_speed_up(&book, &mut c, &log.read().unwrap(), &sent.txid).await.unwrap();
        n.lock().unwrap().locked = true;
        let e = speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Hour).await.unwrap_err();
        assert!(e.starts_with("RPC error -13: "), "{}", e);
        n.lock().unwrap().locked = false;
        let b = speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Hour).await.unwrap();
        assert_eq!(b.fee, q.choices[1].fee);
        // Used once: a second one needs a new quote.
        assert_eq!(speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Hour).await.unwrap_err(), QUOTE_EXPIRED);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bumpfee_errors_in_plain_words() {
        assert_eq!(bump_problem("RPC error -13: Error: Please enter the wallet passphrase".into()), "RPC error -13: Error: Please enter the wallet passphrase");
        assert_eq!(bump_problem("RPC error -4: Transaction does not have a change output".into()), NO_CHANGE);
        assert_eq!(bump_problem("RPC error -4: Transaction is not BIP 125 replaceable".into()), NOT_REPLACEABLE);
        assert!(bump_problem("RPC error -8: Transaction has descendants in the wallet".into()).contains("later transaction"));
    }

    // ---- the log ----

    #[test]
    fn the_log_is_private_written_whole_and_never_lost() {
        let dir = temp("log");
        let log = SendLog::new(&dir.join("app")); // the app's folder isn't there yet: it is made
        assert_eq!(log.read().unwrap(), vec![]);
        let e = |t: &str| LogEntry {
            txid: t.into(), time: 1, address: TO.into(), amount: 5, fee: 1, feerate: 1000, speed: Speed::Hour, max: false, change: 9,
            replaced_by: None, replaces: None,
        };
        log.append(e("aa")).unwrap();
        log.append(e("bb")).unwrap();
        assert_eq!(log.read().unwrap().iter().map(|e| e.txid.as_str()).collect::<Vec<_>>(), ["aa", "bb"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let names: Vec<String> = std::fs::read_dir(dir.join("app")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["sends.json"], "no temp file left behind");
        let body: Value = serde_json::from_slice(&std::fs::read(log.path()).unwrap()).unwrap();
        assert_eq!(body["version"], 1);
        assert_eq!(body["sends"][0]["speed"], "hour");

        // A damaged log is moved aside, not written over.
        std::fs::write(log.path(), b"{not json").unwrap();
        assert!(log.read().is_err());
        log.append(e("cc")).unwrap();
        assert_eq!(log.read().unwrap().len(), 1);
        let aside: Vec<_> = std::fs::read_dir(dir.join("app")).unwrap().filter_map(|e| {
            let n = e.unwrap().file_name().to_string_lossy().into_owned();
            n.starts_with("sends.json.damaged-").then_some(n)
        }).collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(dir.join("app").join(&aside[0])).unwrap(), b"{not json");
        // An older log's entries (no feerate, change or replacements) still read.
        std::fs::write(log.path(), br#"{"version":1,"sends":[{"txid":"dd","time":2,"address":"X","amount":1,"fee":1,"speed":"next","max":true}]}"#).unwrap();
        assert_eq!(log.read().unwrap()[0].change, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    // ---- History and the CSV ----

    fn listed(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| json!({"txid": format!("{:064x}", i + 1), "time": 1_790_536_000 + i as i64, "category": if i % 2 == 0 { "receive" } else { "send" },
                            "amount": if i % 2 == 0 { 1.5 } else { -0.25 }, "fee": if i % 2 == 0 { Value::Null } else { json!(-0.00000226) },
                            "confirmations": n - i, "address": TO, "bip125-replaceable": "no"}))
            .collect()
    }

    #[tokio::test]
    async fn history_pages_newest_first() {
        let mut list = listed(60);
        list.insert(30, json!({"account": "", "category": "move", "amount": 1.0, "time": 1})); // no txid: left out
        let (mut c, calls, _) = node(Node { list, ..Default::default() });
        let p0 = history_page(&mut c, &[], 0).await.unwrap();
        assert_eq!((p0.items.len(), p0.more, p0.page, p0.per_page), (25, true, 0, 25));
        assert_eq!(p0.items[0].txid, format!("{:064x}", 60), "newest first");
        assert_eq!(p0.items[24].txid, format!("{:064x}", 36));
        assert_eq!(params(&calls, "listtransactions")[0], json!(["*", 26, 0, false]));
        let p1 = history_page(&mut c, &[], 1).await.unwrap();
        assert_eq!((p1.items[0].txid.clone(), p1.more), (format!("{:064x}", 35), true));
        assert_eq!(p1.items.len(), 24, "the move line is skipped");
        let p2 = history_page(&mut c, &[], 2).await.unwrap();
        assert_eq!((p2.items.len(), p2.more), (11, false));
        assert_eq!(p2.items.last().unwrap().txid, format!("{:064x}", 1));
        let (send, receive) = (&p0.items[0], &p0.items[1]);
        assert_eq!((send.category.as_str(), send.sats, send.fee, send.amount), ("send", -25_000_000, Some(226), -0.25));
        assert_eq!((receive.category.as_str(), receive.sats, receive.fee), ("receive", 150_000_000, None));
        assert_eq!((send.logged, send.speed, send.replaceable.as_deref()), (false, None, Some("no")));
    }

    #[tokio::test]
    async fn history_shows_what_the_log_knows() {
        let (mut c, _, _) = node(Node { list: listed(4), ..Default::default() });
        let log = [LogEntry {
            txid: format!("{:064x}", 2), time: 1, address: TO.into(), amount: 25_000_000, fee: 226, feerate: 1000, speed: Speed::Cheap,
            max: false, change: 5, replaced_by: Some("9".repeat(64)), replaces: None,
        }];
        let p = history_page(&mut c, &log, 0).await.unwrap();
        let it = p.items.iter().find(|i| i.txid == log[0].txid).unwrap();
        assert_eq!((it.logged, it.speed, it.max, it.replaced_by.clone()), (true, Some(Speed::Cheap), Some(false), Some("9".repeat(64))));
    }

    #[test]
    fn times_in_utc() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_790_690_585), "2026-09-29T14:03:05Z");
        assert_eq!(iso_utc(4_107_542_399), "2100-02-28T23:59:59Z");
        assert_eq!(iso_utc(-1), "1969-12-31T23:59:59Z");
    }

    #[tokio::test]
    async fn the_csv_has_every_transaction_with_the_logs_speed() {
        let (mut c, _, _) = node(Node { list: listed(2), ..Default::default() });
        let log = [LogEntry {
            txid: format!("{:064x}", 2), time: 1, address: TO.into(), amount: 25_000_000, fee: 226, feerate: 1000, speed: Speed::Hour,
            max: false, change: 5, replaced_by: Some("9".repeat(64)), replaces: None,
        }];
        let items = all_history(&mut c, &log).await.unwrap();
        let text = csv(&items);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "txid,time,category,amount,fee,confirmations,address,speed,replaced_by");
        assert_eq!(lines[1], format!("{:064x},2026-09-27T19:06:41Z,send,-0.25000000,0.00000226,1,{},Within an hour,{}", 2, TO, "9".repeat(64)));
        assert_eq!(lines[2], format!("{:064x},2026-09-27T19:06:40Z,receive,1.50000000,,2,{},,", 1, TO));
        assert_eq!(lines.len(), 3);
        // Odd text is quoted, and nothing reads as a formula.
        assert_eq!(csv_text("a,b"), "\"a,b\"");
        assert_eq!(csv_text("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_text("=SUM(A1)"), "'=SUM(A1)");
    }

    #[tokio::test]
    async fn the_csv_reads_every_page_of_a_long_history() {
        let (mut c, calls, _) = node(Node { list: listed(2500), ..Default::default() });
        let items = all_history(&mut c, &[]).await.unwrap();
        assert_eq!(items.len(), 2500);
        assert_eq!((items[0].txid.clone(), items[2499].txid.clone()), (format!("{:064x}", 2500), format!("{:064x}", 1)));
        assert_eq!(params(&calls, "listtransactions").len(), 3);
    }

    /// The whole sequence against a real freebankd. Ignored: it needs a node with a funded wallet.
    ///
    ///   FB_SEND_URL=http://127.0.0.1:<rpcport> FB_SEND_USER=<user> FB_SEND_PASS=<pass> \
    ///     cargo test -j 2 send::tests::real_node -- --ignored --nocapture
    ///
    /// FB_SEND_COOKIE=<datadir>/.cookie instead of the user and password. FB_SEND_PASSFILE: a 0600
    /// file holding the passphrase of an encrypted wallet (never printed). FB_SEND_AMOUNT: sats per
    /// send (default 100000). FB_SEND_MAX=1 also sends everything with Max. Every send goes to a new
    /// address of the same wallet, so only the fees leave it.
    #[tokio::test]
    #[ignore]
    async fn real_node() {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let url = var("FB_SEND_URL").expect("FB_SEND_URL");
        let (user, pass) = match var("FB_SEND_COOKIE") {
            Some(p) => {
                let s = std::fs::read_to_string(p).expect("the cookie");
                let (u, p) = s.trim().split_once(':').expect("user:password");
                (u.to_string(), p.to_string())
            }
            None => (var("FB_SEND_USER").expect("FB_SEND_USER"), var("FB_SEND_PASS").expect("FB_SEND_PASS")),
        };
        let passphrase = var("FB_SEND_PASSFILE").map(|p| std::fs::read_to_string(p).expect("the passphrase file").trim_end().to_string());
        let amount: i64 = var("FB_SEND_AMOUNT").map_or(100_000, |a| a.parse().expect("FB_SEND_AMOUNT in sats"));
        let mut c = FreeBankClient::with_http(reqwest::Client::builder().no_proxy().build().unwrap());
        c.configure(&url, &user, &pass);
        let book = Book::default();
        let dir = temp("real");
        let log = SendLog::new(&dir);
        // One guard for the whole run, as the app keeps one (wallet::RelockGuard).
        let guard = crate::wallet::RelockGuard::default();

        let encrypted = crate::wallet::status(&mut c).await.unwrap().encrypted;
        println!("wallet encrypted: {}", encrypted);
        // What withUnlock does: try, and on -13 unlock for 30 s, try again, lock.
        macro_rules! signing {
            ($what:expr, $call:expr) => {{
                match $call.await {
                    Err(e) if e.starts_with("RPC error -13:") => {
                        println!("{}: locked, as expected ({}); unlocking for 30 s", $what, e);
                        let p = passphrase.clone().expect("the wallet is locked: set FB_SEND_PASSFILE");
                        crate::wallet::unlock(&mut c, &guard, p, 30).await.expect("unlock");
                        let r = $call.await;
                        crate::wallet::lock(&mut c).await.unwrap();
                        r
                    }
                    r => r,
                }
            }};
        }

        let fees = work_out_fees(&mut c).await.unwrap();
        println!("fee choices: basis {}, mempool {} vB, {:?}", fees.basis, fees.mempool_vbytes, fees.choices.iter().map(|c| (c.label, c.sat_per_kvb)).collect::<Vec<_>>());
        assert!(fees.choices.iter().all(|c| c.sat_per_kvb >= MIN_RATE));
        let own = signing!("getnewaddress", c.call_ui("getnewaddress", vec![json!(""), json!("legacy")])).unwrap();
        let own = own.as_str().unwrap().to_string();

        // An address check that fails, and an amount the wallet can't cover.
        assert_eq!(prepare(&book, &mut c, SendRequest { address: "Xnope".into(), amount: Some(1000), max: false, speed: Speed::Next }).await.unwrap_err(), NOT_AN_ADDRESS);
        let e = prepare(&book, &mut c, SendRequest { address: own.clone(), amount: Some(MAX_SATS), max: false, speed: Speed::Next }).await.unwrap_err();
        println!("too much: {}", e);
        assert_eq!(e, NOT_ENOUGH);

        // A send: the fee shown is the fee paid, replaceable, and the size estimate is Core's.
        let q = prepare(&book, &mut c, SendRequest { address: own.clone(), amount: Some(amount), max: false, speed: Speed::Cheap }).await.unwrap();
        println!("prepared: amount {} fee {} ({} sat/vB) vsize {} change {}", q.amount, q.fee, q.sat_per_vb, q.vsize, q.change);
        let sent = signing!("send_confirm", confirm(&book, &mut c, &q.id, Some(&log))).unwrap();
        println!("sent: {}", sent.txid);
        let t = c.call_ui("gettransaction", vec![json!(sent.txid)]).await.unwrap();
        assert_eq!(sats(&t["fee"]), Some(-q.fee), "the fee paid is the fee shown");
        assert_eq!(t["bip125-replaceable"], "yes");
        let tx = c.call_ui("decoderawtransaction", vec![t["hex"].clone()]).await.unwrap();
        let (real, n_in) = (tx["vsize"].as_i64().unwrap(), tx["vin"].as_array().unwrap().len() as i64);
        println!("signed vsize {} (estimated {}), {} inputs, {} outputs", real, q.vsize, n_in, tx["vout"].as_array().unwrap().len());
        assert!(q.vsize >= real && q.vsize - real <= n_in, "the estimate is Core's signed size, 72-byte signatures");
        let paid_rate = q.fee * 1000 / real;
        assert!(paid_rate >= q.sat_per_kvb, "the rate paid ({}) is at least the rate chosen ({})", paid_rate, q.sat_per_kvb);
        assert_eq!(confirm(&book, &mut c, &q.id, Some(&log)).await.unwrap_err(), EXPIRED, "a prepared send goes once");

        // Speed up: the quoted total is what bumpfee takes, and it replaces the old one.
        let bq = quote_speed_up(&book, &mut c, &log.read().unwrap(), &sent.txid).await.unwrap();
        println!("speed up quote: old fee {}, {:?}", bq.old_fee, bq.choices.iter().map(|c| (c.label, c.fee, c.ok)).collect::<Vec<_>>());
        let b = signing!("send_speed_up", speed_up(&book, &mut c, Some(&log), &sent.txid, Speed::Next)).unwrap();
        println!("sped up: {} replaces {}, fee {} (was {})", b.txid, b.old_txid, b.fee, b.old_fee);
        let t2 = c.call_ui("gettransaction", vec![json!(b.txid)]).await.unwrap();
        assert_eq!(sats(&t2["fee"]), Some(-b.fee));
        assert!(b.fee >= bq.choices[0].fee);
        let t1 = c.call_ui("gettransaction", vec![json!(sent.txid)]).await.unwrap();
        assert_eq!(t1["replaced_by_txid"], json!(b.txid));
        let e = quote_speed_up(&book, &mut c, &log.read().unwrap(), &sent.txid).await.unwrap_err();
        assert!(e.starts_with("This send was sped up already"), "{}", e);

        // History: newest first, with the log's speed and the replacement.
        let h = history_page(&mut c, &log.read().unwrap(), 0).await.unwrap();
        let new = h.items.iter().find(|i| i.txid == b.txid && i.category == "send").expect("the new send in History");
        assert_eq!((new.speed, new.logged, new.fee), (Some(Speed::Next), true, Some(b.fee)));
        let old = h.items.iter().find(|i| i.txid == sent.txid && i.category == "send").expect("the old send in History");
        assert_eq!(old.replaced_by.as_deref(), Some(b.txid.as_str()));
        let all = all_history(&mut c, &log.read().unwrap()).await.unwrap();
        let text = csv(&all);
        assert!(text.lines().any(|l| l.starts_with(&b.txid) && l.contains(",send,") && l.contains("Next block")));
        println!("history: {} on page 0 (more: {}), {} in the CSV", h.items.len(), h.more, all.len());

        if var("FB_SEND_MAX").as_deref() == Some("1") {
            let coins = coins_from(&c.call_ui("listunspent", vec![json!(0), json!(9_999_999), json!([]), json!(false)]).await.unwrap());
            // Zero-value coins stay out; so does unconfirmed change from a sped-up send (Core
            // counts a transaction that replaces another as unsafe until it confirms).
            let coins: Vec<Coin> = coins.into_iter().filter(|c| c.sats > 0).collect();
            let q = prepare(&book, &mut c, SendRequest { address: own.clone(), amount: None, max: true, speed: Speed::Hour }).await.unwrap();
            let sum: i64 = coins.iter().map(|c| c.sats).sum();
            println!("max: {} coins, {} sats; amount {} fee {} vsize {}", coins.len(), sum, q.amount, q.fee, q.vsize);
            assert_eq!((q.amount + q.fee, q.change), (sum, 0));
            let m = signing!("send_confirm (max)", confirm(&book, &mut c, &q.id, Some(&log))).unwrap();
            let t = c.call_ui("gettransaction", vec![json!(m.txid)]).await.unwrap();
            let tx = c.call_ui("decoderawtransaction", vec![t["hex"].clone()]).await.unwrap();
            assert_eq!(tx["vout"].as_array().unwrap().len(), 1, "no change");
            assert_eq!(tx["vin"].as_array().unwrap().len(), coins.len(), "every spendable coin");
            assert_eq!(sats(&t["fee"]), Some(-q.fee));
            assert_eq!(quote_speed_up(&book, &mut c, &log.read().unwrap(), &m.txid).await.unwrap_err(), MAX_CANT);
            println!("max sent: {}", m.txid);
        }
        std::fs::remove_dir_all(dir).unwrap();
        println!("REAL NODE: ALL PASS");
    }

    #[test]
    fn the_csv_file_never_replaces_another() {
        let dir = temp("csv");
        let a = save_csv(&dir, "one", "2026-09-29").unwrap();
        let b = save_csv(&dir, "two", "2026-09-29").unwrap();
        assert_eq!(a.file_name().unwrap(), "freebank-history-2026-09-29.csv");
        assert_eq!(b.file_name().unwrap(), "freebank-history-2026-09-29-2.csv");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
