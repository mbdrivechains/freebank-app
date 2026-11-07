//! Bring coins in from other words (v0.7.3): a Sparrow or Electrum wallet's recovery words, or a Bitcoin wallet's that
//! held coins before eCash split from Bitcoin. FreeBank finds their coins on eCash (`scantxoutset`, so no wallet and no
//! rescan) and moves them all into the main eCash wallet in one payment it builds and signs here. That payment carries
//! eCash's replay stamp, so it can't be repeated on Bitcoin: the ECX moves and any bitcoin of the same words stays.
//! The words are used for the moment they're needed and never kept.
//!
//! Which words: BIP39 (Sparrow's, and Electrum's "BIP39 seed" option) at native segwit's usual path m/84'/c'/0', or
//! Electrum's own segwit seed (its default since 2017) at m/0'. Other script types (legacy, nested segwit, taproot) and
//! Electrum's older or two-factor seeds are refused, plainly.

use super::conn::Conn;
use super::keys::{coin, AccountKey, AccountPub};
use super::sign::{self, Spend, SEQUENCE};
use super::wallet;
use crate::seed::Chain;
use bitcoin::absolute::LockTime;
use bitcoin::consensus::encode;
use bitcoin::bip32::Xpriv;
use bitcoin::hashes::hmac::{Hmac, HmacEngine};
use bitcoin::hashes::{sha512, Hash, HashEngine};
use bitcoin::transaction::Version;
use bitcoin::{Amount, NetworkKind, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::str::FromStr;
use zeroize::{Zeroize, Zeroizing};

/// How far along each branch the search looks: Sparrow's and Electrum's gap limit is 20, so a wallet that used this
/// many addresses in a row without a gap of 20 is still found.
pub const SCAN_RANGE: u32 = 999;
/// The most coins one move takes (a payment well under the standard size).
pub const MAX_COINS: usize = 400;
/// The smallest output the node relays to a P2WPKH address.
const DUST: u64 = 294;
/// A P2WPKH input's size, vbytes: a coin worth less than this at the fee rate costs more to move than it brings.
const INPUT_VSIZE: f64 = 68.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// BIP39 words, native segwit (m/84'/c'/0').
    Bip39,
    /// Electrum's own segwit seed (m/0').
    Electrum,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Bip39 => "bip39",
            Kind::Electrum => "electrum",
        }
    }
}

/// Lower case, one space between words (Electrum's and BIP39's normal form for English words; Electrum's for its
/// seed's passphrase too). Built in one wiped string.
fn normal(text: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(text.len()));
    for w in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        for c in w.chars() {
            out.extend(c.to_lowercase());
        }
    }
    out
}

fn hmac_sha512(key: &[u8], msg: &[u8]) -> [u8; 64] {
    let mut e = HmacEngine::<sha512::Hash>::new(key);
    e.input(msg);
    Hmac::<sha512::Hash>::from_engine(e).to_byte_array()
}

/// PBKDF2-HMAC-SHA512 to 64 bytes (one block): BIP39's and Electrum's seed from their words.
fn pbkdf2_sha512(pass: &[u8], salt: &[u8], rounds: u32) -> Zeroizing<[u8; 64]> {
    let mut first = Zeroizing::new(Vec::with_capacity(salt.len() + 4));
    first.extend_from_slice(salt);
    first.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha512(pass, &first);
    let mut out = Zeroizing::new(u);
    for _ in 1..rounds {
        u = hmac_sha512(pass, &u);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    u.zeroize();
    out
}

/// Electrum's seed type, from the HMAC of its words: "100" segwit, "01" standard, "101" two-factor, "102" two-factor
/// segwit (electrum/mnemonic.py, seed_type).
fn electrum_type(words: &str) -> Option<&'static str> {
    let h = hex::encode(hmac_sha512(b"Seed version", words.as_bytes()));
    if h.starts_with("100") {
        Some("segwit")
    } else if h.starts_with("01") {
        Some("standard")
    } else if h.starts_with("101") || h.starts_with("102") {
        Some("2fa")
    } else {
        None
    }
}

