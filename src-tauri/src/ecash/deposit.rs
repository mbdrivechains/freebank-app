//! Deposit at par (v0.3.0 "In and out"; `gateway/docs/distribution/CASH_OUT_DESIGN.md` §3.4): eCash from the app's
//! main eCash wallet into FreeBank through the peg, a BIP300 deposit (M5) built and signed here, as bids are (bmm.rs).
//!
//! The deposit spends FreeBank's treasury output on eCash (its "CTIP", anyone can spend it: `<OP_DRIVECHAIN> <130>
//! OP_TRUE`) and coins of the main wallet, and pays, in this order:
//! 1. the new treasury output: the old value plus the deposit, with the old one's script;
//! 2. right after it (the enforcer's rule), an OP_RETURN with the FreeBank address the deposit goes to, as text: the
//!    plain address, not BitWindow's `s130_…` wrapper (freebankd compares it with what the enforcer read,
//!    CheckDepositWithL1; freebankd's own test stack strips the wrapper the same way);
//! 3. change to the main wallet's change branch, if it is worth having.
//!
//! The treasury output is the enforcer's (`GetCtip`), and must match the eCash node's own view (`gettxout`): the same
//! value and script, confirmed, unspent even in its mempool. A wrong value would shrink the deposit and pay the
//! difference to a miner, so both have to agree. Each coin's value is checked against its parent transaction (a node
//! lying about values could raise the fee: sign.rs, re-review M-A). A deposit is recorded in deposits.json before it
//! goes out, and followed until FreeBank credits it.

use super::conn::Conn;
use super::keys::AccountPub;
use super::sign::Spend;
use super::{sats_of, REPLAY_LOCKTIME};
use crate::seed::Chain;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode;
use bitcoin::script::PushBytesBuf;
use bitcoin::transaction::Version;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// FreeBank's sidechain slot.
pub const SLOT: u8 = 130;
/// The opcodes eCash builds reserve for OP_DRIVECHAIN: OP_NOP5 (alphanet, the test presets) and OP_NOP8 (betanet,
/// the eCash network the app runs on as "main"; the enforcer's NetworkParams::betanet). On the main network only
/// OP_NOP8 is taken (security review L2): a hostile enforcer could otherwise name an OP_NOP5 output anyone can make.
const OP_NOP5: u8 = 0xb4;
const OP_NOP8: u8 = 0xb7;

fn opcodes(chain: Chain) -> &'static [u8] {
    match chain {
        Chain::Main => &[OP_NOP8],
        Chain::Regtest => &[OP_NOP5, OP_NOP8],
    }
}
/// What FreeBank keeps of each deposit, for the maker of the block that credits it (freebankd's SIDECHAIN_DEPOSIT_FEE,
/// src/sidechain.h): it credits the rest, in that block's coinbase.
pub const FREEBANK_DEPOSIT_FEE: u64 = 1_000;
/// The smallest deposit offered: 0.001 eCash.
pub const MIN_DEPOSIT: u64 = 100_000;
/// Change below this goes to the fee instead.
const MIN_CHANGE: u64 = 1_000;
/// Weight units each P2WPKH input's witness adds at its largest (sign.rs).
const P2WPKH_WITNESS_WU: u64 = 109;

/// FreeBank's treasury output on eCash.
#[derive(Debug, Clone, PartialEq)]
pub struct Ctip {
    pub outpoint: OutPoint,
    pub value: u64,
    pub script: ScriptBuf,
}

/// The treasury script's shape on `chain`: `<OP_DRIVECHAIN> PUSH1 130 OP_TRUE`.
pub fn treasury_script_ok(s: &ScriptBuf, chain: Chain) -> bool {
    let b = s.as_bytes();
    b.len() == 4 && opcodes(chain).contains(&b[0]) && b[1] == 0x01 && b[2] == SLOT && b[3] == 0x51
}

