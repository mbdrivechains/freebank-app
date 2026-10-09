//! Several wallets in one app (v0.2.6; both kinds, and the phone on the main wallet only),
//! with the FreeBank node v0.2.19's `createwallet` and `loadwallet` while it runs.
//!
//! Beside the main wallet (`wallet.dat`, made in Setup from the 24 words), two kinds:
//! - **From the words:** wallet number i of the same words, BIP85's HD-Seed WIF at m/83696968'/2'/i' (`seed::hd_seed_at`;
//!   the main wallet is index 0). Made with `createwallet words-<i>`, then `encryptwallet` with the wallet passphrase
//!   (freebankd, Core 0.16, stops after encrypting, and gives the wallet a new HD seed), started again, then `sethdseed`
//!   with the words' seed for i. So the words and the index bring it back, and it shares the passphrase.
//! - **A file:** a wallet file the owner already has (BitWindow's FreeBank, a backup, another node), copied into the
//!   node's wallet folder under a name of its own and opened with `loadwallet`. It keeps its own passphrase and needs
//!   its own backup.
//!
//! Once any of them is open, every wallet call must name its wallet: `rpc::set_main_wallet` makes the app's clients
//! for the local node name the main one, the screens' client names the wallet chosen in the header (`set_wallet`), and
//! the phone's always the main one. A wallet that isn't open after the node restarts is opened on its first call
//! (`rpc`). The node has no `unloadwallet`: a wallet removed from the list stays open until the node restarts, and its
//! file stays where it is (Obliterate lists it).

use crate::commands::ClientState;
use crate::node::{NodeManager, Settings};
use crate::rpc::{FreeBankClient, RpcError};
use crate::seed::{self, Chain};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use zeroize::Zeroizing;

/// The main wallet's name when the node's list doesn't say (Core's default).
pub const MAIN: &str = "wallet.dat";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtraWallet {
    /// Its name in the node: `words-<i>` or `file-<label>`.
    pub name: String,
    /// What the owner called it.
    pub label: String,
    /// "words" or "file".
    pub kind: String,
    /// Its BIP85 index, for a wallet from the words.
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WalletView {
    /// None: the main wallet.
    pub name: Option<String>,
    pub label: String,
    /// "main", "words" or "file".
    pub kind: String,
    /// sECX, as getbalance says; None when the node didn't answer.
    pub balance: Option<f64>,
    pub encrypted: Option<bool>,
    pub active: bool,
}

/// Make the app's clients follow the settings: name the main wallet once others are listed, and the screens' client the
/// chosen one. At start and after each change.
pub fn apply(s: &Settings, client: &mut FreeBankClient) {
    // A main wallet named once (another wallet was added, v0.2.6, or a hosted one made, v0.2.8) stays named: other
    // wallets may be open in the node.
    if s.extra_wallets.is_empty() && s.main_wallet.is_none() {
        crate::rpc::set_main_wallet(None);
        client.set_wallet(None);
        return;
    }
    crate::rpc::set_main_wallet(Some(s.main_wallet.clone().unwrap_or_else(|| MAIN.into())));
    let active = s.active_wallet.clone().filter(|a| s.extra_wallets.iter().any(|w| &w.name == a));
    client.set_wallet(active);
}

fn client_for(mgr: &NodeManager, s: &Settings, wallet: Option<&str>, timeout: Duration) -> Result<FreeBankClient, String> {
    let mut c = crate::recovery::local_client(mgr, s, timeout)?;
    c.set_wallet(wallet.map(String::from));
    Ok(c)
}

fn say(e: RpcError) -> String {
    e.for_ui()
}

/// A label as typed: 1 to 32 characters, no control characters.
fn clean_label(label: &str) -> Result<String, String> {
    let l: String = label.trim().chars().filter(|c| !c.is_control()).collect();
    if l.is_empty() || l.chars().count() > 32 {
        return Err("Give the wallet a name of 1 to 32 characters.".into());
    }
    Ok(l)
}

/// A file name for a wallet from its label: lowercase letters, digits and dashes, not one already taken.
fn file_name(label: &str, taken: &[String]) -> String {
    let mut slug: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug = slug.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    slug.truncate(20);
    if slug.is_empty() {
        slug = "wallet".into();
    }
    let mut name = format!("file-{}", slug);
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("file-{}-{}", slug, n);
        n += 1;
    }
    name
}

