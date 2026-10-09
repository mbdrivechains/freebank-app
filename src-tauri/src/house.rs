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

/// Settlement between houses (v0.4.1, node v0.2.22): sign house `house`'s part of a netting round, the last signature
/// sending it. `signnetting` isn't on rpc_call's list (nothing named "sign" is): this checks first that the round is
/// at its signing stage, has the house in it and doesn't have its signature yet, so a round is signed only as the
/// screen showed it (decodenetting of the same hex). The node checks every bundle against the chain and the house's
/// own part. Wrap it in withUnlock. Returns signnetting's answer: the round to pass on, the house's net and payment,
/// and the txid once sent.
#[tauri::command]
pub async fn netting_sign(client: State<'_, ClientState>, house: u32, round: String) -> Result<Value, String> {
    let round = round.trim();
    if round.is_empty() || round.len() > 2 * MAX_ROUND || !round.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("That isn't a netting round: paste the hex you were given.".into());
    }
    let mut c = client.lock().await;
    let d = c.call_fresh_typed("decodenetting", vec![json!(round)]).await.map_err(|e| e.for_ui())?;
    signable(&d, house)?;
    match c.call_fresh_typed("signnetting", vec![json!(house), json!(round), json!(true)]).await {
        Ok(v) => Ok(v),
        Err(e) if e.did_nothing() => Err(e.for_ui()),
        // The last signature sends the round; signing the same round again builds the same transaction, which the
        // mempool refuses, and the node then says only that it failed to send.
        Err(RpcError::Rpc { message, .. }) if message.contains("send") => Err(NET_MAY_HAVE_GONE.into()),
        Err(RpcError::Rpc { message, .. }) => Err(message),
        Err(_) => Err(NET_MAY_HAVE_GONE.into()),
    }
}

pub(crate) const NET_MAY_HAVE_GONE: &str = "This round may have gone out already (signed before, or your node didn't answer after it was sent). Check your recent payments and your house's notes before signing it again.";

/// Relay takes a round's transaction up to 100,000 bytes; the round carries that and each house's part beside it.
const MAX_ROUND: usize = 250_000;

/// The fee a round this app starts pays (createnetting's default). A round naming this house its starter with a higher
/// fee wasn't started here: another house can write any fee into a round, and the starter's funding pays it.
const OWN_FEE: f64 = 0.001;

/// Whether house `house` may sign this decoded round now.
fn signable(d: &Value, house: u32) -> Result<(), String> {
    let houses: Vec<u64> = d["houses"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    if !houses.contains(&(house as u64)) {
        return Err(format!("House #{house} isn't in this round."));
    }
    if d["stage"].as_str() != Some("signing") {
        return Err(format!("This round isn't ready to sign: it's at {}.", d["stage"].as_str().unwrap_or("an unknown stage")));
    }
    if d["signed"].as_array().map_or(false, |a| a.iter().any(|x| x.as_u64() == Some(house as u64))) {
        return Err(format!("House #{house} has signed this round already: pass it on."));
    }
    if d["starter"].as_u64() == Some(house as u64) && d["fee"].as_f64().map_or(true, |f| f > OWN_FEE + 1e-12) {
        return Err(format!(
            "This round makes House #{house} pay its fee of {} sECX, more than a round this app starts pays: it isn't signed.",
            d["fee"]
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_is_signed_only_at_its_signing_stage_by_a_house_in_it() {
        let r = |stage: &str, signed: Value| json!({ "houses": [1, 5, 6], "stage": stage, "signed": signed });
        assert!(signable(&r("signing", json!([])), 5).is_ok());
        assert!(signable(&r("signing", json!([1])), 5).is_ok());
        assert!(signable(&r("signing", json!([5])), 5).unwrap_err().contains("already"));
        assert!(signable(&r("funding", json!([])), 5).unwrap_err().contains("isn't ready"));
        assert!(signable(&r("complete", json!([1, 5, 6])), 5).is_err());
        assert!(signable(&r("signing", json!([])), 7).unwrap_err().contains("isn't in this round"));
        let f = |starter: u64, fee: f64| json!({ "houses": [1, 5, 6], "stage": "signing", "signed": [], "starter": starter, "fee": fee });
        assert!(signable(&f(5, 0.001), 5).is_ok(), "this app's own fee");
        assert!(signable(&f(5, 0.1), 5).unwrap_err().contains("isn't signed"), "a fee another house wrote in for us");
        assert!(signable(&f(1, 0.1), 5).is_ok(), "another starter's fee isn't ours to pay");
    }

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
