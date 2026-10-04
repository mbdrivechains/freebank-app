//! v0.2.0 wallet: the passphrase first, the app seed's 24 recovery words, backup and restore, and
//! the Wallet section in Settings.
//!
//! - **Protect** (first run, and the Home banner for a wallet without a passphrase): `encryptwallet`,
//!   which stops freebankd (Core 0.16) and gives the wallet a new HD seed; the app starts its node
//!   again, unlocks, sets the HD seed from the words (seed.rs) with `sethdseed`, checks the node
//!   derives the addresses the words predict, saves the words encrypted (seed.enc) and locks.
//!   Restoring from words does the same with typed words, then scans the chain for their coins.
//! - **Settings > Wallet:** what the wallet is (file, lock, balances, key pool, HD seed id, last
//!   backup); Back up; Restore from a backup file or from words (into the wallet's place, the current
//!   wallet moved aside, never deleted); Change passphrase; Show recovery words and Show xprv (behind
//!   the passphrase).
//! - **Move my coins to the new words**, after protecting a wallet that already held coins.
//!
//! The wallet-sensitive RPCs (encryptwallet, walletpassphrase, walletpassphrasechange, sethdseed,
//! backupwallet, signrawtransactionwithwallet, rescanblockchain) go through here, never through the
//! generic `rpc_call`. Passphrases and words are never logged, stored in the clear, or put in an error.

pub mod commands;
pub(crate) mod job;
pub(crate) mod ops;
#[cfg(test)]
mod real_node;
#[cfg(test)]
mod tests;

use crate::node::{process, NodeManager, Settings};
use crate::rpc::{FreeBankClient, RpcError};
use crate::seed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;
pub(crate) const RPC_WALLET_UNLOCK_NEEDED: i64 = -13;
pub(crate) const RPC_WALLET_PASSPHRASE_INCORRECT: i64 = -14;

/// Long wallet calls (encryptwallet rewrites every key; sethdseed and a rescan chunk take a while).
pub(crate) const LONG: Duration = Duration::from_secs(600);

pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Local time as YYYYMMDD-HHMMSS, for the names of files moved aside.
pub(crate) fn stamp() -> String {
    let secs = now();
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the tm we own.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return format!(
                "{:04}{:02}{:02}-{:02}{:02}{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            );
        }
    }
    secs.to_string()
}

/// An amount from the node (a JSON number of ECX) in satoshis. Amounts have 8 decimals and stay far
/// below 2^53 satoshis, so rounding the f64 recovers them exactly.
pub(crate) fn sats(v: &Value) -> i64 {
    v.as_f64().map(|x| (x * 1e8).round() as i64).unwrap_or(0)
}

/// Satoshis as the exact decimal string the node takes ("1.50000000").
pub(crate) fn ecx(sats: i64) -> String {
    let sign = if sats < 0 { "-" } else { "" };
    let a = sats.unsigned_abs();
    format!("{}{}.{:08}", sign, a / 100_000_000, a % 100_000_000)
}

pub(crate) fn is_code(e: &RpcError, code: i64) -> bool {
    matches!(e, RpcError::Rpc { code: c, .. } if *c == code)
}

/// A client for the node on this computer (the cookie in its data folder, re-read after a restart),
/// giving up on a call after `timeout`.
pub(crate) fn local_client(mgr: &NodeManager, s: &Settings, timeout: Duration) -> Result<FreeBankClient, String> {
    let mut c = FreeBankClient::with_http(mgr.http.clone()).with_timeout(timeout);
    if !c.configure_local(&format!("http://127.0.0.1:{}", s.rpc_port), PathBuf::from(&s.datadir)) {
        return Err(format!("FreeBank can't find your node's password (its .cookie) in {}.", s.datadir));
    }
    Ok(c)
}

/// The wallet folder freebankd uses in `datadir` (Core 0.16's GetWalletDir without -walletdir):
/// `wallets/` when that folder exists, else the data folder itself.
pub(crate) fn wallet_dir(datadir: &Path) -> PathBuf {
    let w = datadir.join("wallets");
    if w.is_dir() {
        w
    } else {
        datadir.to_path_buf()
    }
}

/// The default wallet's file.
pub(crate) fn default_wallet(datadir: &Path) -> PathBuf {
    wallet_dir(datadir).join("wallet.dat")
}

// ---- What the app records about the wallet: <app data>/wallet/state.json ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Record {
    /// The HD seed the app last set from its words (its key id, as getwalletinfo prints it).
    pub seed_id: Option<String>,
    pub seed_set_at: Option<u64>,
    /// When the user last showed they have the words for `seed_id` (three of them given back, or all
    /// of them typed in to restore).
    pub words_confirmed_at: Option<u64>,
    /// When the app last backed up the wallet, which HD seed the wallet had then, and where it went.
    pub backup_at: Option<u64>,
    pub backup_seed_id: Option<String>,
    pub backup_path: Option<String>,
}

impl Record {
    pub fn path(app_dir: &Path) -> PathBuf {
        app_dir.join("wallet").join("state.json")
    }