/// The accounts `text` (and the seed's own passphrase, empty for none) may be on `chain`, most likely first, each with
/// which kind of words it takes them for. Usually one; two when the words are both an Electrum segwit seed and valid
/// BIP39 (Electrum's own check against that can't be relied on), and the search tries both. A BIP39-valid seed that only
/// looks like Electrum's older or two-factor kind is taken as BIP39 (about 1 in 200 of Sparrow's would).
pub fn candidates(text: &str, passphrase: &str, chain: Chain) -> Result<Vec<(Kind, AccountKey)>, String> {
    let words = normal(text);
    if words.is_empty() {
        return Err("Enter the wallet's recovery words.".into());
    }
    if !words.is_ascii() || !passphrase.is_ascii() {
        return Err("FreeBank takes English recovery words, and a passphrase of plain letters, digits and punctuation.".into());
    }
    let bip39 = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &words).ok();
    let mut kinds = Vec::new();
    match (electrum_type(&words), bip39.is_some()) {
        (Some("segwit"), true) => kinds.extend([Kind::Electrum, Kind::Bip39]),
        (Some("segwit"), false) => kinds.push(Kind::Electrum),
        (_, true) => kinds.push(Kind::Bip39),
        (Some("standard"), false) => {
            return Err("These are Electrum's words for an older wallet (legacy addresses). FreeBank brings in Electrum's \
                        segwit wallets, its default since 2017: send the coins to a segwit address first."
                .into())
        }
        (Some(_), false) => {
            return Err("These are Electrum's words for a two-factor wallet, which FreeBank can't sign for. Send the coins \
                        to a segwit address first."
                .into())
        }
        (None, false) => return Err("These aren't BIP39 or Electrum recovery words: check each word and their order.".into()),
    }
    let net = match chain {
        Chain::Main => NetworkKind::Main,
        _ => NetworkKind::Test,
    };
    let mut out = Vec::new();
    for kind in kinds {
        let (seed, origin) = match kind {
            // Electrum normalises the passphrase as it does the words (mnemonic.py, mnemonic_to_seed).
            Kind::Electrum => {
                let mut salt = Zeroizing::new(String::from("electrum"));
                salt.push_str(&normal(passphrase));
                (pbkdf2_sha512(words.as_bytes(), salt.as_bytes(), 2048), vec![0])
            }
            Kind::Bip39 => {
                let m = bip39.as_ref().ok_or("These aren't BIP39 recovery words.")?;
                (Zeroizing::new(m.to_seed_normalized(passphrase)), vec![84, coin(chain), 0])
            }
        };
        let mut master = Xpriv::new_master(net, &seed[..]).map_err(|e| e.to_string())?;
        let key = AccountKey::other(&master, chain, &origin);
        crate::seed::wipe(&mut master);
        out.push((kind, key?));
    }
    Ok(out)
}

/// The account `text` gives that is `public` (the one its coins were found on), for the move.
pub fn account_for(text: &str, passphrase: &str, chain: Chain, public: &AccountPub) -> Result<AccountKey, String> {
    candidates(text, passphrase, chain)?
        .into_iter()
        .map(|(_, k)| k)
        .find(|k| k.public == *public)
        .ok_or_else(|| "These aren't the words the coins were found with. Nothing was sent.".to_string())
}

/// A coin of the other wallet's on eCash.
#[derive(Debug, Clone, PartialEq)]
pub struct Coin {
    pub outpoint: OutPoint,
    pub value: u64,
    pub place: (u32, u32),
    pub script: ScriptBuf,
    pub height: u64,
}

