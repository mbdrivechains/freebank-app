//! Nostr events (NIP-01) for the phone link over Nostr (v0.6.0, from the Truthcoin app's link): just what the link
//! needs, written here rather than pulling in a Nostr library. An event's id is SHA-256 of
//! `[0, pubkey, created_at, kind, tags, content]` serialized without spaces; its signature is BIP340 Schnorr
//! (secp256k1) over the id. The Nostr keys only sign events: who is talking is proven by the pairing and the session
//! inside (crypto.rs), never by the Nostr key alone.

use k256::schnorr::{Signature, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

/// The event kind the link uses: ephemeral (20000-29999: relays pass it on and don't keep it), and claimed by no NIP
/// as of October 2026. The Truthcoin app's link uses it too, so relays can't tell the two apart.
pub const KIND: u32 = 21913;
/// Events carry a NIP-40 expiration this far ahead, for relays that keep ephemeral events anyway.
pub const EXPIRES_SECS: u64 = 300;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

pub struct NostrKey(SigningKey);

impl NostrKey {
    pub fn random() -> NostrKey {
        NostrKey(SigningKey::random(&mut rand::rngs::OsRng))
    }

    pub fn from_bytes(b: &[u8]) -> Result<NostrKey, String> {
        SigningKey::from_bytes(b).map(NostrKey).map_err(|_| "bad Nostr key".to_string())
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// The x-only public key, hex (how Nostr writes keys).
    pub fn pubkey(&self) -> String {
        hex::encode(self.0.verifying_key().to_bytes())
    }

    /// A signed event; `aux` is the BIP340 auxiliary randomness (random in use, fixed in test vectors).
    pub fn sign_with(&self, created_at: u64, kind: u32, tags: Vec<Vec<String>>, content: String, aux: &[u8; 32]) -> Event {
        let pubkey = self.pubkey();
        let id = event_id(&pubkey, created_at, kind, &tags, &content);
        let sig = self.0.sign_raw(&id, aux).expect("BIP340 signing");
        Event { id: hex::encode(id), pubkey, created_at, kind, tags, content, sig: hex::encode(sig.to_bytes()) }
    }

    /// An event of the link's kind to `to` (a Nostr pubkey), carrying `content`.
    pub fn message(&self, to: &str, content: String, now: u64) -> Event {
        let tags = vec![vec!["p".into(), to.into()], vec!["expiration".into(), (now + EXPIRES_SECS).to_string()]];
        self.sign_with(now, KIND, tags, content, &rand::random())
    }
}

/// NIP-01's id: SHA-256 of the canonical JSON array. serde_json writes it without spaces and escapes as NIP-01 asks
/// for every character our events carry.
pub fn event_id(pubkey: &str, created_at: u64, kind: u32, tags: &[Vec<String>], content: &str) -> [u8; 32] {
    let ser = serde_json::to_string(&json!([0, pubkey, created_at, kind, tags, content])).expect("serializable");
    Sha256::digest(ser.as_bytes()).into()
}

/// Is the event's id right, and its signature good?
pub fn verify(e: &Event) -> bool {
    let id = event_id(&e.pubkey, e.created_at, e.kind, &e.tags, &e.content);
    if hex::encode(id) != e.id {
        return false;
    }
    let (Ok(pk), Ok(sig)) = (hex::decode(&e.pubkey), hex::decode(&e.sig)) else { return false };
    let (Ok(vk), Ok(sig)) = (VerifyingKey::from_bytes(&pk), Signature::try_from(sig.as_slice())) else { return false };
    vk.verify_raw(&id, &sig).is_ok()
}

/// A Nostr pubkey as the link accepts it: 64 lowercase hex characters.
pub fn valid_pubkey(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The value of the first tag named `name`.
pub fn tag<'a>(e: &'a Event, name: &str) -> Option<&'a str> {
    e.tags.iter().find(|t| t.first().map(String::as_str) == Some(name)).and_then(|t| t.get(1)).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_events_verify_and_changed_ones_dont() {
        let k = NostrKey::random();
        let e = k.message(&"ab".repeat(32), "hello".into(), 1_700_000_000);
        assert!(verify(&e));
        assert_eq!(tag(&e, "p"), Some("ab".repeat(32).as_str()));
        let mut f = e.clone();
        f.content = "hellO".into();
        assert!(!verify(&f));
        let mut g = e.clone();
        g.id = format!("{}{}", if g.id.starts_with('0') { '1' } else { '0' }, &g.id[1..]);
        assert!(!verify(&g), "an id that isn't the event's");
        assert!(valid_pubkey(&k.pubkey()));
    }

    /// BIP340's own test vector 0 (secret key 3, aux 0, message 0): our signer matches the standard.
    #[test]
    fn bip340_vector_0() {
        let mut sk = [0u8; 32];
        sk[31] = 3;
        let k = SigningKey::from_bytes(&sk).unwrap();
        assert_eq!(
            hex::encode(k.verifying_key().to_bytes()).to_uppercase(),
            "F9308A019258C31049344F85F89D5229B531C845836F99B08601F113BCE036F9"
        );
        let sig = k.sign_raw(&[0u8; 32], &[0u8; 32]).unwrap();
        assert_eq!(
            hex::encode(sig.to_bytes()).to_uppercase(),
            "E907831F80848D1069A5371B402410364BDF1C5F8307B0084C55F1CE2DCA821525F66A4A85EA8B71E482A74F382D2CE5EBEEE8FDB2172F477DF4900D310536C0"
        );
    }
}