    pub fn load(app_dir: &Path) -> Record {
        std::fs::read(Self::path(app_dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, app_dir: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        seed::write_private(&Self::path(app_dir), &json)
    }
}

// ---- Is the wallet protected? ----

/// Whether FreeBank's recovery words cover the wallet the node has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppSeed {
    /// FreeBank has no recovery words saved.
    None,
    /// The saved words give this wallet's HD seed.
    Matches,
    /// The saved words belong to another HD seed (a wallet restored from a file, or a seed set by hand).
    Other,
}

/// What the screens need to know about the wallet's protection. `protected` is what holds back the
/// address screens: the wallet has a passphrase and its HD seed comes from FreeBank's words.
#[derive(Debug, Clone, Serialize)]
pub struct Protection {
    pub encrypted: bool,
    /// Unix time (node clock) the wallet locks again; 0 while locked or without a passphrase.
    pub unlocked_until: u64,
    /// getwalletinfo's hdmasterkeyid.
    pub hd_seed_id: Option<String>,
    pub app_seed: AppSeed,
    pub protected: bool,
    /// The user has shown they have the words of the current seed.
    pub words_confirmed: bool,
    pub seed_set_at: Option<u64>,
    pub backup_at: Option<u64>,
    /// No backup since the wallet got its current HD seed.
    pub backup_due: bool,
    pub txcount: u64,
    /// No transactions yet: the first-run flow opens by itself, and address screens wait.
    pub new_wallet: bool,
    /// The app started this node and can restart it; else another program runs it.
    pub node_is_ours: bool,
    /// The seed file is there but can't be read.
    pub seed_file_problem: Option<String>,
}

pub(crate) fn protection_from(info: &Value, app_dir: &Path, node_is_ours: bool) -> Protection {
    let st = crate::wallet::status_from(info);
    let hd = info["hdmasterkeyid"].as_str().map(String::from);
    let (app_seed, seed_file_problem) = match seed::read_file(&seed::seed_path(app_dir)) {
        Ok(None) => (AppSeed::None, None),
        Ok(Some(bytes)) => match seed::file_key_id(&bytes) {
            Ok(id) if hd.as_deref() == Some(seed::key_id_hex(&id).as_str()) => (AppSeed::Matches, None),
            Ok(_) => (AppSeed::Other, None),
            Err(e) => (AppSeed::None, Some(e.for_ui())),
        },
        Err(e) => (AppSeed::None, Some(e)),
    };
    let r = Record::load(app_dir);
    let txcount = info["txcount"].as_u64().unwrap_or(0);
    let current = hd.is_some() && r.seed_id == hd;
    Protection {
        encrypted: st.encrypted,
        unlocked_until: st.unlocked_until,
        protected: st.encrypted && app_seed == AppSeed::Matches,
        words_confirmed: current && r.words_confirmed_at.is_some(),
        seed_set_at: if current { r.seed_set_at } else { None },
        backup_at: r.backup_at,
        backup_due: r.backup_at.is_none() || r.backup_seed_id != hd,
        hd_seed_id: hd,
        app_seed,
        txcount,
        new_wallet: txcount == 0,
        node_is_ours,
        seed_file_problem,
    }
}

/// What a paired phone hears when it asks for an address while addresses are held.
pub const WALLET_NOT_SET_UP: &str = "Your desktop wallet isn't set up yet. Finish setting it up on your desktop.";

/// Whether addresses are held: a new wallet (no transactions yet) that isn't protected. The same
/// state `wallet_protection` reports, and the screens' $holdAddresses (src/lib/walletSeed.ts): an
/// address handed out before the passphrase would come from the seed encryptwallet replaces.
/// `info` is getwalletinfo's answer.
pub fn addresses_held(info: &Value, app_dir: &Path) -> bool {
    let p = protection_from(info, app_dir, false);
    p.new_wallet && !p.protected
}

pub(crate) async fn protection(c: &mut FreeBankClient, mgr: &NodeManager) -> Result<Protection, String> {
    let info = c.call_ui("getwalletinfo", vec![]).await?;
    Ok(protection_from(&info, &mgr.app_dir, process::child_alive(mgr).await))
}

/// Settings > Wallet.
#[derive(Debug, Clone, Serialize)]
pub struct WalletInfo {
    #[serde(flatten)]
    pub protection: Protection,
    /// The wallet's file, when the node is the one on this computer.
    pub wallet_file: Option<String>,
    pub wallet_name: String,
    pub balance_sats: i64,
    pub unconfirmed_sats: i64,
    pub immature_sats: i64,
    pub keypool: u64,
    pub keypool_change: u64,
    /// Where FreeBank keeps its encrypted copy of the words.
    pub seed_file: String,
    pub backup_path: Option<String>,
    /// The BIP85 path the FreeBank wallet's HD seed comes from.
    pub bip85_path: &'static str,
}

pub(crate) async fn info(c: &mut FreeBankClient, mgr: &NodeManager) -> Result<WalletInfo, String> {
    let w = c.call_ui("getwalletinfo", vec![]).await?;
    let s = mgr.settings.lock().await.clone();
    let name = w["walletname"].as_str().unwrap_or("wallet.dat").to_string();
    let file = wallet_dir(Path::new(&s.datadir)).join(&name);
    Ok(WalletInfo {
        protection: protection_from(&w, &mgr.app_dir, process::child_alive(mgr).await),
        wallet_file: file.is_file().then(|| file.to_string_lossy().into_owned()),
        wallet_name: name,
        balance_sats: sats(&w["balance"]),
        unconfirmed_sats: sats(&w["unconfirmed_balance"]),
        immature_sats: sats(&w["immature_balance"]),
        keypool: w["keypoolsize"].as_u64().unwrap_or(0),
        keypool_change: w["keypoolsize_hd_internal"].as_u64().unwrap_or(0),
        seed_file: seed::seed_path(&mgr.app_dir).to_string_lossy().into_owned(),
        backup_path: Record::load(&mgr.app_dir).backup_path,
        bip85_path: seed::BIP85_PATH_TEXT,
    })
}