/// The other wallet's coins on eCash now: its receive and change branches, the first SCAN_RANGE + 1 addresses of each.
/// Left out: coinbase coins not yet spendable (under 100 confirmations), coins already spent by a payment in the node's
/// mempool (a move not yet confirmed), and the same coin named twice. Each coin's key is checked against the one derived
/// here, and its value and script against its parent transaction, which its txid vouches for (review M-A, as deposits'
/// coins): a node that lied about values on two tries could otherwise join signatures from both into a payment that
/// gives most of it away as fee.
pub async fn scan(conn: &Conn, acct: &AccountPub) -> Result<Vec<Coin>, String> {
    let req = json!([
        {"desc": acct.descriptor(0)?, "range": [0, SCAN_RANGE]},
        {"desc": acct.descriptor(1)?, "range": [0, SCAN_RANGE]},
    ]);
    let r = conn.node().without_timeout().call_typed("scantxoutset", vec![json!("start"), req]).await.map_err(|e| {
        match e {
            crate::rpc::RpcError::Rpc { code: -8, .. } => {
                "The eCash node is already looking through its coins for something else. Try again in a few minutes."
                    .to_string()
            }
            e => format!("The eCash node couldn't look for the coins: {}", e),
        }
    })?;
    if r["success"] != Value::Bool(true) {
        return Err("The eCash node stopped before it finished looking for the coins. Try again.".into());
    }
    let wrong = || "The eCash node describes a coin wrongly. Nothing was done.".to_string();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for u in r["unspents"].as_array().cloned().unwrap_or_default() {
        if u["coinbase"] == Value::Bool(true) && u["confirmations"].as_u64().unwrap_or(0) < 100 {
            continue;
        }
        let Some(place) = u["desc"].as_str().and_then(|d| acct.place_of_desc(d)) else { continue };
        let script = acct.script(place.0, place.1)?;
        let (Some(txid), Some(vout), Some(block)) = (
            u["txid"].as_str().and_then(|t| Txid::from_str(t).ok()),
            u["vout"].as_u64().and_then(|v| u32::try_from(v).ok()),
            u["blockhash"].as_str(),
        ) else {
            return Err(wrong());
        };
        let outpoint = OutPoint { txid, vout };
        if !seen.insert(outpoint) {
            continue;
        }
        // The parent, from its block (no txindex needed): its txid is worked out here, so its outputs are what they say.
        let raw = conn
            .node()
            .call_typed("getrawtransaction", vec![json!(txid.to_string()), json!(false), json!(block)])
            .await
            .map_err(|e| format!("The eCash node couldn't show a coin's transaction: {}", e))?;
        let parent: Transaction = raw.as_str().and_then(|h| encode::deserialize_hex(h).ok()).ok_or_else(wrong)?;
        if parent.compute_txid() != txid {
            return Err(wrong());
        }
        let Some(o) = parent.output.get(vout as usize) else { return Err(wrong()) };
        if o.script_pubkey != script {
            continue;
        }
        // Still unspent with the mempool counted: null when a payment waiting there spends it.
        let live = conn
            .node()
            .call_typed("gettxout", vec![json!(txid.to_string()), json!(vout), json!(true)])
            .await
            .map_err(|e| format!("The eCash node couldn't say whether a coin is spent: {}", e))?;
        if live.is_null() {
            continue;
        }
        out.push(Coin { outpoint, value: o.value.to_sat(), place, script, height: u["height"].as_u64().unwrap_or(0) });
    }
    out.sort_by(|a, b| b.value.cmp(&a.value));
    Ok(out)
}

/// The move, ready to sign.
#[derive(Debug)]
pub struct Sweep {
    pub tx: Transaction,
    pub spends: Vec<Spend>,
    /// The coins it takes and their worth.
    pub taken: usize,
    pub taken_sats: u64,
    /// Coins worth less than the fee to move them, left where they are.
    pub small: usize,
    pub arrives: u64,
    pub fee: u64,
}

