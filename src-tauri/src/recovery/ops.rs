//! The quick wallet actions: show the words or the xprv, change the passphrase, back up, look at a
//! backup file before restoring it, and move coins onto the new words' addresses.

use super::*;
use crate::seed::{Chain, OpenError};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use zeroize::{Zeroize, Zeroizing};

const NO_WORDS_YET: &str = "FreeBank has no recovery words saved for this wallet yet. Set them up first.";

/// Open the app's seed file. If it doesn't open with `pass` but a `seed.enc.new` beside it does (a
/// passphrase change stopped half way), that one is finished and used.
pub(crate) fn open_saved(app_dir: &Path, pass: &str) -> Result<(seed::Entropy, [u8; 20]), String> {
    let path = seed::seed_path(app_dir);
    let pending = path.with_file_name("seed.enc.new");
    let bytes = match seed::read_file(&path)? {
        Some(b) => b,
        None => return Err(NO_WORDS_YET.into()),
    };
    match seed::open(&bytes, pass) {
        Ok(v) => Ok(v),
        Err(OpenError::WrongPassphrase) => {
            let Some(newer) = seed::read_file(&pending)? else {
                return Err(OpenError::WrongPassphrase.for_ui());
            };
            let v = seed::open(&newer, pass).map_err(|_| OpenError::WrongPassphrase.for_ui())?;
            std::fs::rename(&pending, &path).map_err(|e| format!("Couldn't finish updating {}: {}", path.display(), e))?;
            Ok(v)
        }
        Err(e) => Err(e.for_ui()),
    }
}

async fn open_saved_blocking(app_dir: &Path, pass: &Zeroizing<String>) -> Result<(seed::Entropy, [u8; 20]), String> {
    let (dir, p) = (app_dir.to_path_buf(), pass.clone());
    tokio::task::spawn_blocking(move || open_saved(&dir, &p)).await.map_err(|e| e.to_string())?
}

// ---- Show the words or the xprv ----

/// What "Show recovery words" and "Show xprv" hand the screen. Wiped when dropped (after it is sent).
#[derive(Debug, Default, Serialize)]
pub struct Revealed {
    pub words: Option<Vec<String>>,
    pub xprv: Option<String>,
    /// Whether the words give the HD seed the node's wallet has now (None when the node didn't answer).
    pub matches_wallet: Option<bool>,
}

impl Drop for Revealed {
    fn drop(&mut self) {
        self.words.zeroize();
        self.xprv.zeroize();
    }
}

pub(crate) async fn reveal(app_dir: &Path, c: &mut FreeBankClient, pass: Zeroizing<String>, what: &str) -> Result<Revealed, String> {
    if what != "words" && what != "xprv" {
        return Err("FreeBank can show the recovery words or the xprv.".into());
    }
    if pass.is_empty() {
        return Err("Please enter your wallet passphrase.".into());
    }
    let (entropy, id) = open_saved_blocking(app_dir, &pass).await?;
    let info = c.call_ui("getwalletinfo", vec![]).await.ok();
    let mut out = Revealed::default();
    out.matches_wallet = info.map(|i| i["hdmasterkeyid"].as_str() == Some(seed::key_id_hex(&id).as_str()));
    if what == "words" {
        out.words = Some(seed::words(&entropy).to_vec());
    } else {
        let chain = match c.call_ui("getblockchaininfo", vec![]).await {
            Ok(b) => Chain::from_name(b["chain"].as_str().unwrap_or("main")).unwrap_or(Chain::Main),
            Err(_) => Chain::Main,
        };
        let hd = seed::freebank_hd_seed(&entropy)?;
        out.xprv = Some(seed::master_xprv(&hd, chain)?.to_string());
    }
    Ok(out)
}

// ---- Change the passphrase ----

