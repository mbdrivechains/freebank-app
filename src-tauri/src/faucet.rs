//! The network's faucet (v0.6.1): on easynet, test coins to one address a day, asked when the user presses "Get easynet
//! coins" in Receive. The page and its front are FreeBank's (distribution `faucet/`), the paying program easynet's.
//! It is asked from Rust at the network's one fixed address over TLS, as Ask FreeBank is (`ask.rs`): only the address
//! goes, and the faucet counts requests by the caller's IP address, which its front sets (X-Forwarded-For).

use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;

/// What the faucet paid.
#[derive(Debug, Serialize, PartialEq)]
pub struct Paid {
    pub txid: String,
    /// As the faucet says it ("100", "0.1").
    pub amount: String,
    /// "ECX" (easynet's own coin) or "sECX" (FreeBank's).
    pub coin: String,
}

/// An address the faucet pays, as it should go, or why not. easynet's: `tb1…` (bech32), `m…`/`n…`, `2…`; FreeBank's on
/// easynet: `s…` (what getnewaddress gives), `X…` (older), `fbk1…` (bech32). Bech32 goes in lower case; base58 as it
/// is (its case counts). The faucet checks it properly; this only keeps out what can't be one.
pub fn check(address: &str) -> Result<String, String> {
    const BECH32: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let t = address.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(body) = lower.strip_prefix("tb1").or_else(|| lower.strip_prefix("fbk1")) {
        let one_case = t == lower || t == t.to_ascii_uppercase();
        if one_case && (20..=90).contains(&body.len()) && body.bytes().all(|b| BECH32.contains(&b)) {
            return Ok(lower);
        }
    } else if t.starts_with(['m', 'n', '2', 's', 'X']) && (26..=35).contains(&t.len()) && t.bytes().all(|b| BASE58.contains(&b)) {
        return Ok(t.to_string());
    }
    Err("That isn't an easynet or FreeBank address.".into())
}

/// The faucet's answer, or the sentence it gave (at most 300 characters).
pub fn read(status: u16, body: &[u8]) -> Result<Paid, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| {
        if status == 429 { "That's enough coins for now. Try again tomorrow." } else { "The faucet isn't answering. Try again later." }
            .to_string()
    })?;
    if let Some(e) = v["error"].as_str() {
        return Err(sentence(e));
    }
    let txid = v["txid"].as_str().filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()));
    let coin = v["coin"].as_str().filter(|c| matches!(*c, "ECX" | "sECX"));
    let amount = v["amount"].as_str().filter(|a| !a.is_empty() && a.len() <= 20 && a.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
    match (txid, coin, amount) {
        (Some(t), Some(c), Some(a)) => Ok(Paid { txid: t.to_ascii_lowercase(), amount: a.into(), coin: c.into() }),
        _ => Err("The faucet gave an answer FreeBank couldn't read.".into()),
    }
}

/// The faucet's reason as a sentence ("this address had ECX in the last 24 hours" → "This address had ECX in the last
/// 24 hours."), at most 300 characters, with no control characters. A first word with a capital in it is a name, and
/// stays as it is ("sECX went to your IP…").
fn sentence(e: &str) -> String {
    let mut s: String = e.chars().filter(|c| !c.is_control()).take(299).collect::<String>().trim().to_string();
    let first_word = s.split_whitespace().next().unwrap_or("");
    if let Some(first) = s.chars().next().filter(|_| !first_word.chars().any(char::is_uppercase)) {
        s.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    }
    if !s.is_empty() && !s.ends_with(['.', '!', '?']) {
        s.push('.');
    }
    s
}

const MAX_BODY: usize = 8 * 1024;