/// The next BIP85 index for a wallet from the words: one past the highest used (the main wallet is 0).
pub fn next_index(extras: &[ExtraWallet]) -> u32 {
    extras.iter().filter_map(|w| w.index).max().map_or(1, |i| i + 1)
}

/// The main wallet's name as the node lists it (its first wallet), else Core's default.
async fn main_name(mgr: &NodeManager, s: &Settings) -> String {
    let Ok(c) = client_for(mgr, s, None, Duration::from_secs(20)) else { return MAIN.into() };
    let first = c.call_root("listwallets", vec![]).await.ok().and_then(|v| v.get(0).and_then(Value::as_str).map(String::from));
    first.unwrap_or_else(|| MAIN.into())
}

async fn save(mgr: &NodeManager, client: &ClientState, s: Settings) -> Result<(), String> {
    mgr.save_settings(s.clone()).await?;
    apply(&s, &mut *client.lock().await);
    Ok(())
}

pub(crate) async fn views(mgr: &NodeManager, s: &Settings) -> Vec<WalletView> {
    let active = s.active_wallet.clone().filter(|a| s.extra_wallets.iter().any(|w| &w.name == a));
    let mut out = Vec::new();
    let mut entries: Vec<(Option<String>, String, String)> = vec![(None, "Main".into(), "main".into())];
    entries.extend(s.extra_wallets.iter().map(|w| (Some(w.name.clone()), w.label.clone(), w.kind.clone())));
    for (name, label, kind) in entries {
        let c = client_for(mgr, s, name.as_deref(), Duration::from_secs(20)).ok();
        let (balance, encrypted) = match &c {
            Some(c) => (
                c.call_typed("getbalance", vec![]).await.ok().and_then(|v| v.as_f64()),
                c.call_typed("getwalletinfo", vec![]).await.ok().map(|i| i.get("unlocked_until").is_some()),
            ),
            None => (None, None),
        };
        out.push(WalletView { active: name == active, name, label, kind, balance, encrypted });
    }
    out
}

#[tauri::command]
pub async fn wallets_list(mgr: State<'_, Arc<NodeManager>>) -> Result<Vec<WalletView>, String> {
    let s = mgr.settings.lock().await.clone();
    Ok(views(&mgr, &s).await)
}

/// Which wallet the screens use (None: the main one). The phone keeps the main one.
#[tauri::command]
pub async fn wallet_select(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    name: Option<String>,
) -> Result<(), String> {
    let mut s = mgr.settings.lock().await.clone();
    if let Some(n) = &name {
        if !s.extra_wallets.iter().any(|w| &w.name == n) {
            return Err("FreeBank doesn't know that wallet.".into());
        }
    }
    s.active_wallet = name;
    save(&mgr, &client, s).await
}

/// Take a wallet off the list. It stays open in the node until the node restarts, and its file stays.
#[tauri::command]
pub async fn wallet_forget(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    name: String,
) -> Result<Vec<WalletView>, String> {
    let mut s = mgr.settings.lock().await.clone();
    s.extra_wallets.retain(|w| w.name != name);
    if s.active_wallet.as_deref() == Some(name.as_str()) {
        s.active_wallet = None;
    }
    save(&mgr, &client, s.clone()).await?;
    Ok(views(&mgr, &s).await)
}

/// A new wallet from the saved words, at the next index: create, encrypt with the passphrase (the node stops and is
/// started again), then give it the words' seed for that index. Takes a minute or two.
#[tauri::command]
pub async fn wallet_add_words(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    label: String,
    passphrase: String,
) -> Result<Vec<WalletView>, String> {
    add_words(&mgr, &client, label, Zeroizing::new(passphrase)).await
}