/// The enforcer's GetCtip answer (Connect JSON; the txid in display order, `ReverseHex`; 64-bit numbers may come as
/// strings). None: FreeBank has no treasury output yet.
pub fn parse_ctip(v: &Value) -> Result<Option<(Txid, u32, u64)>, String> {
    let c = match v.get("ctip") {
        None | Some(Value::Null) => return Ok(None),
        Some(c) => c,
    };
    let bad = || "The enforcer's answer about FreeBank's treasury can't be read.".to_string();
    let txid = c["txid"]["hex"].as_str().ok_or_else(bad)?;
    let txid = Txid::from_str(txid).map_err(|_| bad())?;
    let vout = match &c["vout"] {
        Value::Null => 0,
        n => n.as_u64().ok_or_else(bad)? as u32,
    };
    let value = match &c["value"] {
        Value::String(s) => s.parse::<u64>().map_err(|_| bad())?,
        n => n.as_u64().ok_or_else(bad)?,
    };
    Ok(Some((txid, vout, value)))
}

/// The plain FreeBank address in freebankd's `getdepositaddress` answer (`s130_<address>_<6 hex of SHA-256>`, as
/// freebankd's GenerateDepositAddress), checked; a bare address is taken as it is.
pub fn plain_deposit_address(answer: &str) -> Result<String, String> {
    use bitcoin::hashes::{sha256, Hash};
    let t = answer.trim();
    let base58 = |s: &str| (25..=64).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() && !"0OIl".contains(c));
    if let Some(rest) = t.strip_prefix(&format!("s{SLOT}_")) {
        let (addr, sum) = rest.rsplit_once('_').ok_or("FreeBank's deposit address can't be read.")?;
        let want = sha256::Hash::hash(format!("s{SLOT}_{addr}_").as_bytes()).to_string();
        if !base58(addr) || sum.len() != 6 || want[..6] != *sum {
            return Err("FreeBank's deposit address has a wrong checksum.".into());
        }
        return Ok(addr.to_string());
    }
    if base58(t) {
        return Ok(t.to_string());
    }
    Err("FreeBank's answer isn't a deposit address.".into())
}

/// A coin of the main wallet, checked to be its own.
#[derive(Debug, Clone, PartialEq)]
pub struct Coin {
    pub outpoint: OutPoint,
    pub value: u64,
    pub place: (u32, u32),
    pub script: ScriptBuf,
}

/// A deposit ready to sign.
#[derive(Debug, Clone)]
pub struct Built {
    /// What goes into the treasury.
    pub deposit: u64,
    pub tx: Transaction,
    pub spends: Vec<Spend>,
    pub fee: u64,
    #[allow(dead_code)] // read by the tests
    pub change: u64,
}

fn data_output(address: &str) -> Result<TxOut, String> {
    let push = PushBytesBuf::try_from(address.as_bytes().to_vec()).map_err(|_| "The FreeBank address is too long.")?;
    Ok(TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return(&push) })
}

fn the_tx(ctip: &Ctip, address: &str, amount: u64, coins: &[Coin], change: Option<(&ScriptBuf, u64)>) -> Result<Transaction, String> {
    let treasury = ctip.value.checked_add(amount).ok_or("That amount is too large.")?;
    let mut input = vec![TxIn {
        previous_output: ctip.outpoint,
        script_sig: ScriptBuf::new(),
        sequence: Sequence(super::sign::SEQUENCE),
        witness: Witness::new(),
    }];
    input.extend(coins.iter().map(|c| TxIn {
        previous_output: c.outpoint,
        script_sig: ScriptBuf::new(),
        sequence: Sequence(super::sign::SEQUENCE),
        witness: Witness::new(),
    }));
    let mut output = vec![
        TxOut { value: Amount::from_sat(treasury), script_pubkey: ctip.script.clone() },
        data_output(address)?,
    ];
    if let Some((script, value)) = change {
        output.push(TxOut { value: Amount::from_sat(value), script_pubkey: script.clone() });
    }
    Ok(Transaction { version: Version::TWO, lock_time: LockTime::from_consensus(REPLAY_LOCKTIME), input, output })
}

/// The signed size of `tx`, vbytes, with every wallet input's witness at its largest (the treasury input has none).
pub fn signed_vsize(tx: &Transaction, wallet_inputs: usize) -> u64 {
    let base = encode::serialize(tx).len() as u64;
    (base * 4 + 2 + P2WPKH_WITNESS_WU * wallet_inputs as u64).div_ceil(4)
}

