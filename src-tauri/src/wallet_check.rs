//! Two checks in Settings › Wallet (v0.6.8), neither of which changes anything:
//! - "Is this address mine?": an eCash address (bc1…, tb1…) is looked for among the addresses this wallet's recovery
//!   words make for the app's eCash wallets, on the chain its form names; any other goes to the FreeBank node's wallets.
//! - "Check my words": typed words are this wallet's recovery words, or not.

use crate::commands::ClientState;
use crate::ecash::keys::{Account, Root};
use crate::node::NodeManager;
use crate::seed::{self, Chain};
use bitcoin::bip32::ChildNumber;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{CompressedPublicKey, ScriptBuf};
use serde::Serialize;
use serde_json::json;
use std::str::FromStr;
use std::sync::Arc;
use tauri::State;
use zeroize::Zeroizing;

/// How far each eCash account's receiving and change addresses are searched: years of Receive presses.
const SEARCH: u32 = 5_000;

/// An answer, in words for the screen.
#[derive(Debug, Serialize, PartialEq)]
pub struct Checked {
    pub yes: bool,
    pub said: String,
}

/// The eCash chain an address's form names, or None for a form the app's eCash wallets don't make.
fn ecash_chain(address: &str) -> Option<Chain> {
    let a = address.to_ascii_lowercase();
    if a.starts_with("bcrt1") {
        Some(Chain::Regtest)
    } else if a.starts_with("bc1") {
        Some(Chain::Main)
    } else if a.starts_with("tb1") {
        Some(Chain::Test)
    } else {
        None
    }
}

fn chain_words(chain: Chain) -> &'static str {
    match chain {
        Chain::Main => "on eCash beta and eCash mainnet (they share the form, and the keys)",
        Chain::Test => "on easynet",
        Chain::Regtest => "on regtest",
    }
}

/// Look for `address` among the addresses the words' eCash root makes, both accounts, both branches.
fn ecash_lookup(entropy: &[u8; 32], chain: Chain, address: &str) -> Result<Checked, String> {
    let want = bitcoin::Address::from_str(address.trim())
        .map_err(|_| "That isn't a valid address: check it for a typo.".to_string())?
        .assume_checked()
        .script_pubkey();
    // The accounts' public keys, then the root (a private key) goes before the long search.
    let accounts = {
        let root = Root::from_words(entropy, chain)?;
        [Account::Main, Account::Bids].map(|a| root.account(chain, a).map(|k| (a, k.public.xpub)))
    };
    let secp = Secp256k1::verification_only();
    for acct in accounts {
        let (account, xpub) = acct?;
        for branch in [0u32, 1] {
            let b = xpub.ckd_pub(&secp, ChildNumber::Normal { index: branch }).map_err(|e| e.to_string())?;
            for i in 0..SEARCH {
                let k = b.ckd_pub(&secp, ChildNumber::Normal { index: i }).map_err(|e| e.to_string())?;
                if ScriptBuf::new_p2wpkh(&CompressedPublicKey(k.public_key).wpubkey_hash()) == want {
                    return Ok(Checked { yes: true, said: found(chain, account, branch, i) });
                }
            }
        }
    }
    Ok(Checked {
        yes: false,
        said: format!(
            "No: it isn't among the first {} receiving and change addresses of your eCash wallet or your bidding wallet {}, \
             made from this wallet's recovery words.",
            fmt_count(SEARCH),
            chain_words(chain)
        ),
    })
}

fn found(chain: Chain, account: Account, branch: u32, index: u32) -> String {
    let wallet = match account {
        Account::Main => "your eCash wallet's",
        Account::Bids => "your bidding wallet's",
    };
    let kind = if branch == 0 { "receiving" } else { "change" };
    format!(
        "Yes: it is {wallet} {kind} address number {}, {}, made from this wallet's recovery words.",
        fmt_count(index + 1),
        chain_words(chain)
    )
}