pub(crate) async fn add_words(
    mgr: &NodeManager,
    client: &ClientState,
    label: String,
    pass: Zeroizing<String>,
) -> Result<Vec<WalletView>, String> {
    let label = clean_label(&label)?;
    let (entropy, _) = crate::recovery::ops::open_saved_blocking(&mgr.app_dir, &pass).await?;
    let mut s = mgr.settings.lock().await.clone();
    let index = next_index(&s.extra_wallets);
    let name = format!("words-{}", index);
    if s.extra_wallets.iter().any(|w| w.name == name) {
        return Err(format!("FreeBank already lists a wallet called {}.", name));
    }
    // Name the main wallet from now on: once this one opens, unnamed wallet calls are refused.
    if s.main_wallet.is_none() {
        s.main_wallet = Some(main_name(mgr, &s).await);
    }
    crate::rpc::set_main_wallet(s.main_wallet.clone());
    let root = client_for(mgr, &s, None, Duration::from_secs(60))?;
    match root.call_root("createwallet", vec![json!(name)]).await {
        Ok(_) => {}
        // A wallet file of that name left by an earlier try: open it and carry on.
        Err(RpcError::Rpc { code: -4, message }) if message.contains("already exists") => {
            let _ = root.call_root("loadwallet", vec![json!(name)]).await;
        }
        Err(e) => return Err(format!("The node couldn't make the wallet: {}", say(e))),
    }
    let mut w = client_for(mgr, &s, Some(&name), Duration::from_secs(600))?;
    let info = w.call_fresh_typed("getwalletinfo", vec![]).await.map_err(say)?;
    if info.get("unlocked_until").is_none() {
        let ours = crate::node::process::child_alive(mgr).await;
        crate::recovery::job::encrypt(&mut w, &pass).await?;
        crate::recovery::job::restart_after_encrypt(mgr, &s, ours, &Default::default()).await?;
        w = client_for(mgr, &s, Some(&name), Duration::from_secs(600))?;
    }
    // The words' seed for this index, unlocked for the moment it takes.
    let chain = Chain::from_name(
        w.call_fresh_typed("getblockchaininfo", vec![]).await.map_err(say)?["chain"].as_str().unwrap_or("main"),
    )?;
    let hd = seed::hd_seed_at(&entropy, index)?;
    let wif = seed::wif(&hd, chain);
    let want = seed::key_id_hex(&seed::key_id(&hd)?);
    let info = w.call_fresh_typed("getwalletinfo", vec![]).await.map_err(say)?;
    if info["hdmasterkeyid"].as_str() != Some(want.as_str()) {
        w.call_fresh_typed("walletpassphrase", vec![json!(pass.as_str()), json!(60)]).await.map_err(|e| match e {
            RpcError::Rpc { code: -14, .. } => "That passphrase doesn't open the new wallet.".to_string(),
            e => say(e),
        })?;
        let r = w.call_fresh_typed("sethdseed", vec![json!(true), json!(wif.as_str())]).await;
        let _ = w.call_fresh_typed("walletlock", vec![]).await;
        r.map_err(|e| format!("The new wallet didn't take its seed: {}", say(e)))?;
        let now = w.call_fresh_typed("getwalletinfo", vec![]).await.map_err(say)?;
        if now["hdmasterkeyid"].as_str() != Some(want.as_str()) {
            return Err("The new wallet didn't take the seed from your words. Nothing was sent to it.".into());
        }
    }
    drop(wif);
    let mut s2 = mgr.settings.lock().await.clone();
    s2.main_wallet = s.main_wallet.clone();
    s2.extra_wallets.push(ExtraWallet { name, label, kind: "words".into(), index: Some(index) });
    save(mgr, client, s2.clone()).await?;
    crate::activity::note("wallets: a wallet from the words added");
    Ok(views(mgr, &s2).await)
}

