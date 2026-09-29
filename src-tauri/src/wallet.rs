//! The wallet's lock. v0.2.0 makes the wallet passphrase required, so every signing action meets a
//! locked wallet: the screen's `withUnlock` (src/lib/wallet.ts) catches the node's -13, asks for the
//! passphrase, unlocks for a few seconds, retries once and locks again.
//!
//! The passphrase goes to the node and is dropped. It is never logged, stored, or put in an error.

use crate::commands::ClientState;
use crate::rpc::{FreeBankClient, RpcError};
use serde::Serialize;
use serde_json::{json, Value};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

/// The longest unlock the app asks for.
pub const MAX_UNLOCK_SECONDS: i64 = 300;

/// How far an unlock keeps from the moment the node relocks after the app's previous one.
pub const RELOCK_MARGIN: Duration = Duration::from_secs(2);

/// freebankd's wallet (Bitcoin Core 0.16's) deadlocks for good if `walletpassphrase` runs at the
/// moment an earlier unlock's relock timer fires: the call holds the wallet lock while it replaces
/// that timer, which waits for the timer's callback, which waits for the wallet lock (fixed upstream
/// in Core 0.20). `walletlock` doesn't cancel the timer. The app keeps one `RelockGuard`, shared by
/// the screens and the phone link, and every `walletpassphrase` it makes goes through it: unlocks run
/// one at a time, and one waits only when the last relock is due within RELOCK_MARGIN, until that
/// has run. Replacing a timer well before it fires is safe.
#[derive(Default)]
pub struct RelockGuard {
    /// When the node relocks after the last `walletpassphrase`: between the call's start and end,
    /// plus its timeout.
    window: tokio::sync::Mutex<Option<(tokio::time::Instant, tokio::time::Instant)>>,
}

impl RelockGuard {
    /// Run `call`, a `walletpassphrase` for `secs`, clear of the last relock. `may_have_set` says
    /// whether a failed call may still have set a timer: an unanswered one may, a refusal doesn't.
    pub async fn run<T, E, Fut>(
        &self,
        secs: u64,
        call: impl FnOnce() -> Fut,
        may_have_set: impl Fn(&E) -> bool,
    ) -> Result<T, E>
    where
        Fut: Future<Output = Result<T, E>>,
    {
        let mut window = self.window.lock().await;
        if let Some((first, last)) = *window {
            let now = tokio::time::Instant::now();
            if now + RELOCK_MARGIN >= first && now < last + RELOCK_MARGIN {
                tokio::time::sleep_until(last + RELOCK_MARGIN).await;
            }
        }
        let start = tokio::time::Instant::now();
        let r = call().await;
        if r.as_ref().map_or_else(|e| may_have_set(e), |_| true) {
            let t = Duration::from_secs(secs);
            *window = Some((start + t, tokio::time::Instant::now() + t));
        }
        r
    }
}

/// The app's one guard, as Tauri state.
pub type RelockState = Arc<RelockGuard>;

/// Core's "the wallet has no passphrase" (walletpassphrase or walletlock on an unencrypted wallet).
const RPC_WALLET_WRONG_ENC_STATE: i64 = -15;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WalletStatus {
    /// The wallet has a passphrase.
    pub encrypted: bool,
    /// Unix time, on the node's clock, when the wallet locks again; 0 while locked. Always 0 for a
    /// wallet without a passphrase (check `encrypted` first).
    pub unlocked_until: u64,
}

/// getwalletinfo's answer: Core only reports `unlocked_until` for an encrypted wallet, and sets it
/// to 0 whenever the wallet locks (walletlock, or the timer walletpassphrase started).
pub fn status_from(info: &Value) -> WalletStatus {
    match info.get("unlocked_until") {
        Some(u) => WalletStatus { encrypted: true, unlocked_until: u.as_i64().unwrap_or(0).max(0) as u64 },
        None => WalletStatus { encrypted: false, unlocked_until: 0 },
    }
}

pub fn clamp_seconds(seconds: i64) -> i64 {
    seconds.clamp(1, MAX_UNLOCK_SECONDS)
}

pub async fn status(c: &mut FreeBankClient) -> Result<WalletStatus, String> {
    Ok(status_from(&c.call_ui("getwalletinfo", vec![]).await?))
}

/// walletpassphrase for `seconds` (clamped to 1..=300). A wrong passphrase comes back as the node's
/// "RPC error -14: …".
pub async fn unlock(
    c: &mut FreeBankClient,
    guard: &RelockGuard,
    passphrase: String,
    seconds: i64,
) -> Result<WalletStatus, String> {
    if passphrase.is_empty() {
        // Core answers an empty one with its help text, not an error code.
        return Err("Please enter your wallet passphrase.".into());
    }
    let secs = clamp_seconds(seconds);
    guard
        .run(
            secs as u64,
            || c.call_fresh_typed("walletpassphrase", vec![json!(passphrase), json!(secs)]),
            // The node's own refusal (a wrong passphrase) sets no timer; anything else may have.
            |e: &RpcError| !matches!(e, RpcError::Rpc { .. }),
        )
        .await
        .map_err(|e| e.for_ui())?;
    status(c).await
}

