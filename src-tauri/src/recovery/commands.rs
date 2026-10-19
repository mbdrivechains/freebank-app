//! The Tauri commands for the wallet's passphrase, recovery words, backup and restore (v0.2.0).
//! Registered in lib.rs's invoke_handler, in the block marked `// v0.2.0 wallet`.

use super::job::{self, Job};
use super::ops;
use super::*;
use crate::commands::ClientState;
use crate::wallet::RelockState;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::{Zeroize, Zeroizing};

type Mgr = Arc<NodeManager>;

/// Whether the wallet has a passphrase and its HD seed comes from FreeBank's words (`protected`),
/// and what else the banner and the first-run flow need.
#[tauri::command]
pub async fn wallet_protection(client: State<'_, ClientState>, mgr: State<'_, Mgr>) -> Result<Protection, String> {
    // The main wallet, whichever the screens chose (v0.2.6, several wallets).
    let shared = client.lock().await;
    let mut c = shared.for_main();
    protection(&mut c, &mgr).await
}

/// Settings > Wallet.
#[tauri::command]
pub async fn wallet_info(client: State<'_, ClientState>, mgr: State<'_, Mgr>) -> Result<WalletInfo, String> {
    // Copies of backup files chosen earlier and never restored go now.
    ops::forget_uploads(&mgr.app_dir);
    // The main wallet, whichever the screens chose (v0.2.6, several wallets).
    let shared = client.lock().await;
    let mut c = shared.for_main();
    info(&mut c, &mgr).await
}

/// Start the passphrase-first setup: new words (`words` empty) or restored ones. On a wallet without
/// a passphrase, `passphrase` becomes its passphrase; on one that has a passphrase, it must be it.
/// `fresh`: move the current wallet aside first, so the words go into a new wallet.
#[tauri::command]
pub async fn wallet_setup_start(
    client: State<'_, ClientState>,
    mgr: State<'_, Mgr>,
    guard: State<'_, RelockState>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    passphrase: String,
    words: Option<String>,
    fresh: bool,
) -> Result<(), String> {
    let passphrase = Zeroizing::new(passphrase);
    let words = words.map(Zeroizing::new);
    if passphrase.is_empty() {
        return Err("Please enter a passphrase.".into());
    }
    let restore = match &words {
        Some(w) => Some(seed::parse_words(w)?),
        None => None,
    };
    setup_start(&client, &mgr, &guard, &phone, passphrase, restore, fresh, None).await
}

/// The other networks on this computer whose wallet has recovery words this app keeps (v0.5.0): (id, name), for "Use the
/// words of my Beta wallet" on easynet.
#[tauri::command]
pub fn wallet_words_elsewhere(mgr: State<'_, Mgr>) -> Vec<(String, String)> {
    let top = crate::network::top_of(&mgr.app_dir);
    let here = crate::network::current();
    crate::network::ALL
        .iter()
        .filter(|n| n.id != here.id)
        .filter(|n| seed::seed_path(&crate::network::app_dir(&top, n)).is_file())
        .map(|n| (n.id.to_string(), n.name.to_string()))
        .collect()
}

/// Set this wallet up with the recovery words of another network's wallet on this computer (v0.5.0): its sealed copy
/// opened with that wallet's passphrase (`from_passphrase`), then the words go in as a restore would put them, under that
/// same passphrase: the same words under a weaker one would be only as safe as it (review finding 8). `passphrase`, the
/// one chosen first, isn't used. The same words make the same FreeBank keys on each network; the networks share no
/// coins.
#[tauri::command]
pub async fn wallet_setup_from_network(
    client: State<'_, ClientState>,
    mgr: State<'_, Mgr>,
    guard: State<'_, RelockState>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    passphrase: String,
    from: String,
    from_passphrase: String,
) -> Result<(), String> {
    drop(Zeroizing::new(passphrase));
    let from_passphrase = Zeroizing::new(from_passphrase);
    if from_passphrase.is_empty() {
        return Err("Please enter that wallet's passphrase.".into());
    }
    let (entropy, n) = words_elsewhere(&mgr.app_dir, crate::network::current(), &from, &from_passphrase)?;
    let text = format!("Give this wallet the recovery words of your {} wallet", n.name);
    setup_start(&client, &mgr, &guard, &phone, from_passphrase, Some(entropy), false, Some(text)).await
}