#[derive(Debug, Serialize, PartialEq)]
pub struct Changed {
    /// "updated": FreeBank's copy of the words now opens with the new passphrase. "none": FreeBank had
    /// no words saved. "kept": the copy couldn't be updated (see `note`); the words haven't changed.
    pub seed_file: &'static str,
    pub note: Option<String>,
}

/// walletpassphrasechange, then FreeBank's copy of the words sealed again under the new passphrase.
/// The new copy is written beside the old one first (seed.enc.new) and swapped in only once the node
/// has the new passphrase; if the swap fails, both stay, and `open_saved` finishes it later.
pub(crate) async fn change_passphrase(
    app_dir: &Path,
    c: &mut FreeBankClient,
    old: Zeroizing<String>,
    new: Zeroizing<String>,
) -> Result<Changed, String> {
    if old.is_empty() {
        return Err("Please enter your current passphrase.".into());
    }
    if new.is_empty() {
        return Err("Please choose a new passphrase.".into());
    }
    if *old == *new {
        return Err("The new passphrase is the same as the current one.".into());
    }
    let info = c.call_ui("getwalletinfo", vec![]).await?;
    if info.get("unlocked_until").is_none() {
        return Err("Your wallet has no passphrase yet. Protect it first.".into());
    }
    let path = seed::seed_path(app_dir);
    let pending_path = path.with_file_name("seed.enc.new");
    let mut note = None;
    let mut sealed = None;
    let had_file = seed::read_file(&path)?.is_some();
    if had_file {
        match open_saved_blocking(app_dir, &old).await {
            Ok((entropy, id)) => {
                let n = new.clone();
                sealed = Some(
                    tokio::task::spawn_blocking(move || seed::seal(&entropy, &id, &n, seed::KDF))
                        .await
                        .map_err(|e| e.to_string())??,
                );
            }
            Err(_) => {
                // A typo, or a copy saved under another passphrase: walletpassphrasechange below
                // tells which (it refuses a wrong one). The copy stays as it is either way.
                note = Some(
                    "FreeBank's copy of your recovery words doesn't open with your current passphrase (it was saved \
                     under a different one, or it can't be read), so it stays as it was."
                        .to_string(),
                );
            }
        }
    }
    if let Some(bytes) = &sealed {
        seed::write_private(&pending_path, bytes)?;
    }
    if let Err(e) = c.call_fresh_typed("walletpassphrasechange", vec![json!(old.as_str()), json!(new.as_str())]).await {
        let _ = std::fs::remove_file(&pending_path);
        return Err(if is_code(&e, RPC_WALLET_PASSPHRASE_INCORRECT) {
            "That isn't your current passphrase.".into()
        } else {
            e.for_ui()
        });
    }
    if sealed.is_none() {
        return Ok(Changed { seed_file: if had_file { "kept" } else { "none" }, note });
    }
    if let Err(e) = std::fs::rename(&pending_path, &path) {
        return Ok(Changed {
            seed_file: "kept",
            note: Some(format!(
                "Your wallet now opens with the new passphrase. FreeBank couldn't finish updating its copy of your \
                 recovery words ({}): {} still opens with your old passphrase and {} with the new one, and FreeBank \
                 finishes the swap the next time it opens them. Your recovery words themselves haven't changed.",
                e,
                path.display(),
                pending_path.display()
            )),
        });
    }
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(Changed { seed_file: "updated", note })
}

// ---- Back up ----

/// Back the wallet up into `folder` (the node writes it with backupwallet, or its file is copied when
/// it is stopped), and record when, and for which HD seed.
pub(crate) async fn backup(mgr: &NodeManager, c: &mut FreeBankClient, folder: &Path) -> Result<Vec<String>, String> {
    let saved = crate::node::obliterate::backup_wallet(mgr, folder).await?;
    // Listed in <app data>/backups.json, as the node screens' "Back up wallet" does, so Settings >
    // Security checks it in later sessions too.
    crate::security::record_backups(mgr, &saved);
    let seed_id = c
        .call_ui("getwalletinfo", vec![])
        .await
        .ok()
        .and_then(|i| i["hdmasterkeyid"].as_str().map(String::from));
    if mgr.still_here().is_ok() {
        let mut r = Record::load(&mgr.app_dir);
        r.backup_at = Some(now());
        r.backup_seed_id = seed_id;
        r.backup_path = saved.first().cloned();
        r.save(&mgr.app_dir)?;
    }
    Ok(saved)
}