/// The move: the coins (largest first) to `to`, at `rate` sat/vB, with eCash's replay stamp. Coins worth less than
/// their own input's fee are left out (they'd lower what arrives, and dust sent to a wallet is a way to trace it); then
/// as many of the rest as one payment takes, up to MAX_COINS and FreeBank's largest fee.
pub fn sweep(coins: &[Coin], to: ScriptBuf, rate: f64) -> Result<Sweep, String> {
    let rate = rate.max(1.0);
    let worth: Vec<&Coin> = coins.iter().filter(|c| c.value as f64 > INPUT_VSIZE * rate).collect();
    let small = coins.len() - worth.len();
    if worth.is_empty() {
        return Err(if coins.is_empty() {
            "There are no coins to move.".into()
        } else {
            "These coins are worth less than the fee to move them.".into()
        });
    }
    let build = |take: &[&Coin], out: u64| Transaction {
        version: Version::TWO,
        lock_time: LockTime::from_consensus(super::replay_locktime()),
        input: take
            .iter()
            .map(|c| TxIn {
                previous_output: c.outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence(SEQUENCE),
                witness: Witness::new(),
            })
            .collect(),
        output: vec![TxOut { value: Amount::from_sat(out), script_pubkey: to.clone() }],
    };
    let mut n = worth.len().min(MAX_COINS);
    let fee_of = |n: usize| (sign::signed_vsize(&build(&worth[..n], 0)) as f64 * rate).ceil() as u64;
    while n > 1 && fee_of(n) > wallet::MAX_FEE {
        n -= 1;
    }
    let fee = fee_of(n);
    if fee > wallet::MAX_FEE {
        return Err("Moving these coins would cost more than FreeBank's largest fee: try again when fees are lower.".into());
    }
    let take = &worth[..n];
    let total = take.iter().try_fold(0u64, |a, c| a.checked_add(c.value)).ok_or("The coins' amounts don't add up.")?;
    let arrives = total.checked_sub(fee).filter(|a| *a >= DUST).ok_or("These coins are worth less than the fee to move them.")?;
    let spends = take
        .iter()
        .enumerate()
        .map(|(vin, c)| Spend { vin, place: c.place, value: c.value, script: c.script.clone() })
        .collect();
    Ok(Sweep { tx: build(take, arrives), spends, taken: n, taken_sats: total, small, arrives, fee })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABOUT: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn one(text: &str, pass: &str, chain: Chain) -> (Kind, AccountKey) {
        let mut c = candidates(text, pass, chain).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.len(), 1, "one reading");
        c.remove(0)
    }

    #[test]
    fn bip39_words_give_bip84s_account() {
        // BIP84's published vector: "abandon" x11 "about", no passphrase.
        let (kind, key) = one(ABOUT, "", Chain::Main);
        assert_eq!(kind, Kind::Bip39);
        assert_eq!(key.public.zpub(), "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs");
        assert_eq!(key.public.address(0, 0).unwrap(), "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
        assert_eq!(key.public.path(), "m/84'/0'/0'");
        // Spaces, capitals and line breaks don't matter.
        let (_, again) = one(&format!("  {}\n", ABOUT.to_uppercase()), "", Chain::Main);
        assert_eq!(again.public, key.public);
        // A BIP39 passphrase makes another wallet, and its case counts.
        let (_, other) = one(ABOUT, "TREZOR", Chain::Main);
        let (_, lower) = one(ABOUT, "trezor", Chain::Main);
        assert_ne!(other.public.xpub, key.public.xpub);
        assert_ne!(other.public.xpub, lower.public.xpub);
        // Test prefixes: m/84'/1'/0'.
        let (_, t) = one(ABOUT, "", Chain::Test);
        assert_eq!(t.public.path(), "m/84'/1'/0'");
        assert!(t.public.address(0, 0).unwrap().starts_with("tb1q"));
        // The move finds the account it was prepared for.
        assert!(account_for(ABOUT, "", Chain::Main, &key.public).is_ok());
        assert!(account_for(ABOUT, "TREZOR", Chain::Main, &key.public).is_err());
    }

    #[test]
    fn electrum_segwit_seeds_give_electrums_wallet() {
        // Electrum's own test (tests/test_wallet_vertical.py, test_electrum_seed_segwit).
        let words = "bitter grass shiver impose acquire brush forget axis eager alone wine silver";
        let c = candidates(words, "", Chain::Main).unwrap();
        let (kind, key) = &c[0];
        assert_eq!(*kind, Kind::Electrum);
        assert_eq!(key.public.path(), "m/0'");
        assert_eq!(key.public.zpub(), "zpub6nsHdRuY92FsMKdbn9BfjBCG6X8pyhCibNP6uDvpnw2cyrVhecvHRMa3Ne8kdJZxjxgwnpbHLkcR4bfnhHy6auHPJyDTQ3kianeuVLdkCYQ");
        assert_eq!(key.public.address(0, 0).unwrap(), "bc1q3g5tmkmlvxryhh843v4dz026avatc0zzr6h3af");
        // If these words also pass BIP39's checksum, the BIP39 reading comes second, never first.
        assert!(c.iter().skip(1).all(|(k, _)| *k == Kind::Bip39));
        // Places under m/0' are this account's; m/84'/0'/0' ones aren't.
        let fp = key.public.fingerprint;
        let p = bitcoin::bip32::DerivationPath::from_str("m/0'/1/4").unwrap();
        assert_eq!(key.public.place(fp, &p), Some((1, 4)));
        let q = bitcoin::bip32::DerivationPath::from_str("m/84'/0'/0'/1/4").unwrap();
        assert_eq!(key.public.place(fp, &q), None);
        // Electrum's seed extension is normalised as Electrum does: case and spacing don't matter.
        let a = &candidates(words, "My  Secret ", Chain::Main).unwrap()[0].1;
        let b = &candidates(words, "my secret", Chain::Main).unwrap()[0].1;
        assert_eq!(a.public, b.public);
        assert_ne!(a.public, key.public);
    }

    #[test]
    fn bip39_words_that_look_like_electrums_old_kinds_are_bip39() {
        // BIP39-valid 12-word phrases whose Electrum HMAC says "standard" or "2fa" (about 1 in 200): taken as BIP39.
        let wl = bip39::Language::English.word_list();
        let mut checked = 0;
        'outer: for a in wl.iter() {
            for w in wl.iter() {
                let words = format!("{a} winner thank year wave sausage worth useful legal winner thank {w}");
                if bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &words).is_err() {
                    continue;
                }
                if matches!(electrum_type(&words), Some("standard") | Some("2fa")) {
                    let c = candidates(&words, "", Chain::Main).unwrap();
                    assert_eq!(c.len(), 1);
                    assert_eq!(c[0].0, Kind::Bip39);
                    checked += 1;
                    if checked == 3 {
                        break 'outer;
                    }
                }
            }
        }
        assert_eq!(checked, 3);
    }

    #[test]
    fn other_words_are_refused_plainly() {
        assert!(candidates("", "", Chain::Main).is_err());
        let bad = candidates("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon", "", Chain::Main);
        assert!(matches!(bad, Err(e) if e.contains("check each word")));
        assert!(candidates(ABOUT, "pässe", Chain::Main).is_err());
    }

    #[test]
    fn pbkdf2_matches_bip39s_seed() {
        // BIP39's published seed for "abandon" x11 "about" with the passphrase "TREZOR".
        let s = pbkdf2_sha512(ABOUT.as_bytes(), b"mnemonicTREZOR", 2048);
        assert_eq!(hex::encode(&s[..]), "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04");
    }

    fn coins_of(key: &AccountKey, values: &[u64]) -> Vec<Coin> {
        values
            .iter()
            .enumerate()
            .map(|(i, &value)| Coin {
                outpoint: OutPoint {
                    txid: {
                        let mut b = [7u8; 32];
                        b[..8].copy_from_slice(&(i as u64).to_le_bytes());
                        Txid::from_byte_array(b)
                    },
                    vout: i as u32,
                },
                value,
                place: (0, i as u32),
                script: key.public.script(0, i as u32).unwrap(),
                height: 10,
            })
            .collect()
    }

    #[test]
    fn the_move_spends_the_coins_with_the_replay_stamp() {
        let (_, key) = one(ABOUT, "", Chain::Main);
        let coins = coins_of(&key, &[50_000, 20_000]);
        let to = key.public.script(0, 99).unwrap();
        let m = sweep(&coins, to.clone(), 2.0).unwrap();
        assert_eq!(m.tx.lock_time.to_consensus_u32(), super::super::replay_locktime());
        assert!(m.tx.input.iter().all(|i| i.sequence.0 == SEQUENCE));
        assert_eq!(m.tx.output.len(), 1);
        assert_eq!(m.tx.output[0].script_pubkey, to);
        assert_eq!((m.taken, m.taken_sats, m.small), (2, 70_000, 0));
        assert_eq!(m.arrives + m.fee, 70_000);
        assert_eq!(m.tx.output[0].value.to_sat(), m.arrives);
        assert_eq!(m.fee, (sign::signed_vsize(&m.tx) as f64 * 2.0).ceil() as u64);
        // It signs: each input with its own place's key.
        assert!(sign::sign(m.tx, &m.spends, &key).unwrap().len() > 100);
        // Nothing, and only dust, are refused.
        assert!(sweep(&[], to.clone(), 2.0).is_err());
        assert!(sweep(&coins_of(&key, &[100]), to, 2.0).is_err());
    }

    #[test]
    fn coins_worth_less_than_their_fee_stay_and_big_moves_take_what_fits() {
        let (_, key) = one(ABOUT, "", Chain::Main);
        let to = key.public.script(0, 99).unwrap();
        // At 10 sat/vB an input costs about 680 sats: a 500-sat coin stays.
        let m = sweep(&coins_of(&key, &[50_000, 500, 30_000]), to.clone(), 10.0).unwrap();
        assert_eq!((m.taken, m.small, m.taken_sats), (2, 1, 80_000));
        // At 100 sat/vB, 400 coins would pass the largest fee: the move takes as many as fit (the search sorts them
        // largest first).
        let many: Vec<u64> = (0..400u64).map(|i| 1_000_000 + i).collect();
        let m = sweep(&coins_of(&key, &many), to, 100.0).unwrap();
        assert!(m.taken < 400 && m.fee <= wallet::MAX_FEE, "{} coins, fee {}", m.taken, m.fee);
        assert_eq!(m.spends.len(), m.taken);
    }
}