/// The words sealed in network `from`'s app folder beside this one (`app_dir`, network `here`), opened with that
/// wallet's passphrase.
pub fn words_elsewhere(
    app_dir: &std::path::Path,
    here: &crate::network::Network,
    from: &str,
    passphrase: &str,
) -> Result<(seed::Entropy, &'static crate::network::Network), String> {
    let n = crate::network::by_id(from).filter(|n| n.id != here.id).ok_or("No such wallet on this computer.")?;
    let top = if here.id == crate::network::BETA.id {
        app_dir.to_path_buf()
    } else {
        app_dir.parent().and_then(std::path::Path::parent).map(std::path::Path::to_path_buf).unwrap_or_default()
    };
    let file = std::fs::read(seed::seed_path(&crate::network::app_dir(&top, n)))
        .map_err(|_| format!("Your {} wallet's recovery words aren't on this computer.", n.name))?;
    let (entropy, _) = seed::open(&file, passphrase).map_err(|e| match e {
        seed::OpenError::WrongPassphrase => format!("That isn't your {} wallet's passphrase: it doesn't open its recovery words.", n.name),
        e => format!("Your {} wallet's copy of its recovery words can't be read: {}", n.name, e.for_ui()),
    })?;
    Ok((entropy, n))
}

#[allow(clippy::too_many_arguments)]
async fn setup_start(
    client: &State<'_, ClientState>,
    mgr: &State<'_, Mgr>,
    guard: &State<'_, RelockState>,
    phone: &State<'_, crate::phone::commands::PhoneState>,
    passphrase: Zeroizing<String>,
    restore: Option<seed::Entropy>,
    fresh: bool,
    approve: Option<String>,
) -> Result<(), String> {
    mgr.still_here()?;
    if !fresh {
        let p = {
            // The main wallet, whichever the screens chose (v0.2.6, several wallets).
            let shared = client.lock().await;
            let mut c = shared.for_main();
            protection(&mut c, &mgr).await?
        };
        if p.protected {
            return Err("Your wallet already has its passphrase and recovery words.".into());
        }
    }
    // New words would answer "Lost your phone?" for an attacker's wallet in this one's place (security re-review M1).
    let text = approve.unwrap_or_else(|| {
        match (fresh, restore.is_some()) {
            (true, true) => "Put a wallet from recovery words in place of this one",
            (true, false) => "Make a new wallet in place of this one",
            (false, _) => "Give this wallet new recovery words",
        }
        .to_string()
    });
    crate::phone::commands::approve_change(phone, &mgr.app_dir, &text).await?;
    job::start(mgr.inner().clone(), guard.inner().clone(), Job::Setup { passphrase, restore, fresh })
}

#[tauri::command]
pub fn wallet_setup_progress() -> job::Progress {
    job::progress()
}

/// A new wallet's 24 words, once, right after the setup. Wiped here when sent.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct Words(Vec<String>);

