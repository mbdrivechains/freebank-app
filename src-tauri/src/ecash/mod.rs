//! The app's eCash wallet (v0.2.6; operator, 2026-10-04: "i want them ecash wallet now .. adn bmm now").
//!
//! Two named wallets of the app's own in the eCash node already running (BitWindow's, or one installed by hand),
//! never in the enforcer (operator's decision 2026-09-29, `gateway/docs/distribution/APP_SELF_SUFFICIENT_SCOPE.md`):
//! - the main eCash wallet, whose key comes from the words, which the wallet passphrase opens for each payment;
//! - the bidding wallet, whose key is kept in an owner-only file so bids can go out with nobody there; it holds only
//!   what the owner moves into it (operator, 2026-10-04, chose "Small bidding wallet").
//!
//! Both are watch-only in the eCash node, and FreeBank checks and signs every payment itself (security review H1, H2):
//! neither the passphrase nor a private key goes to the node. Both come from the app's 24 words (`keys.rs`), so the
//! words bring them back. The node's login is found the way BitWindow finds it (`conn.rs`).

pub mod bmm;
pub mod commands;
pub mod conn;
pub mod keys;
pub mod sign;
pub mod wallet;

#[cfg(test)]
mod tests;

/// eCash's replay protection, as BitWindow stamps every eCash send (its `replay.ReplayLockTime`): nLockTime
/// 499999999 with an input below SEQUENCE_FINAL. eCash's patched node takes it as final; stock Bitcoin Core reads it as
/// a height ~500 million blocks away and refuses it, so the payment can't be replayed onto Bitcoin. The eCash node's own
/// wallet doesn't stamp it (locktime 0 on regtest, checked), so every transaction the app builds sets it.
pub const REPLAY_LOCKTIME: u32 = 499_999_999;

/// eCash amounts, sats -> "0.00000000" (the screens show eCash with 8 places, as ECX).
pub fn to_coins(sats: u64) -> String {
    format!("{}.{:08}", sats / 100_000_000, sats % 100_000_000)
}

/// A node's BTC-style amount (a JSON number) in sats.
pub fn sats_of(v: &serde_json::Value) -> Option<u64> {
    let f = v.as_f64()?;
    if !f.is_finite() || f < 0.0 {
        return None;
    }
    Some((f * 100_000_000.0).round() as u64)
}