/// Open a wallet file the owner has: `data` is the file, base64. It is copied into the node's wallet folder (readable
/// by this user only) and opened; nothing is copied if the node can't open it.
#[tauri::command]
pub async fn wallet_add_file(
    mgr: State<'_, Arc<NodeManager>>,
    client: State<'_, ClientState>,
    label: String,
    data: String,
) -> Result<Vec<WalletView>, String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::io::Write;
    let label = clean_label(&label)?;
    let bytes = STANDARD.decode(data.trim()).map_err(|_| "That file couldn't be read.")?;
    // A Berkeley DB wallet file: its magic at byte 12 (0x00053162, either order).
    if bytes.len() < 4096 || !(bytes[12..16] == [0x62, 0x31, 0x05, 0x00] || bytes[12..16] == [0x00, 0x05, 0x31, 0x62]) {
        return Err("That isn't a FreeBank (Bitcoin Core 0.16) wallet file.".into());
    }
    let mut s = mgr.settings.lock().await.clone();
    let taken: Vec<String> = s.extra_wallets.iter().map(|w| w.name.clone()).collect();
    let name = file_name(&label, &taken);
    let dir = crate::recovery::wallet_dir(std::path::Path::new(&s.datadir));
    let path = dir.join(&name);
    if path.exists() {
        return Err(format!("{} already exists; FreeBank won't replace it.", path.display()));
    }
    {
        let mut f = crate::node::install::private_file(&path).map_err(|e| format!("Couldn't write {}: {}", path.display(), e))?;
        f.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    if s.main_wallet.is_none() {
        s.main_wallet = Some(main_name(&mgr, &s).await);
    }
    let root = client_for(&mgr, &s, None, Duration::from_secs(120))?;
    // Name the main wallet from now on: once this one opens, unnamed wallet calls are refused.
    crate::rpc::set_main_wallet(s.main_wallet.clone());
    let opened = root.call_root("loadwallet", vec![json!(name)]).await;
    if let Err(e) = opened {
        if s.extra_wallets.is_empty() {
            crate::rpc::set_main_wallet(None);
        }
        let _ = std::fs::remove_file(&path);
        return Err(format!("The node couldn't open that wallet: {}", say(e)));
    }
    s.extra_wallets.push(ExtraWallet { name, label, kind: "file".into(), index: None });
    save(&mgr, &client, s.clone()).await?;
    crate::activity::note("wallets: a wallet file added");
    Ok(views(&mgr, &s).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_indexes() {
        let w = |name: &str, index: Option<u32>| ExtraWallet { name: name.into(), label: "x".into(), kind: "words".into(), index };
        assert_eq!(next_index(&[]), 1);
        assert_eq!(next_index(&[w("words-1", Some(1)), w("file-a", None), w("words-3", Some(3))]), 4);
        assert_eq!(file_name("My Savings!", &[]), "file-my-savings");
        assert_eq!(file_name("My Savings!", &["file-my-savings".into()]), "file-my-savings-2");
        assert_eq!(file_name("ŧ", &[]), "file-wallet");
        assert!(clean_label("").is_err());
        assert!(clean_label(&"x".repeat(33)).is_err());
        assert_eq!(clean_label("  House\n ").unwrap(), "House");
    }

    #[test]
    fn the_clients_follow_the_settings() {
        let mut c = FreeBankClient::default();
        let mut s = Settings::default();
        s.active_wallet = Some("words-1".into());
        apply(&s, &mut c);
        // No other wallets listed: unnamed calls, as before.
        assert_eq!((crate::rpc::main_wallet(), c.wallet()), (None, None));
        s.extra_wallets.push(ExtraWallet { name: "words-1".into(), label: "Savings".into(), kind: "words".into(), index: Some(1) });
        apply(&s, &mut c);
        assert_eq!((crate::rpc::main_wallet().as_deref(), c.wallet()), (Some(MAIN), Some("words-1")));
        // An active wallet no longer listed falls back to the main one.
        s.active_wallet = Some("gone".into());
        apply(&s, &mut c);
        assert_eq!(c.wallet(), None);
        crate::rpc::set_main_wallet(None);
    }
}

/// Against a real node v0.2.19 with a second wallet open (ignored; FB_WALLETS_RPC=host:port of a node with
/// "wallet.dat" and "second" loaded, login t/t): unnamed wallet calls are refused once two are open; the app's
/// clients name the main wallet, or the one chosen; a wallet that can't be opened says so, once.
#[cfg(test)]
mod real {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn wallets_real_node() {
        let at = std::env::var("FB_WALLETS_RPC").expect("FB_WALLETS_RPC");
        let dir = std::env::temp_dir().join(format!("fb-wallets-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("freebank.conf"), "rpcuser=t\nrpcpassword=t\n").unwrap();
        let mut c = FreeBankClient::with_http(reqwest::Client::builder().no_proxy().build().unwrap());
        assert!(c.configure_local(&format!("http://{}", at), dir.clone()));
        let wallets = c.call_root("listwallets", vec![]).await.unwrap();
        assert!(wallets.as_array().unwrap().len() >= 2, "{wallets}");
        // Unnamed: refused, as the node does with two open.
        crate::rpc::set_main_wallet(None);
        assert!(matches!(c.call_typed("getwalletinfo", vec![]).await, Err(RpcError::Rpc { code: -19, .. })));
        // The main one named.
        crate::rpc::set_main_wallet(Some(MAIN.into()));
        assert_eq!(c.call_typed("getwalletinfo", vec![]).await.unwrap()["walletname"], MAIN);
        // The chosen one; the phone's calls still the main one.
        c.set_wallet(Some("second".into()));
        assert_eq!(c.call_typed("getwalletinfo", vec![]).await.unwrap()["walletname"], "second");
        assert_eq!(c.call_fresh_typed_main("getwalletinfo", vec![]).await.unwrap()["walletname"], MAIN);
        // A wallet the node doesn't have: it tries to open it once, and says why it couldn't.
        c.set_wallet(Some("no-such-wallet".into()));
        let e = c.call_typed("getbalance", vec![]).await.unwrap_err();
        assert!(matches!(e, RpcError::Rpc { code: -18, .. }), "{e:?}");
        crate::rpc::set_main_wallet(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Makes hosted wallets for the phone link (v0.2.8, `phone::hosted`): create the wallet, encrypt it, start the node
/// again. The main wallet is named from then on, as for a wallet from the words.
pub struct HostedMaker {
    pub mgr: Arc<NodeManager>,
}

impl crate::phone::hosted::Maker for HostedMaker {
    fn create_encrypted<'a>(&'a self, name: &'a str, pass: &'a str) -> crate::phone::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let mgr = &self.mgr;
            let mut s = mgr.settings.lock().await.clone();
            if s.main_wallet.is_none() {
                s.main_wallet = Some(main_name(mgr, &s).await);
                mgr.save_settings(s.clone()).await?;
            }
            crate::rpc::set_main_wallet(s.main_wallet.clone());
            let root = client_for(mgr, &s, None, Duration::from_secs(60))?;
            match root.call_root("createwallet", vec![json!(name)]).await {
                Ok(_) => {}
                // Made before an interruption: open it and carry on.
                Err(RpcError::Rpc { code: -4, message }) if message.contains("already exists") => {
                    let _ = root.call_root("loadwallet", vec![json!(name)]).await;
                }
                Err(e) => return Err(format!("The node couldn't make the wallet: {}", say(e))),
            }
            let mut w = client_for(mgr, &s, Some(name), Duration::from_secs(600))?;
            let info = w.call_fresh_typed("getwalletinfo", vec![]).await.map_err(say)?;
            if info.get("unlocked_until").is_none() {
                let ours = crate::node::process::child_alive(mgr).await;
                let pass = Zeroizing::new(pass.to_string());
                crate::recovery::job::encrypt(&mut w, &pass).await?;
                crate::recovery::job::restart_after_encrypt(mgr, &s, ours, &Default::default()).await?;
            }
            Ok(())
        })
    }
}
