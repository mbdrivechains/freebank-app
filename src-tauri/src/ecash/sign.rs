//! Signing eCash payments here, never in the eCash node (v0.2.6 security review H1, H2, L4). The node holds the
//! wallets watch-only and funds a payment (a PSBT); FreeBank decodes it and refuses it unless it is exactly what was
//! asked: every input this account's (its key derived here for the place the PSBT names, and the coin's script that
//! key's), one output to the recipient for the amount, any other output change to this account's change branch, eCash's
//! replay stamp and no timelock (re-review L-B), and the fee what was quoted and under its ceilings, the rate's worked
//! out here from the transaction's size (re-review L-C). Then it signs each input here (P2WPKH, BIP143) and gives the
//! node only the finished transaction.

use super::keys::{AccountKey, AccountPub};
use bitcoin::consensus::encode;
use bitcoin::ecdsa::Signature;
use bitcoin::psbt::Psbt;
use bitcoin::secp256k1::{Message, Secp256k1};
use bitcoin::sighash::{EcdsaSighashType, SighashCache};
use bitcoin::{Amount, ScriptBuf, Transaction, Witness};

/// An input to sign: its index, its place in the account (branch, index), its value and script.
#[derive(Debug, Clone, PartialEq)]
pub struct Spend {
    pub vin: usize,
    pub place: (u32, u32),
    pub value: u64,
    pub script: ScriptBuf,
}

/// What a payment must be.
#[derive(Debug, Clone)]
pub struct Expect {
    /// The recipient's script and what it must get.
    pub to: ScriptBuf,
    pub sats: u64,
    /// The fee the quote said.
    pub fee: u64,
    /// The most the fee may be, whatever was quoted.
    pub max_fee: u64,
    /// The most its rate may be, sat/vB, on the signed transaction's largest size.
    pub max_rate: u64,
}

/// The sequence every input must have: replaceable (BIP125), no relative timelock.
pub const SEQUENCE: u32 = 0xffff_fffd;

/// The largest the signed transaction can be, vbytes: each input's P2WPKH witness at its largest (a 72-byte signature
/// and its sighash byte, a 33-byte key, their lengths and the item count) and the segwit marker.
pub fn signed_vsize(tx: &Transaction) -> u64 {
    let base = encode::serialize(tx).len() as u64; // no witness yet
    let weight = base * 4 + 2 + 109 * tx.input.len() as u64;
    weight.div_ceil(4)
}

/// A payment checked against what was asked: the unsigned transaction and the inputs to sign.
#[derive(Debug)]
pub struct Checked {
    pub tx: Transaction,
    pub spends: Vec<Spend>,
    pub fee: u64,
}

fn b64(psbt: &str) -> Result<Psbt, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let bytes = STANDARD.decode(psbt.trim()).map_err(|_| "The eCash node's payment can't be read.")?;
    Psbt::deserialize(&bytes).map_err(|_| "The eCash node's payment can't be read.".into())
}