// ---- A backup file, before restoring it ----

/// What the screen shows about a backup file before "Restore this backup".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BackupFile {
    /// Names this upload for `wallet_restore_file_start`.
    pub token: String,
    pub size: u64,
    /// Has a passphrase (None: can't tell).
    pub encrypted: Option<bool>,
    /// An HD wallet (it has an HD chain record).
    pub hd: bool,
}

/// The largest file taken as a wallet backup (the screen checks it before reading the file too).
pub(crate) const MAX_BACKUP: usize = 64 << 20;

/// Is it a wallet, and what can be told from outside? A wallet is a Berkeley DB btree file (the magic
/// 0x00053162 at byte 12 of its first 4 KiB page, as Core's IsBerkeleyBtree checks). Its records are
/// keyed by type names: "mkey" (the encrypted master key) is only in wallets with a passphrase, and
/// "key" (a key in the clear) only in wallets without one; "hdchain" marks an HD wallet.
pub(crate) fn inspect(bytes: &[u8]) -> Result<(Option<bool>, bool), String> {
    if bytes.len() < 4096 {
        return Err("That file isn't a wallet backup: it's too small.".into());
    }
    let magic = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    if magic != 0x0005_3162 && magic != 0x6231_0500 {
        return Err("That file isn't a wallet backup.".into());
    }
    let has = |pat: &[u8]| bytes.windows(pat.len()).any(|w| w == pat);
    let encrypted = if has(b"\x04mkey") {
        Some(true)
    } else if has(b"\x03key") {
        Some(false)
    } else {
        None
    };
    Ok((encrypted, has(b"\x07hdchain")))
}

/// The one backup file waiting for "Restore this backup": its token and where the screen's copy is.
static UPLOAD: LazyLock<Mutex<Option<(String, PathBuf)>>> = LazyLock::new(Default::default);

pub(crate) fn upload_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("restore")
}

