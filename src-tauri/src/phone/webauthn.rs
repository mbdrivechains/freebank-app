//! Checking a phone's passkey (WebAuthn) assertion: freebank-phone's relay/PROTOCOL.md, "Face ID:
//! passkeys". The phone's platform authenticator signs `authenticatorData || SHA-256(clientDataJSON)`
//! with an ES256 key the phone added; the desktop checks every part before it trusts the session.

use p256::ecdsa::{signature::Verifier, DerSignature, VerifyingKey};
use sha2::{Digest, Sha256};

/// Why an assertion was refused (for the log and tests; the phone is told only that it failed).
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    ClientData,
    Type,
    Challenge,
    Origin,
    CrossOrigin,
    Short,
    RpId,
    Flags,
    Key,
    Signature,
    Counter,
}

/// The origin and relying party the page has, from the relay URL the desktop uses:
/// `wss://host/ws` gives ("https://host", "host"); `ws://localhost:8480/ws` gives
/// ("http://localhost:8480", "localhost"). Parsed as browsers do (punycode, case, default ports,
/// security review I2), so the origin is the one the page's assertions carry.
pub fn origin_for(relay_url: &str) -> Option<(String, String)> {
    let u = url::Url::parse(relay_url).ok()?;
    let scheme = match u.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return None,
    };
    if !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    let host = u.host_str().filter(|h| !h.is_empty())?.to_string();
    // A port is part of the origin unless it is the scheme's default (url leaves those out).
    let origin = match u.port() {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    };
    Some((origin, host))
}

/// Check an assertion against the phone's key (`pk`, the 65-byte point) and the live challenge. The
/// signature counter the phone then has, or why it was refused.
pub fn verify(
    pk: &[u8],
    rp_id: &str,
    origin: &str,
    challenge: &[u8],
    ad: &[u8],
    cdj: &[u8],
    sig: &[u8],
    stored_count: u32,
) -> Result<u32, Refused> {
    let client: serde_json::Value = serde_json::from_slice(cdj).map_err(|_| Refused::ClientData)?;
    if client["type"] != "webauthn.get" {
        return Err(Refused::Type);
    }
    if client["challenge"].as_str() != Some(crate::phone::crypto::b64u(challenge).as_str()) {
        return Err(Refused::Challenge);
    }
    if client["origin"].as_str() != Some(origin) {
        return Err(Refused::Origin);
    }
    if !(client["crossOrigin"].is_null() || client["crossOrigin"] == false) {
        return Err(Refused::CrossOrigin);
    }
    if ad.len() < 37 {
        return Err(Refused::Short);
    }
    if ad[..32] != Sha256::digest(rp_id.as_bytes())[..] {
        return Err(Refused::RpId);
    }
    if ad[32] & 0x01 == 0 || ad[32] & 0x04 == 0 {
        return Err(Refused::Flags);
    }
    let key = VerifyingKey::from_sec1_bytes(pk).map_err(|_| Refused::Key)?;
    let sig = DerSignature::try_from(sig).map_err(|_| Refused::Signature)?;
    let mut msg = ad.to_vec();
    msg.extend_from_slice(&Sha256::digest(cdj));
    key.verify(&msg, &sig).map_err(|_| Refused::Signature)?;
    let count = u32::from_be_bytes([ad[33], ad[34], ad[35], ad[36]]);
    if (count != 0 || stored_count != 0) && count <= stored_count {
        return Err(Refused::Counter);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::crypto::unb64u;

    const VECTORS: &str = include_str!("../../testdata/phone/webauthn-v1.json");

    #[test]
    fn the_shared_vectors() {
        let v: serde_json::Value = serde_json::from_str(VECTORS).unwrap();
        let s = |x: &serde_json::Value| unb64u(x.as_str().unwrap()).unwrap();
        let (pk, challenge) = (s(&v["pk"]), s(&v["challenge"]));
        let (rp, origin) = (v["rp_id"].as_str().unwrap(), v["origin"].as_str().unwrap());
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 19);
        for c in cases {
            let got = verify(&pk, rp, origin, &challenge, &s(&c["ad"]), &s(&c["cdj"]), &s(&c["sig"]), c["stored_count"].as_u64().unwrap() as u32);
            match c["ok"].as_bool().unwrap() {
                true => assert_eq!(got, Ok(c["new_count"].as_u64().unwrap() as u32), "{}", c["name"]),
                false => assert!(got.is_err(), "{} passed", c["name"]),
            }
        }
    }

    #[test]
    fn origins_from_relay_urls() {
        assert_eq!(origin_for("wss://app.ecxfreebank.com/ws"), Some(("https://app.ecxfreebank.com".into(), "app.ecxfreebank.com".into())));
        assert_eq!(origin_for("ws://localhost:8480/ws"), Some(("http://localhost:8480".into(), "localhost".into())));
        assert_eq!(origin_for("wss://App.EcxFreeBank.com:443/ws"), Some(("https://app.ecxfreebank.com".into(), "app.ecxfreebank.com".into())));
        assert_eq!(origin_for("wss://app.ecxfreebank.com:8443/ws"), Some(("https://app.ecxfreebank.com:8443".into(), "app.ecxfreebank.com".into())));
        assert_eq!(origin_for("https://app.ecxfreebank.com/ws"), None);
        assert_eq!(origin_for("wss://user@app.ecxfreebank.com/ws"), None);
        // As browsers read it: the extra slash is skipped, so the host is "ws".
        assert_eq!(origin_for("wss:///ws"), Some(("https://ws".into(), "ws".into())));
        assert_eq!(origin_for("wss://"), None);
        assert_eq!(origin_for("wss://bücher.example/ws"), Some(("https://xn--bcher-kva.example".into(), "xn--bcher-kva.example".into())));
    }
}