/// Decode `psbt` and check it against `expect`, for `acct`.
pub fn check(psbt: &str, acct: &AccountPub, expect: &Expect) -> Result<Checked, String> {
    let p = b64(psbt)?;
    let tx = p.unsigned_tx.clone();
    if p.inputs.len() != tx.input.len() || p.outputs.len() != tx.output.len() {
        return Err("The eCash node's payment is malformed.".into());
    }
    // eCash's replay stamp, so it can't be replayed on Bitcoin, and nothing that could hold it back (re-review L-B).
    if tx.lock_time.to_consensus_u32() != super::REPLAY_LOCKTIME || tx.input.iter().any(|i| i.sequence.0 != SEQUENCE) {
        return Err("The eCash node's payment lacks eCash's replay stamp or carries a timelock. Nothing was sent.".into());
    }
    let mut spends = Vec::new();
    let mut total_in = 0u64;
    for (vin, input) in p.inputs.iter().enumerate() {
        let utxo = input.witness_utxo.as_ref().ok_or("The eCash node's payment spends a coin it doesn't describe.")?;
        // The coin's whole parent transaction, which its txid vouches for: a segwit v0 signature commits only to its
        // own input's value, so a node lying about another input's value on two tries could combine the two signatures
        // into a payment with a far larger fee (re-review M-A). Core's walletcreatefundedpsbt includes it.
        let prev = input.non_witness_utxo.as_ref().ok_or("The eCash node's payment doesn't show a coin's history.")?;
        let outpoint = tx.input[vin].previous_output;
        if prev.compute_txid() != outpoint.txid || prev.output.get(outpoint.vout as usize) != Some(utxo) {
            return Err("The eCash node's payment describes a coin wrongly. Nothing was sent.".into());
        }
        let place = input
            .bip32_derivation
            .iter()
            .find_map(|(pk, (fp, path))| acct.place(*fp, path).filter(|&(b, i)| acct.pubkey(b, i).is_ok_and(|k| k.0 == *pk)))
            .ok_or("The eCash node's payment spends a coin that isn't this wallet's.")?;
        if acct.script(place.0, place.1)? != utxo.script_pubkey {
            return Err("The eCash node's payment spends a coin that isn't this wallet's.".into());
        }
        let value = utxo.value.to_sat();
        total_in = total_in.checked_add(value).ok_or("The eCash node's payment adds up to too much.")?;
        spends.push(Spend { vin, place, value, script: utxo.script_pubkey.clone() });
    }
    let mut paid = 0u64;
    let mut to_recipient = 0;
    let mut total_out = 0u64;
    for (vout, out) in tx.output.iter().enumerate() {
        let v = out.value.to_sat();
        total_out = total_out.checked_add(v).ok_or("The eCash node's payment adds up to too much.")?;
        if out.script_pubkey == expect.to {
            to_recipient += 1;
            paid += v;
            continue;
        }
        // Anything else is change: this account's change branch, its key derived here.
        let change = p.outputs[vout].bip32_derivation.iter().any(|(pk, (fp, path))| {
            acct.place(*fp, path).is_some_and(|(b, i)| {
                b == 1 && acct.pubkey(b, i).is_ok_and(|k| k.0 == *pk) && acct.script(b, i).is_ok_and(|s| s == out.script_pubkey)
            })
        });
        if !change {
            return Err("The eCash node's payment pays somewhere it shouldn't. Nothing was sent.".into());
        }
    }
    if to_recipient != 1 || paid != expect.sats {
        return Err("The eCash node's payment isn't the amount to the address asked for. Nothing was sent.".into());
    }
    let fee = total_in.checked_sub(total_out).ok_or("The eCash node's payment spends more than its coins.")?;
    if fee != expect.fee || fee > expect.max_fee {
        return Err("The eCash node's payment has another fee than the one shown. Nothing was sent.".into());
    }
    if fee > signed_vsize(&tx).saturating_mul(expect.max_rate) {
        return Err("The eCash node's payment pays a higher fee rate than FreeBank asked for. Nothing was sent.".into());
    }
    Ok(Checked { tx, spends, fee })
}

/// Sign `spends` of `tx` with `key` (P2WPKH, SIGHASH_ALL) and give the finished transaction, hex.
pub fn sign(mut tx: Transaction, spends: &[Spend], key: &AccountKey) -> Result<String, String> {
    let secp = Secp256k1::new();
    let mut witnesses = Vec::new();
    {
        let mut cache = SighashCache::new(&tx);
        for s in spends {
            let sk = key.secret(s.place.0, s.place.1)?;
            let pk = bitcoin::CompressedPublicKey(sk.public_key(&secp));
            if ScriptBuf::new_p2wpkh(&pk.wpubkey_hash()) != s.script {
                return Err("A coin's key isn't the one FreeBank derives for it. Nothing was sent.".into());
            }
            let h = cache
                .p2wpkh_signature_hash(s.vin, &s.script, Amount::from_sat(s.value), EcdsaSighashType::All)
                .map_err(|e| e.to_string())?;
            let mut sk = sk;
            let sig = secp.sign_ecdsa(&Message::from(h), &sk);
            sk.non_secure_erase();
            let sig = Signature { signature: sig, sighash_type: EcdsaSighashType::All };
            witnesses.push((s.vin, Witness::p2wpkh(&sig, &pk.0)));
        }
    }
    for (vin, w) in witnesses {
        tx.input[vin].witness = w;
    }
    Ok(encode::serialize_hex(&tx))
}

/// The fee-rate ceiling to send `tx_hex` with (BTC/kvB, as sendrawtransaction's maxfeerate, written with 8 decimals as
/// Core's amounts are, rounded up): just above its own rate. Only an honest node heeds it: the checks above are the
/// defence, this is belt and braces.
pub fn max_fee_rate(tx_hex: &str, fee: u64) -> Result<String, String> {
    let tx: Transaction = encode::deserialize_hex(tx_hex).map_err(|e| e.to_string())?;
    let vsize = tx.vsize().max(1) as u64;
    // sats per kvB, a little over the transaction's own (1% and 1 sat/vB more), as whole sats.
    let per_kvb = (fee * 1000 * 101 / 100) / vsize + 1000 + 1;
    Ok(format!("{}.{:08}", per_kvb / 100_000_000, per_kvb % 100_000_000))
}