/// Pick coins (largest first) to pay `amount` plus the fee at `rate` sat/vB, with change if it is worth having, and
/// build the deposit.
pub fn build(ctip: &Ctip, address: &str, amount: u64, coins: &[Coin], rate: u64, change_script: &ScriptBuf) -> Result<Built, String> {
    if amount < MIN_DEPOSIT {
        return Err(format!("The smallest deposit is {} eCash.", super::to_coins(MIN_DEPOSIT)));
    }
    let mut sorted = coins.to_vec();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));
    for k in 1..=sorted.len() {
        let picked = &sorted[..k];
        let have: u64 = picked.iter().map(|c| c.value).sum();
        // With change first, then without.
        let with = the_tx(ctip, address, amount, picked, Some((change_script, 0)))?;
        let fee_with = signed_vsize(&with, k) * rate;
        if have >= amount + fee_with + MIN_CHANGE {
            let change = have - amount - fee_with;
            let tx = the_tx(ctip, address, amount, picked, Some((change_script, change)))?;
            return Ok(Built { deposit: amount, spends: spends(picked), tx, fee: fee_with, change });
        }
        let without = the_tx(ctip, address, amount, picked, None)?;
        let fee_without = signed_vsize(&without, k) * rate;
        if have >= amount + fee_without {
            // What's left over is too small for change, so it goes to the fee.
            return Ok(Built { deposit: amount, spends: spends(picked), tx: without, fee: have - amount, change: 0 });
        }
    }
    let have: u64 = coins.iter().map(|c| c.value).sum();
    Err(format!(
        "The eCash wallet doesn't have enough confirmed eCash for that deposit and its fee: {} confirmed.",
        super::to_coins(have)
    ))
}

/// Everything the wallet's confirmed coins hold, less the fee: no change.
pub fn build_max(ctip: &Ctip, address: &str, coins: &[Coin], rate: u64) -> Result<Built, String> {
    if coins.is_empty() {
        return Err("The eCash wallet has no confirmed eCash to deposit.".into());
    }
    let have: u64 = coins.iter().map(|c| c.value).sum();
    let probe = the_tx(ctip, address, 0, coins, None)?;
    let fee = signed_vsize(&probe, coins.len()) * rate;
    let amount = have.checked_sub(fee).filter(|&a| a >= MIN_DEPOSIT).ok_or(format!(
        "The smallest deposit is {} eCash, and the wallet's confirmed eCash less the fee is less.",
        super::to_coins(MIN_DEPOSIT)
    ))?;
    let tx = the_tx(ctip, address, amount, coins, None)?;
    Ok(Built { deposit: amount, spends: spends(coins), tx, fee, change: 0 })
}

fn spends(picked: &[Coin]) -> Vec<Spend> {
    picked
        .iter()
        .enumerate()
        .map(|(i, c)| Spend { vin: i + 1, place: c.place, value: c.value, script: c.script.clone() })
        .collect()
}

fn say(e: crate::rpc::RpcError) -> String {
    format!("The eCash node: {e}")
}