fn fmt_count(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "Is this address mine?" An eCash address needs the wallet's passphrase (the words are sealed with it).
#[tauri::command]
pub async fn wallet_address_mine(
    mgr: State<'_, Arc<NodeManager>>,
    address: String,
    passphrase: Option<String>,
) -> Result<Checked, String> {
    let address = address.trim().to_string();
    if address.is_empty() {
        return Err("Paste an address first.".into());
    }
    let Some(chain) = ecash_chain(&address) else {
        return freebank_lookup(&mgr, &address).await;
    };
    let pass = Zeroizing::new(passphrase.unwrap_or_default());
    let (entropy, _) = crate::recovery::ops::open_saved_blocking(&mgr.app_dir, &pass).await?;
    tokio::task::spawn_blocking(move || ecash_lookup(&entropy, chain, &address)).await.map_err(|e| e.to_string())?
}

/// This wallet's FreeBank wallets (the main one and those added in Settings › Wallet; never the hosted ones the node
/// keeps for other people), each asked `getaddressinfo`.
async fn freebank_lookup(mgr: &NodeManager, address: &str) -> Result<Checked, String> {
    let s = mgr.settings.lock().await.clone();
    let mut wallets: Vec<(Option<String>, String)> = vec![(None, "main".into())];
    wallets.extend(s.extra_wallets.iter().map(|w| (Some(w.name.clone()), w.label.clone())));
    let several = wallets.len() > 1;
    for (name, label) in &wallets {
        let c = crate::wallets::client_for(mgr, &s, name.as_deref(), std::time::Duration::from_secs(20))?;
        let info = match c.call("getaddressinfo", vec![json!(address)]).await {
            Ok(i) => i,
            Err(e) if e.contains("Invalid address") || e.contains("Invalid or unsupported") => {
                return Err("That isn't a FreeBank or eCash address: check it for a typo.".into())
            }
            Err(e) => return Err(e),
        };
        if info["ismine"] == true {
            let which = if several { format!("your FreeBank wallet \"{label}\"") } else { "your FreeBank wallet".into() };
            return Ok(Checked { yes: true, said: format!("Yes: it is an address of {which}.") });
        }
    }
    Ok(Checked {
        yes: false,
        said: if several {
            "No: none of your FreeBank wallets on this node has it.".into()
        } else {
            "No: your FreeBank wallet doesn't have it.".into()
        },
    })
}

/// "Check my words": the typed words' wallet against FreeBank's sealed copy and the node's main wallet (read without the
/// passphrase). The words are wiped once looked at.
#[tauri::command]
pub async fn wallet_words_match(client: State<'_, ClientState>, mgr: State<'_, Arc<NodeManager>>, words: String) -> Result<Checked, String> {
    let words = Zeroizing::new(words);
    let Some(id) = seed::words_key_id_hex(&words)? else {
        return Ok(Checked { yes: false, said: "Those aren't 24 recovery words: check each word, and that there are 24.".into() });
    };
    let sealed = seed::sealed_key_id_hex(&mgr.app_dir);
    let node = {
        let shared = client.lock().await;
        let mut c = shared.for_main();
        c.call_ui("getwalletinfo", vec![]).await.ok().and_then(|i| i["hdmasterkeyid"].as_str().map(String::from))
    };
    Ok(words_answer(&id, sealed.as_deref(), node.as_deref()))
}

fn words_answer(id: &str, sealed: Option<&str>, node: Option<&str>) -> Checked {
    let (sealed_ok, node_ok) = (sealed == Some(id), node == Some(id));
    let said = if sealed.is_none() && node.is_none() {
        "FreeBank has nothing to check them against: it keeps no recovery words for this wallet, and the node didn't answer."
    } else if (sealed_ok || sealed.is_none()) && (node_ok || node.is_none()) {
        "Yes: these are this wallet's recovery words. Nothing was changed."
    } else if sealed_ok {
        "These are the words FreeBank keeps for this wallet, but the node's wallet no longer comes from them (its seed was \
         changed outside FreeBank). Nothing was changed."
    } else if node_ok {
        "These make the node's wallet, but the words FreeBank keeps for it are another wallet's. Nothing was changed."
    } else {
        "No: these words make another wallet, not this one. Nothing was changed. Compare them with what you wrote down, \
         word by word."
    };
    Checked { yes: said.starts_with("Yes"), said: said.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        assert_eq!(ecash_chain("bc1q47etdwjyr3huwvn4tmf3w9psnypcvndev39uq0"), Some(Chain::Main));
        assert_eq!(ecash_chain("BC1Q47ETDWJYR3HUWVN4TMF3W9PSNYPCVNDEV39UQ0"), Some(Chain::Main));
        assert_eq!(ecash_chain("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx"), Some(Chain::Test));
        assert_eq!(ecash_chain("bcrt1qxyz"), Some(Chain::Regtest));
        assert_eq!(ecash_chain("XVHQL58xFdgoYuufkHfCPLQH1fY1mjB4an"), None, "FreeBank's own: the node is asked");
        assert_eq!(fmt_count(5000), "5,000");
        assert_eq!(fmt_count(12), "12");
        assert_eq!(fmt_count(1_234_567), "1,234,567");
    }

    #[test]
    fn finds_the_wallets_own_addresses_and_no_others() {
        let entropy = [7u8; 32];
        for chain in [Chain::Main, Chain::Test] {
            let root = Root::from_words(&entropy, chain).unwrap();
            let main = root.account(chain, Account::Main).unwrap().public.clone();
            let bids = root.account(chain, Account::Bids).unwrap().public.clone();
            let r = ecash_lookup(&entropy, chain, &main.address(0, 0).unwrap()).unwrap();
            assert!(r.yes && r.said.contains("eCash wallet's receiving address number 1,"), "{}", r.said);
            let r = ecash_lookup(&entropy, chain, &main.address(1, 41).unwrap()).unwrap();
            assert!(r.yes && r.said.contains("change address number 42"), "{}", r.said);
            let r = ecash_lookup(&entropy, chain, &bids.address(0, 3).unwrap()).unwrap();
            assert!(r.yes && r.said.contains("bidding wallet's receiving address number 4"), "{}", r.said);
            let other = Root::from_words(&[8u8; 32], chain).unwrap().account(chain, Account::Main).unwrap().public.clone();
            let r = ecash_lookup(&entropy, chain, &other.address(0, 0).unwrap()).unwrap();
            assert!(!r.yes && r.said.starts_with("No:"), "{}", r.said);
        }
        // Beta's and mainnet's form, easynet's keys: another coin type, so not found.
        let test_main = Root::from_words(&entropy, Chain::Test).unwrap().account(Chain::Test, Account::Main).unwrap().public.clone();
        let as_bc1 = bitcoin::Address::p2wpkh(&test_main.pubkey(0, 0).unwrap(), bitcoin::KnownHrp::Mainnet).to_string();
        assert!(!ecash_lookup(&entropy, Chain::Main, &as_bc1).unwrap().yes);
        assert!(ecash_lookup(&entropy, Chain::Main, "bc1qnotanaddress").is_err());
    }

    #[test]
    fn the_words_answers() {
        assert!(words_answer("ab", Some("ab"), Some("ab")).yes);
        assert!(words_answer("ab", Some("ab"), None).yes, "the node didn't answer: FreeBank's copy decides");
        assert!(words_answer("ab", None, Some("ab")).yes, "no sealed copy: the node's seed decides");
        let r = words_answer("ab", Some("ab"), Some("cd"));
        assert!(!r.yes && r.said.contains("no longer comes from them"), "{}", r.said);
        let r = words_answer("ab", Some("cd"), Some("cd"));
        assert!(!r.yes && r.said.starts_with("No:"), "{}", r.said);
        let r = words_answer("ab", Some("cd"), Some("ab"));
        assert!(!r.yes && r.said.contains("another wallet's"), "the sealed copy is another wallet's: not a yes");
        assert!(!words_answer("ab", None, None).yes);
    }
}