/// Remove the screen's copies of backup files, except the one waiting to be restored: a wallet
/// copy (maybe without a passphrase) shouldn't linger in the app's folder.
pub(crate) fn forget_uploads(app_dir: &Path) {
    let keep = UPLOAD.lock().unwrap().as_ref().map(|(_, p)| p.clone());
    for entry in std::fs::read_dir(upload_dir(app_dir)).into_iter().flatten().flatten() {
        let p = entry.path();
        if Some(&p) != keep.as_ref() && p.is_file() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Keep the screen's copy of a chosen backup file (0600, in the app's folder) after checking it is a
/// wallet. A newer upload replaces an older one.
pub(crate) fn take_upload(app_dir: &Path, bytes: &[u8]) -> Result<BackupFile, String> {
    if bytes.len() > MAX_BACKUP {
        return Err("That file is too big to be a FreeBank wallet backup.".into());
    }
    let (encrypted, hd) = inspect(bytes)?;
    let dir = upload_dir(app_dir);
    seed::private_dir(&dir)?;
    *UPLOAD.lock().unwrap() = None;
    forget_uploads(app_dir);
    let token = format!("{:016x}", rand::random::<u64>());
    let path = dir.join(format!("upload-{}.dat", token));
    seed::write_private(&path, bytes)?;
    *UPLOAD.lock().unwrap() = Some((token.clone(), path));
    Ok(BackupFile { token, size: bytes.len() as u64, encrypted, hd })
}

/// The upload named by `token`, handed over once.
pub(crate) fn claim_upload(token: &str) -> Result<PathBuf, String> {
    let mut u = UPLOAD.lock().unwrap();
    match u.take() {
        Some((t, path)) if t == token && path.is_file() => Ok(path),
        other => {
            *u = other;
            Err("Choose the backup file again.".into())
        }
    }
}

// ---- Move coins onto the new words ----

/// Coins the wallet holds on addresses the current HD seed doesn't cover (an older seed's, or keys
/// imported by hand). Notes, bills, term deposits and pool shares are never among them: freebankd's
/// listunspent leaves them out (AvailableCoins skips them), so they can't be swept as plain ECX.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MovePlan {
    pub coins: usize,
    pub total_sats: i64,
    /// Coins beyond one transaction's worth, left for a second move.
    pub later: usize,
    /// The wallet holds notes (they stay where they are).
    pub has_notes: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Moved {
    pub txid: String,
    pub coins: usize,
    /// What arrived at the new address, and the fee.
    pub sent_sats: i64,
    pub fee_sats: i64,
    pub to: String,
    pub later: usize,
}

/// At most this many coins in one move (a standard transaction stays under 100 kvB).
const MOVE_MAX_COINS: usize = 500;

struct OldCoins {
    picked: Vec<Value>,
    total: i64,
    later: usize,
}

async fn old_coins(c: &mut FreeBankClient) -> Result<(OldCoins, Option<String>), String> {
    let current = c.call_ui("getwalletinfo", vec![]).await?["hdmasterkeyid"].as_str().map(String::from);
    let list = c
        .call_ui("listunspent", vec![json!(0), json!(9_999_999), json!([]), json!(false)])
        .await?;
    let coins: Vec<Value> = list
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|u| u["spendable"] == json!(true))
        .collect();
    let mut seed_of: HashMap<String, Option<String>> = HashMap::new();
    let mut old = Vec::new();
    for u in coins {
        let Some(addr) = u["address"].as_str().map(String::from) else { continue };
        if !seed_of.contains_key(&addr) {
            let a = c.call_ui("getaddressinfo", vec![json!(addr)]).await?;
            seed_of.insert(addr.clone(), a["hdmasterkeyid"].as_str().map(String::from));
        }
        if current.is_none() || seed_of[&addr] != current {
            old.push(u);
        }
    }
    // The biggest first, so a move that has to stop at the limit carries the most.
    old.sort_by_key(|u| std::cmp::Reverse(sats(&u["amount"])));
    let later = old.len().saturating_sub(MOVE_MAX_COINS);
    old.truncate(MOVE_MAX_COINS);
    let total = old.iter().map(|u| sats(&u["amount"])).sum();
    Ok((OldCoins { picked: old, total, later }, current))
}

pub(crate) async fn move_plan(c: &mut FreeBankClient) -> Result<MovePlan, String> {
    let (old, _) = old_coins(c).await?;
    let has_notes = c
        .call_ui("listmynotes", vec![])
        .await
        .ok()
        .and_then(|v| v.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false);
    Ok(MovePlan { coins: old.picked.len(), total_sats: old.total, later: old.later, has_notes })
}

/// The fee rate for a move, in ECX per kvB as the node takes it: 1 sat/vB (the wallet's minimum),
/// unless more than a block's worth is waiting, then the node's estimate for the next blocks.
async fn move_fee_rate(c: &mut FreeBankClient) -> String {
    const MIN: i64 = 1_000; // sat per kvB
    let waiting = c
        .call_ui("getmempoolinfo", vec![])
        .await
        .ok()
        .and_then(|m| m["bytes"].as_u64())
        .unwrap_or(0);
    // MAX_BLOCK_WEIGHT 4,000,000 is a million virtual bytes.
    if waiting <= 1_000_000 {
        return ecx(MIN);
    }
    let estimate = c
        .call_ui("estimatesmartfee", vec![json!(2)])
        .await
        .ok()
        .and_then(|e| e["feerate"].as_f64())
        .map(|f| (f * 1e8).round() as i64);
    ecx(estimate.unwrap_or(2 * MIN).max(MIN))
}

