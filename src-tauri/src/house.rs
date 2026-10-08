//! Run a house (v0.4.0): a batch lock of the house's mint. The mint's float wallet builds and signs the lock as the
//! notes' holder (`createnotelock <house> <units> <fee> <float address>`, on the mint's server). `note_lock_check`
//! says what the partners' approval would lock; `note_lock_send` approves it again and sends it only if it still
//! locks the same, held at the mint's float. Both drop the partners' signed lock here, so it never reaches the screens
//! (`approvenotelock` and `sendrawtransaction` aren't on rpc_call's list). The mint's wallet pays the fee; nothing
//! leaves this wallet.

use crate::commands::ClientState;
use crate::rpc::{FreeBankClient, RpcError};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

/// A lock is a few hundred bytes; anything far bigger isn't one.
const MAX_HEX: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoteLock {
    pub house: u64,
    pub units: u64,
    pub holder: String,
    pub txid: String,
}

pub(crate) const LOCK_WENT_OUT: &str = "This lock is on chain already: the house's token backing shows it.";
pub(crate) const LOCK_MAY_HAVE_GONE: &str = "Your node didn't answer after FreeBank handed it this lock, so it may have gone out. Check the house's token backing before making another lock.";
pub(crate) const LOCK_COINS_MOVED: &str = "The float's notes moved since this lock was made (a payout or a sweep): make a new lock on the mint's server.";
pub(crate) const LOCK_WAITING: &str = "A lock of this house is already waiting for a block. Wait for it, and check the house's token backing before making another.";

/// What the lock in `hex` would lock, from the partners' approval (whose signed lock is dropped).
async fn approve(c: &mut FreeBankClient, hex: &str) -> Result<(NoteLock, String), String> {
    let hex = hex.trim();
    if hex.is_empty() || hex.len() > MAX_HEX || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("That isn't a lock: paste the hex the mint's server printed.".into());
    }
    let a = match c.call_fresh_typed("approvenotelock", vec![json!(hex)]).await {
        Ok(a) => a,
        Err(RpcError::Rpc { message, .. }) if message.contains("already in the mempool") => return Err(LOCK_WAITING.into()),
        Err(e) => return Err(e.for_ui()),
    };
    let lock = read(&a).ok_or("approvenotelock's answer isn't a lock")?;
    let signed = a["hex"].as_str().ok_or("approvenotelock gave no lock")?.to_string();
    Ok((lock, signed))
}

fn read(a: &Value) -> Option<NoteLock> {
    Some(NoteLock {
        house: a["house"].as_u64()?,
        units: a["units"].as_u64()?,
        holder: a["holder"].as_str()?.to_string(),
        txid: a["txid"].as_str()?.to_string(),
    })
}

/// Whether the lock is what the screen showed, held at the mint's float.
fn same(l: &NoteLock, house: u32, units: u64, float: &str) -> bool {
    l.house == house as u64 && l.units == units && l.holder == float
}

/// What the mint's lock `hex` would lock. Wrap it in withUnlock (the approval signs with the house's keys).
#[tauri::command]
pub async fn note_lock_check(client: State<'_, ClientState>, hex: String) -> Result<NoteLock, String> {
    let mut c = client.lock().await;
    approve(&mut c, &hex).await.map(|(l, _)| l)
}

/// Approve the mint's lock `hex` and send it, if it locks `units` of house `house`'s notes held at `float` (the mint's
/// float address). Wrap it in withUnlock. Returns the txid.
#[tauri::command]
pub async fn note_lock_send(
    client: State<'_, ClientState>,
    hex: String,
    house: u32,
    units: u64,
    float: String,
) -> Result<String, String> {
    let mut c = client.lock().await;
    let (lock, signed) = approve(&mut c, &hex).await?;
    if lock.holder != float {
        return Err(format!("This lock takes notes held at {}, not the mint's float ({float}): it isn't sent.", lock.holder));
    }
    if !same(&lock, house, units, &float) {
        return Err("This lock isn't the one shown: check it again.".into());
    }
    let txid = match c.call_fresh_typed("sendrawtransaction", vec![json!(signed)]).await {
        Ok(t) => t,
        Err(RpcError::Rpc { code: -27, .. }) => return Err(LOCK_WENT_OUT.into()),
        Err(RpcError::Rpc { code: -25, message }) if message.contains("Missing inputs") => return Err(LOCK_COINS_MOVED.into()),
        Err(e) if e.did_nothing() => return Err(e.for_ui()),
        Err(_) => return Err(LOCK_MAY_HAVE_GONE.into()),
    };
    txid.as_str().map(str::to_string).ok_or_else(|| LOCK_MAY_HAVE_GONE.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_is_sent_only_as_shown_from_the_float() {
        let l = read(&json!({ "hex": "00", "txid": "t", "house": 3, "units": 500000, "holder": "Xfloat" })).unwrap();
        assert!(same(&l, 3, 500000, "Xfloat"));
        assert!(!same(&l, 4, 500000, "Xfloat"));
        assert!(!same(&l, 3, 500001, "Xfloat"));
        assert!(!same(&l, 3, 500000, "Xcustomer"), "notes held anywhere but the float aren't locked");
        assert!(read(&json!({ "hex": "00" })).is_none());
    }
}