/// FreeBank's treasury output, from the enforcer (Connect JSON over HTTP, as freebankd asks it), checked against the
/// eCash node: the same value and script, at least one confirmation, and unspent even in its mempool (a deposit
/// already waiting would make this one a double spend).
pub async fn treasury(http: &reqwest::Client, enforcer: &str, conn: &Conn) -> Result<Ctip, String> {
    let chain = conn.chain;
    let url = format!("http://{}/cusf.mainchain.v1.ValidatorService/GetCtip", enforcer.trim_start_matches("http://"));
    let reply = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .body(json!({ "sidechain_number": SLOT }).to_string())
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|_| format!("FreeBank couldn't reach the enforcer at {enforcer}."))?;
    if !reply.status().is_success() {
        return Err(format!("The enforcer didn't answer about FreeBank's treasury ({}).", reply.status()));
    }
    let v: Value = reply.json().await.map_err(|_| "The enforcer's answer can't be read.".to_string())?;
    let (txid, vout, value) = parse_ctip(&v)?.ok_or("FreeBank has no treasury output on eCash yet, so the app can't deposit.")?;
    // include_mempool: a deposit waiting in the mempool shows this output as spent.
    let out = conn.node().call_typed("gettxout", vec![json!(txid.to_string()), json!(vout), json!(true)]).await.map_err(say)?;
    if out.is_null() {
        return Err("A deposit is already waiting for the next eCash block. Try again after it.".into());
    }
    let node_value = sats_of(&out["value"]).ok_or("The eCash node's answer about the treasury can't be read.")?;
    let script = out["scriptPubKey"]["hex"]
        .as_str()
        .and_then(|h| ScriptBuf::from_hex(h).ok())
        .ok_or("The eCash node's answer about the treasury can't be read.")?;
    if node_value != value {
        return Err("The enforcer and the eCash node disagree about FreeBank's treasury. Nothing was done.".into());
    }
    if out["confirmations"].as_i64().unwrap_or(0) < 1 {
        return Err("FreeBank's treasury output isn't confirmed yet. Try again after the next eCash block.".into());
    }
    if !treasury_script_ok(&script, chain) {
        return Err("FreeBank's treasury output on eCash isn't the shape a deposit needs. Nothing was done.".into());
    }
    Ok(Ctip { outpoint: OutPoint { txid, vout }, value, script })
}

/// The main wallet's confirmed coins, each checked against its key and its parent transaction.
pub async fn coins(conn: &Conn, name: &str, acct: &AccountPub) -> Result<Vec<Coin>, String> {
    let w = conn.wallet(name);
    let list = w.call_typed("listunspent", vec![json!(1)]).await.map_err(say)?;
    let mut out = Vec::new();
    for u in list.as_array().into_iter().flatten() {
        let (Some(txid), Some(vout), Some(desc), Some(spk)) =
            (u["txid"].as_str(), u["vout"].as_u64(), u["desc"].as_str(), u["scriptPubKey"].as_str())
        else {
            continue;
        };
        let Some(place) = acct.place_of_desc(desc) else { continue };
        let script = acct.script(place.0, place.1)?;
        if script.to_hex_string() != spk {
            continue;
        }
        // The value from the parent transaction itself, which its txid vouches for.
        let t = w.call_typed("gettransaction", vec![json!(txid), json!(true)]).await.map_err(say)?;
        let parent: Transaction = t["hex"]
            .as_str()
            .and_then(|h| encode::deserialize_hex(h).ok())
            .ok_or("The eCash node's record of a coin can't be read.")?;
        let txid = Txid::from_str(txid).map_err(|e| e.to_string())?;
        if parent.compute_txid() != txid {
            return Err("The eCash node describes a coin wrongly. Nothing was done.".into());
        }
        let Some(o) = parent.output.get(vout as usize) else { continue };
        if o.script_pubkey != script {
            continue;
        }
        out.push(Coin { outpoint: OutPoint { txid, vout: vout as u32 }, value: o.value.to_sat(), place, script });
    }
    Ok(out)
}

/// A deposit, as kept in `<app data>/wallet/deposits.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Record {
    pub txid: String,
    /// The signed transaction, so a deposit that may not have gone out can be sent again: the same txid, so it can
    /// never count twice.
    pub hex: String,
    pub sats: u64,
    pub fee: u64,
    /// The FreeBank address it credits.
    pub address: String,
    /// Unix seconds.
    pub time: u64,
    /// "signed" (recorded, not yet handed to the node), "sent", "credited", "failed" (it can't confirm: its coins are
    /// back in the wallet).
    pub state: String,
    /// The FreeBank wallet the address is in (None: the node's default wallet).
    #[serde(default)]
    pub fb_wallet: Option<String>,
}

pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("deposits.json")
}