/// Spend every coin not on the current seed's addresses to one new address of this wallet, the fee
/// taken from the amount. Runs inside the screen's withUnlock: a locked wallet answers -13 before an
/// address is made, and the screen unlocks and asks again.
pub(crate) async fn move_coins(c: &mut FreeBankClient) -> Result<Moved, String> {
    let info = c.call_ui("getwalletinfo", vec![]).await?;
    if info["unlocked_until"].as_u64() == Some(0) {
        return Err(format!(
            "RPC error {}: Error: Please enter the wallet passphrase with walletpassphrase first.",
            RPC_WALLET_UNLOCK_NEEDED
        ));
    }
    let (old, current) = old_coins(c).await?;
    if old.picked.is_empty() {
        return Err("Nothing to move: all your coins are on addresses your recovery words cover.".into());
    }
    let to = c.call_ui("getnewaddress", vec![json!(""), json!("legacy")]).await?;
    let to = to.as_str().ok_or("Your node didn't give a new address.")?.to_string();
    let a = c.call_ui("getaddressinfo", vec![json!(to)]).await?;
    if current.is_none() || a["hdmasterkeyid"].as_str().map(String::from) != current {
        return Err("Your node's new address isn't from your recovery words' seed, so nothing was moved.".into());
    }
    let inputs: Vec<Value> = old.picked.iter().map(|u| json!({"txid": u["txid"], "vout": u["vout"]})).collect();
    let raw = c
        .call_ui("createrawtransaction", vec![json!(inputs), json!({ to.clone(): ecx(old.total) })])
        .await?;
    let rate = move_fee_rate(c).await;
    let funded = c
        .call_ui(
            "fundrawtransaction",
            vec![raw, json!({"subtractFeeFromOutputs": [0], "feeRate": rate})],
        )
        .await?;
    let hex = funded["hex"].as_str().ok_or("Your node didn't build the transaction.")?.to_string();
    let fee = sats(&funded["fee"]);
    // Only the chosen coins go in, and everything but the fee comes out at the new address.
    let tx = c.call_ui("decoderawtransaction", vec![json!(hex)]).await?;
    let vin: HashSet<(String, u64)> = tx["vin"]
        .as_array()
        .map(|v| v.iter().map(|i| (i["txid"].as_str().unwrap_or("").to_string(), i["vout"].as_u64().unwrap_or(0))).collect())
        .unwrap_or_default();
    let chosen: HashSet<(String, u64)> = old
        .picked
        .iter()
        .map(|u| (u["txid"].as_str().unwrap_or("").to_string(), u["vout"].as_u64().unwrap_or(0)))
        .collect();
    let vout = tx["vout"].as_array().cloned().unwrap_or_default();
    let pays_to = vout.first().map(|o| o["scriptPubKey"]["addresses"] == json!([to]));
    if vin != chosen || vout.len() != 1 || pays_to != Some(true) || sats(&vout[0]["value"]) != old.total - fee {
        return Err("Your node built a different transaction than FreeBank asked for, so nothing was sent.".into());
    }
    if fee <= 0 || fee * 10 > old.total {
        return Err(format!(
            "The fee would be {} ECX, more than a tenth of what would move, so nothing was sent.",
            ecx(fee)
        ));
    }
    let signed = c.call_ui("signrawtransactionwithwallet", vec![json!(hex)]).await?;
    if signed["complete"] != json!(true) {
        return Err("Your wallet couldn't sign for every coin, so nothing was sent.".into());
    }
    let txid = c.call_ui("sendrawtransaction", vec![signed["hex"].clone()]).await?;
    Ok(Moved {
        txid: txid.as_str().unwrap_or("").to_string(),
        coins: old.picked.len(),
        sent_sats: old.total - fee,
        fee_sats: fee,
        to,
        later: old.later,
    })
}