#[tauri::command]
pub async fn faucet_ask(address: String) -> Result<Paid, String> {
    let url = crate::network::current().faucet.ok_or("This network has no faucet.")?;
    let address = check(&address)?;
    // Its own client, as ask.rs's: https only, no redirects, the app's user agent.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .user_agent(concat!("FreeBank-app/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let mut r = http
        .post(url)
        .timeout(Duration::from_secs(40))
        .json(&json!({ "address": address }))
        .send()
        .await
        .map_err(|_| "The faucet isn't answering. Try again later.".to_string())?;
    let status = r.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|_| "The faucet stopped answering. Try again.".to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err("The faucet gave an answer FreeBank couldn't read.".into());
        }
    }
    let out = read(status, &body);
    match &out {
        Ok(p) => crate::activity::note(&format!("faucet: paid {} {}", p.amount, p.coin)),
        Err(e) => crate::activity::note(&format!("faucet: {}", e)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_easynet_and_freebank_addresses_go() {
        let a = format!("tb1q{}", "w508d6qejxtdg4y5r3zarvary0c5xw7k".repeat(1));
        assert_eq!(check(&format!("  {}  ", a.to_uppercase())).unwrap(), a);
        assert!(check(&format!("fbk1q{}", "qpzry9x8gf2tvdw0s3jn54khce6mua7l")).is_ok());
        // Base58 keeps its case: a FreeBank address from getnewaddress ("s…", easynet's note of 2026-10-21), an older
        // "X…", and easynet's own "m…", "n…", "2…".
        for good in [
            "sQJ2rDqWbUAEpG8kNEbRZBAwbiyiLCdhYb",
            "XoNWPCVzMb8BYYbwvtyfjXQN4dRJfB8hfz",
            "mipcBbFg9gMiCh81Kj8tqqdgoZub1ZJRfn",
            "n3GNqMveyvaPvUbH469vDRadqpJMPc84JA",
            "2N2JD6wb56AfK4tfmM6PwdVmoYk2dCKf4Br",
        ] {
            assert_eq!(check(good).as_deref(), Ok(good), "{good}");
        }
        for bad in [
            "",
            "tb1",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kbio",
            "tb1Qw508d6qejxtdg4y5r3zarvary0c5xw7k",
            "fbk1 qqqqqqqqqqqqqqqqqqqqqqqq",
            "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2",
            "sQJ2rDqWbUAEpG8kNEbRZBAwbiyiLCdhY0",
            "sQJ2rD",
        ] {
            assert!(check(bad).is_err(), "{bad}");
        }
        assert!(check(&format!("tb1{}", "q".repeat(91))).is_err(), "too long");
    }

    #[test]
    fn the_faucets_answers() {
        let t = "ab".repeat(32);
        assert_eq!(
            read(200, format!(r#"{{"txid":"{t}","amount":"100","coin":"ECX"}}"#).as_bytes()),
            Ok(Paid { txid: t.clone(), amount: "100".into(), coin: "ECX".into() })
        );
        assert_eq!(read(429, br#"{"error":"One payment per address a day."}"#), Err("One payment per address a day.".into()));
        assert_eq!(read(429, b"<html>"), Err("That's enough coins for now. Try again tomorrow.".into()));
        assert_eq!(read(502, b""), Err("The faucet isn't answering. Try again later.".into()));
        // Anything odd in a paid answer is refused, not shown.
        for odd in [
            format!(r#"{{"txid":"{t}","amount":"100","coin":"BTC"}}"#),
            format!(r#"{{"txid":"zz","amount":"100","coin":"ECX"}}"#),
            format!(r#"{{"txid":"{t}","amount":"1e9<b>","coin":"ECX"}}"#),
        ] {
            assert_eq!(read(200, odd.as_bytes()), Err("The faucet gave an answer FreeBank couldn't read.".into()), "{odd}");
        }
        let long = format!(r#"{{"error":"{}\u0007"}}"#, "x".repeat(400));
        assert_eq!(read(400, long.as_bytes()).unwrap_err().len(), 300);
        // The faucet's own words, as a sentence (its 429 of 2026-10-21).
        assert_eq!(
            read(429, br#"{"error":"this address had ECX in the last 24 hours"}"#),
            Err("This address had ECX in the last 24 hours.".into())
        );
        assert_eq!(read(503, br#"{"error":"Empty. Try later!"}"#), Err("Empty. Try later!".into()));
        // A coin's name keeps its case (the faucet's per-IP limit, walked on 2026-10-22).
        assert_eq!(
            read(429, br#"{"error":"sECX went to your IP in the last 24 hours"}"#),
            Err("sECX went to your IP in the last 24 hours.".into())
        );
    }
}