impl Drop for Words {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[tauri::command]
pub fn wallet_setup_words() -> Option<Words> {
    job::take_words().map(|w| Words(w.to_vec()))
}

/// The user gave back the words they were asked for: record it for the wallet's current seed.
#[tauri::command]
pub async fn wallet_words_confirmed(client: State<'_, ClientState>, mgr: State<'_, Mgr>) -> Result<(), String> {
    mgr.still_here()?;
    let id = {
        // The main wallet, whichever the screens chose (v0.2.6, several wallets).
        let shared = client.lock().await;
        let mut c = shared.for_main();
        c.call_ui("getwalletinfo", vec![]).await?["hdmasterkeyid"].as_str().map(String::from)
    };
    let mut r = Record::load(&mgr.app_dir);
    if id.is_none() || r.seed_id != id {
        return Err("These aren't the words of your wallet's current seed.".into());
    }
    r.words_confirmed_at = Some(now());
    r.save(&mgr.app_dir)
}

/// Check typed words as they come: how many, which aren't recovery words, and whether all 24 fit.
#[tauri::command]
pub fn seed_check_words(words: String) -> seed::WordsCheck {
    let w = Zeroizing::new(words);
    seed::check_words(&w)
}

/// Show the recovery words ("words") or the FreeBank wallet's master xprv ("xprv"), after the
/// passphrase opens FreeBank's copy of the words. While "Approve sends on my phone" is on, a phone
/// approves it too: whoever sees them could take everything elsewhere (v0.2.5 security review H2).
#[tauri::command]
pub async fn wallet_reveal(
    client: State<'_, ClientState>,
    mgr: State<'_, Mgr>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    passphrase: String,
    what: String,
) -> Result<ops::Revealed, String> {
    let passphrase = Zeroizing::new(passphrase);
    let r = {
        // The main wallet, whichever the screens chose (v0.2.6, several wallets).
        let shared = client.lock().await;
        let mut c = shared.for_main();
        ops::reveal(&mgr.app_dir, &mut c, passphrase, &what).await?
    };
    if let Some(p) = phone.guard(&mgr.app_dir)?.filter(|p| p.approve_over().is_some()) {
        let text = if what == "xprv" {
            "Show this wallet's master key (xprv) on the desktop"
        } else {
            "Show this wallet's recovery words on the desktop"
        };
        p.request_approval(crate::phone::Approve::Change { text: text.into() }).await?;
    }
    Ok(r)
}

/// walletpassphrasechange, and FreeBank's copy of the words sealed again under the new passphrase.
#[tauri::command]
pub async fn wallet_change_passphrase(
    app: AppHandle,
    client: State<'_, ClientState>,
    mgr: State<'_, Mgr>,
    old: String,
    new: String,
) -> Result<ops::Changed, String> {
    let (old, new) = (Zeroizing::new(old), Zeroizing::new(new));
    mgr.still_here()?;
    let changed = {
        // The main wallet, whichever the screens chose (v0.2.6, several wallets).
        let shared = client.lock().await;
        let mut c = shared.for_main();
        ops::change_passphrase(&mgr.app_dir, &mut c, old, new).await?
    };
    passphrase_changed(&app);
    Ok(changed)
}

/// Called once the wallet's passphrase has changed: whatever holds the old one must forget it now.
/// The phone module keeps the passphrase in memory while "Let my phone send while FreeBank is open"
/// is on; it no longer opens the wallet, so it goes (as when that setting is turned off). The screen
/// hears "wallet-passphrase-changed" (src/lib/walletSeed.ts bumps `passphraseChanged`).
pub fn passphrase_changed(app: &AppHandle) {
    if let Some(p) = app.try_state::<crate::phone::commands::PhoneState>() {
        p.forget_passphrase();
    }
    let _ = app.emit("wallet-passphrase-changed", ());
}

/// Back up now, into Documents (or the home folder), and record when.
#[tauri::command]
pub async fn wallet_backup_now(app: AppHandle, client: State<'_, ClientState>, mgr: State<'_, Mgr>) -> Result<Vec<String>, String> {
    let path = app.path();
    let folder = path
        .document_dir()
        .ok()
        .filter(|d| d.is_dir())
        .or_else(|| path.home_dir().ok())
        .ok_or("FreeBank couldn't find your Documents folder or your home folder.")?;
    // The main wallet, whichever the screens chose (v0.2.6, several wallets).
    let shared = client.lock().await;
    let mut c = shared.for_main();
    ops::backup(&mgr, &mut c, &folder).await
}

/// "Restore from a backup file": the screen sends the chosen file's bytes, base64 in `data` (plain
/// JSON works on every IPC path). They are checked to be a wallet and kept (0600) until
/// `wallet_restore_file_start`.
#[tauri::command]
pub async fn wallet_restore_file_check(mgr: State<'_, Mgr>, data: String) -> Result<ops::BackupFile, String> {
    use base64::Engine;
    mgr.still_here()?;
    if data.len() > ops::MAX_BACKUP / 3 * 4 + 4 {
        return Err("That file is too big to be a FreeBank wallet backup.".into());
    }
    let dir = mgr.app_dir.clone();
    tokio::task::spawn_blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data.as_bytes())
            .map_err(|_| "Choose the backup file again.".to_string())?;
        ops::take_upload(&dir, &bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Put the checked backup in the wallet's place: stop the node, move the current wallet aside, copy
/// the backup in, start, scan the chain. Refused while another program runs the node.
#[tauri::command]
pub async fn wallet_restore_file_start(
    mgr: State<'_, Mgr>,
    guard: State<'_, RelockState>,
    phone: State<'_, crate::phone::commands::PhoneState>,
    token: String,
) -> Result<(), String> {
    mgr.still_here()?;
    crate::phone::commands::approve_change(&phone, &mgr.app_dir, "Put a wallet from a backup file in place of this one").await?;
    let upload = ops::claim_upload(&token)?;
    job::start(mgr.inner().clone(), guard.inner().clone(), Job::RestoreFile { upload })
}

/// The coins that "Move my coins to the new words" would move.
#[tauri::command]
pub async fn wallet_move_plan(client: State<'_, ClientState>) -> Result<ops::MovePlan, String> {
    // The main wallet, whichever the screens chose (v0.2.6, several wallets).
    let shared = client.lock().await;
    let mut c = shared.for_main();
    ops::move_plan(&mut c).await
}

/// Move them: one transaction to a new address of this wallet, the fee taken from the amount. The
/// screen runs it inside withUnlock.
#[tauri::command]
pub async fn wallet_move_coins(client: State<'_, ClientState>) -> Result<ops::Moved, String> {
    // The main wallet, whichever the screens chose (v0.2.6, several wallets).
    let shared = client.lock().await;
    let mut c = shared.for_main();
    ops::move_coins(&mut c).await
}

#[cfg(test)]
mod elsewhere_tests {
    use super::*;

    #[test]
    fn another_networks_words_open_with_its_passphrase() {
        let top = std::env::temp_dir().join(format!("fbelse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        let entropy = seed::new_entropy();
        let hd = seed::freebank_hd_seed(&entropy).unwrap();
        let file = seed::seal(&entropy, &seed::key_id(&hd).unwrap(), "beta pass", seed::Kdf { m_kib: 64, t: 1, p: 1 }).unwrap();
        seed::private_dir(&top.join("wallet")).unwrap();
        std::fs::write(seed::seed_path(&top), file).unwrap();
        let easy = crate::network::app_dir(&top, &crate::network::EASYNET);
        let (got, n) = words_elsewhere(&easy, &crate::network::EASYNET, "beta", "beta pass").unwrap();
        assert_eq!(&*got, &*entropy);
        assert_eq!(n.id, "beta");
        assert!(words_elsewhere(&easy, &crate::network::EASYNET, "beta", "wrong").unwrap_err().contains("isn't your Beta wallet's passphrase"));
        assert!(words_elsewhere(&top, &crate::network::BETA, "beta", "beta pass").is_err(), "not its own network");
        assert!(words_elsewhere(&top, &crate::network::BETA, "easynet", "x").unwrap_err().contains("aren't on this computer"));
        std::fs::remove_dir_all(&top).unwrap();
    }
}