/// walletlock. A wallet without a passphrase has nothing to lock, and that isn't an error.
pub async fn lock(c: &mut FreeBankClient) -> Result<WalletStatus, String> {
    match c.call_fresh_typed("walletlock", vec![]).await {
        Ok(_) => {}
        Err(crate::rpc::RpcError::Rpc { code: RPC_WALLET_WRONG_ENC_STATE, .. }) => {}
        Err(e) => return Err(e.for_ui()),
    }
    status(c).await
}

/// `{encrypted, unlocked_until}`
#[tauri::command]
pub async fn wallet_status(client: State<'_, ClientState>) -> Result<WalletStatus, String> {
    let mut c = client.lock().await;
    status(&mut c).await
}

/// Unlock the wallet for `seconds` (1..=300); returns the new status.
#[tauri::command]
pub async fn wallet_unlock(
    client: State<'_, ClientState>,
    guard: State<'_, RelockState>,
    passphrase: String,
    seconds: i64,
) -> Result<WalletStatus, String> {
    let mut c = client.lock().await;
    unlock(&mut c, &guard, passphrase, seconds).await
}

/// Lock the wallet now; returns the new status.
#[tauri::command]
pub async fn wallet_lock(client: State<'_, ClientState>) -> Result<WalletStatus, String> {
    let mut c = client.lock().await;
    lock(&mut c).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::stub;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    const PASS: &str = "correct horse battery staple";

    /// A node with an encrypted wallet: walletpassphrase (PASS only) and walletlock move its lock.
    fn encrypted_node() -> (FreeBankClient, stub::Calls) {
        let until = Arc::new(AtomicU64::new(0));
        stub::serve(move |m, p| match m {
            "getwalletinfo" => Ok(json!({"walletname": "", "balance": 1.0, "unlocked_until": until.load(Ordering::SeqCst)})),
            "walletpassphrase" if p[0] == PASS => {
                until.store(1_900_000_000 + p[1].as_u64().unwrap(), Ordering::SeqCst);
                Ok(Value::Null)
            }
            "walletpassphrase" => Err((-14, "Error: The wallet passphrase entered was incorrect.".into())),
            "walletlock" => {
                until.store(0, Ordering::SeqCst);
                Ok(Value::Null)
            }
            _ => Err((-32601, "Method not found".into())),
        })
    }

    #[test]
    fn status_reads_getwalletinfo() {
        assert_eq!(status_from(&json!({"balance": 1})), WalletStatus { encrypted: false, unlocked_until: 0 });
        assert_eq!(status_from(&json!({"unlocked_until": 0})), WalletStatus { encrypted: true, unlocked_until: 0 });
        assert_eq!(
            status_from(&json!({"unlocked_until": 1_900_000_030})),
            WalletStatus { encrypted: true, unlocked_until: 1_900_000_030 }
        );
    }

    #[test]
    fn seconds_are_clamped() {
        assert_eq!([clamp_seconds(-5), clamp_seconds(0), clamp_seconds(30), clamp_seconds(301), clamp_seconds(i64::MAX)], [1, 1, 30, 300, 300]);
    }

    #[tokio::test]
    async fn unlock_then_lock() {
        let (mut c, calls) = encrypted_node();
        assert_eq!(status(&mut c).await.unwrap(), WalletStatus { encrypted: true, unlocked_until: 0 });

        let st = unlock(&mut c, &RelockGuard::default(), PASS.into(), 3600).await.unwrap();
        assert_eq!(st, WalletStatus { encrypted: true, unlocked_until: 1_900_000_300 });
        assert!(calls.lock().unwrap().iter().any(|(m, p)| m == "walletpassphrase" && p[1] == 300), "clamped to 300 s");

        assert_eq!(lock(&mut c).await.unwrap(), WalletStatus { encrypted: true, unlocked_until: 0 });
    }

    #[tokio::test]
    async fn a_wrong_passphrase_keeps_its_code_and_never_echoes() {
        let (mut c, _) = encrypted_node();
        let e = unlock(&mut c, &RelockGuard::default(), "hunter2".into(), 30).await.unwrap_err();
        assert!(e.starts_with("RPC error -14: "), "{}", e);
        assert!(!e.contains("hunter2"));
        assert_eq!(status(&mut c).await.unwrap().unlocked_until, 0);
    }

    #[tokio::test]
    async fn an_empty_passphrase_never_reaches_the_node() {
        let (mut c, calls) = encrypted_node();
        assert!(unlock(&mut c, &RelockGuard::default(), String::new(), 30).await.is_err());
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_wallet_without_a_passphrase() {
        let (mut c, _) = stub::serve(|m, _| match m {
            "getwalletinfo" => Ok(json!({"walletname": "", "balance": 1.0})),
            "walletlock" | "walletpassphrase" => {
                Err((RPC_WALLET_WRONG_ENC_STATE, format!("Error: running with an unencrypted wallet, but {} was called.", m)))
            }
            _ => Err((-32601, "Method not found".into())),
        });
        let none = WalletStatus { encrypted: false, unlocked_until: 0 };
        assert_eq!(status(&mut c).await.unwrap(), none);
        assert_eq!(lock(&mut c).await.unwrap(), none, "nothing to lock is not an error");
        assert!(unlock(&mut c, &RelockGuard::default(), PASS.into(), 30).await.unwrap_err().starts_with("RPC error -15: "));
    }
}