/// Whether a FreeBank wallet's `listtransactions` shows the deposit credited: an output of a block's coinbase
/// ("generate", or "immature" while young) to its address, of the deposit less FreeBank's fee. (getreceivedbyaddress
/// leaves coinbase outputs out.)
pub fn credited_in(list: &Value, address: &str, sats: u64) -> bool {
    list.as_array().into_iter().flatten().any(|t| {
        matches!(t["category"].as_str(), Some("generate" | "immature"))
            && t["address"].as_str() == Some(address)
            && sats_of(&t["amount"]).is_some_and(|a| a + FREEBANK_DEPOSIT_FEE >= sats)
    })
}

/// The deposits on record; none when there's no file. A file that can't be read is an error, never an empty list:
/// writing over it would lose a deposit waiting to be sent again (security review L4).
pub fn load(app_dir: &Path) -> Result<Vec<Record>, String> {
    match std::fs::read(path(app_dir)) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("FreeBank's record of deposits can't be read ({e}); it is left as it is.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("FreeBank's record of deposits can't be read: {e}")),
    }
}

pub fn save(app_dir: &Path, records: &[Record]) -> Result<(), String> {
    let p = path(app_dir);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    super::wallet::write_private(&p, &serde_json::to_vec_pretty(records).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctip(value: u64) -> Ctip {
        Ctip {
            outpoint: OutPoint { txid: Txid::from_str(&"11".repeat(32)).unwrap(), vout: 1 },
            value,
            script: ScriptBuf::from_bytes(vec![0xb7, 0x01, 130, 0x51]),
        }
    }

    fn coin(n: u8, value: u64) -> Coin {
        Coin {
            outpoint: OutPoint { txid: Txid::from_str(&format!("{n:02x}").repeat(32)).unwrap(), vout: 0 },
            value,
            place: (0, n as u32),
            script: ScriptBuf::from_bytes([vec![0x00, 0x14], vec![n; 20]].concat()),
        }
    }

    const ADDR: &str = "mvJt9PaS3ia9X1CKLYPRwW5fhhzSwYSaHP";

    #[test]
    fn the_treasury_output_then_the_address_then_change() {
        let change = ScriptBuf::from_bytes([vec![0x00, 0x14], vec![9; 20]].concat());
        let b = build(&ctip(5_000_000_000), ADDR, 40_000_000, &[coin(2, 30_000_000), coin(3, 100_000_000)], 2, &change).unwrap();
        // The treasury input first, then the one coin that covers it (the larger).
        assert_eq!(b.tx.input[0].previous_output, ctip(0).outpoint);
        assert_eq!(b.tx.input.len(), 2);
        assert_eq!(b.spends, vec![Spend { vin: 1, place: (0, 3), value: 100_000_000, script: coin(3, 0).script }]);
        // The new treasury output: the old value plus the deposit, the old script; the address right after it.
        assert_eq!(b.tx.output[0].value.to_sat(), 5_040_000_000);
        assert_eq!(b.tx.output[0].script_pubkey.as_bytes(), &[0xb7, 0x01, 130, 0x51]);
        assert_eq!(b.tx.output[1].script_pubkey.as_bytes()[2..], *ADDR.as_bytes());
        assert!(b.tx.output[1].script_pubkey.is_op_return());
        assert_eq!(b.tx.output[2].script_pubkey, change);
        // It adds up: what goes in is what comes out plus the fee, at the rate on the largest signed size.
        let ins = 5_000_000_000 + 100_000_000;
        let outs: u64 = b.tx.output.iter().map(|o| o.value.to_sat()).sum();
        assert_eq!(ins - outs, b.fee);
        assert_eq!(b.fee, signed_vsize(&b.tx, 1) * 2);
        assert_eq!(b.tx.lock_time.to_consensus_u32(), REPLAY_LOCKTIME);
        assert!(b.tx.input.iter().all(|i| i.sequence.0 == super::super::sign::SEQUENCE));
    }

    #[test]
    fn small_change_goes_to_the_fee_and_too_little_is_said() {
        let change = ScriptBuf::from_bytes([vec![0x00, 0x14], vec![9; 20]].concat());
        // Just enough with a little over: no change output, the rest is fee.
        let b = build(&ctip(1_000_000), ADDR, 1_000_000, &[coin(2, 1_000_700)], 2, &change).unwrap();
        assert_eq!(b.tx.output.len(), 2);
        assert_eq!(b.fee, 700);
        assert_eq!(b.change, 0);
        assert!(build(&ctip(1_000_000), ADDR, 1_000_000, &[coin(2, 1_000_100)], 2, &change).unwrap_err().contains("enough"));
        assert!(build(&ctip(1_000_000), ADDR, 50_000, &[coin(2, 9_000_000)], 2, &change).unwrap_err().contains("smallest"));
        // The treasury's shape, by network: OP_NOP8 only on main; another slot never.
        assert!(treasury_script_ok(&ScriptBuf::from_bytes(vec![0xb7, 0x01, 130, 0x51]), Chain::Main));
        assert!(!treasury_script_ok(&ScriptBuf::from_bytes(vec![0xb4, 0x01, 130, 0x51]), Chain::Main));
        assert!(treasury_script_ok(&ScriptBuf::from_bytes(vec![0xb4, 0x01, 130, 0x51]), Chain::Regtest));
        assert!(!treasury_script_ok(&ScriptBuf::from_bytes(vec![0xb7, 0x01, 131, 0x51]), Chain::Regtest));
        // Max: every coin, no change, the fee off the deposit.
        let m = build_max(&ctip(0), ADDR, &[coin(2, 1_000_000), coin(3, 2_000_000)], 2).unwrap();
        assert_eq!(m.tx.input.len(), 3);
        assert_eq!(m.tx.output.len(), 2);
        assert_eq!(m.deposit + m.fee, 3_000_000);
        assert_eq!(m.tx.output[0].value.to_sat(), m.deposit);
        // Several coins when one isn't enough.
        let b = build(&ctip(0), ADDR, 1_500_000, &[coin(2, 1_000_000), coin(3, 1_000_000)], 1, &change).unwrap();
        assert_eq!(b.tx.input.len(), 3);
    }

    #[test]
    fn a_credit_is_a_coinbase_output_to_the_address_less_freebanks_fee() {
        let list = json!([
            {"category": "generate", "amount": 0.0, "address": ADDR},
            {"category": "receive", "amount": 1.5, "address": ADDR},
            {"category": "immature", "amount": 1.49999, "address": "XOther"}
        ]);
        assert!(!credited_in(&list, ADDR, 150_000_000), "a plain receive or another address isn't the credit");
        let list = json!([{"category": "immature", "amount": 1.49999, "address": ADDR}]);
        assert!(credited_in(&list, ADDR, 150_000_000));
        let list = json!([{"category": "generate", "amount": 1.49998, "address": ADDR}]);
        assert!(!credited_in(&list, ADDR, 150_000_000), "less than the deposit less the fee");
    }

    #[test]
    fn the_enforcers_answer_and_the_deposit_address() {
        let v = json!({"ctip": {"txid": {"hex": "aa".repeat(32)}, "vout": 2, "value": "5040000000", "sequenceNumber": "3"}});
        assert_eq!(parse_ctip(&v).unwrap(), Some((Txid::from_str(&"aa".repeat(32)).unwrap(), 2, 5_040_000_000)));
        // vout 0 is left out by proto3; a number is taken too.
        let v = json!({"ctip": {"txid": {"hex": "bb".repeat(32)}, "value": 7}});
        assert_eq!(parse_ctip(&v).unwrap().unwrap().1, 0);
        assert_eq!(parse_ctip(&json!({})).unwrap(), None);
        assert!(parse_ctip(&json!({"ctip": {"txid": {"hex": "zz"}}})).is_err());

        use bitcoin::hashes::{sha256, Hash};
        let sum = &sha256::Hash::hash(format!("s130_{ADDR}_").as_bytes()).to_string()[..6];
        assert_eq!(plain_deposit_address(&format!("s130_{ADDR}_{sum}")).unwrap(), ADDR);
        assert_eq!(plain_deposit_address(ADDR).unwrap(), ADDR);
        assert!(plain_deposit_address(&format!("s130_{ADDR}_000000")).unwrap_err().contains("checksum"));
        assert!(plain_deposit_address(&format!("s131_{ADDR}_{sum}")).is_err());
    }
}
