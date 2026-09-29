//! The phone relay's crypto (relay/PROTOCOL.md in freebank-distribution, "Keys and crypto",
//! "Pairing", "The comparison code", "Session handshake", "The relay"): P-256 ECDH, HKDF-SHA256,
//! AES-256-GCM, SHA-256, and ECDSA for the relay proof. Every function takes its keys explicitly so
//! the test vectors can drive it.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::{Digest, Sha256};

pub const PAIR_INFO: &[u8] = b"fb-pair-v1";
pub const KK_LABEL: &[u8] = b"fb-kk-v1";
pub const SESSION_INFO: &[u8] = b"fb-session-v1";
pub const SAS_LABEL: &[u8] = b"fb-pair-sas-v1";
/// What the relay proof signs, before the challenge: D is also the desktop's ECDH key, so its
/// signatures carry a context and can't be passed off as anything else.
pub const PROOF_LABEL: &[u8] = b"fb-relay-proof-v1";

pub fn b64u(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

pub fn unb64u(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| "bad base64url".to_string())
}

/// Uncompressed SEC1, 65 bytes.
pub fn pub_bytes(p: &PublicKey) -> Vec<u8> {
    p.to_encoded_point(false).as_bytes().to_vec()
}

pub fn pub_b64u(p: &PublicKey) -> String {
    b64u(&pub_bytes(p))
}

/// A public key from b64u uncompressed SEC1. Compressed or off-curve points are refused.
pub fn parse_pub(s: &str) -> Result<PublicKey, String> {
    let b = unb64u(s)?;
    if b.len() != 65 || b[0] != 4 {
        return Err("public key must be 65-byte uncompressed SEC1".into());
    }
    PublicKey::from_sec1_bytes(&b).map_err(|_| "public key is not on P-256".to_string())
}

pub fn random_secret() -> SecretKey {
    SecretKey::random(&mut rand::rngs::OsRng)
}

/// The 32-byte X coordinate of the shared point (WebCrypto deriveBits(256)).
pub fn ecdh(sk: &SecretKey, pk: &PublicKey) -> [u8; 32] {
    let s = p256::ecdh::diffie_hellman(sk.to_nonzero_scalar(), pk.as_affine());
    let mut out = [0u8; 32];
    out.copy_from_slice(s.raw_secret_bytes());
    out
}

pub fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .expect("HKDF length is within limits");
}

pub fn sha256(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

/// room = b64u(SHA-256(D_pub))[0..22]
pub fn room(d_pub: &PublicKey) -> String {
    b64u(&sha256(&pub_bytes(d_pub)))[..22].to_string()
}

pub fn seal(key: &[u8; 32], nonce: &[u8; 12], pt: &[u8]) -> Vec<u8> {
    Aes256Gcm::new_from_slice(key)
        .expect("32-byte key")
        .encrypt(Nonce::from_slice(nonce), pt)
        .expect("AES-GCM encrypt")
}

pub fn open(key: &[u8; 32], nonce: &[u8; 12], ct: &[u8]) -> Result<Vec<u8>, String> {
    Aes256Gcm::new_from_slice(key)
        .expect("32-byte key")
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| "decrypt failed".to_string())
}

/// 12-byte big-endian counter nonce.
pub fn counter_nonce(n: u64) -> [u8; 12] {
    let mut b = [0u8; 12];
    b[4..].copy_from_slice(&n.to_be_bytes());
    b
}

/// Desktop side of pairing: k = HKDF(ECDH(D, E_pub), salt = C, info = "fb-pair-v1", L = 32).
pub fn pair_key(d: &SecretKey, e_pub: &PublicKey, c: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 32];
    hkdf(&ecdh(d, e_pub), c, PAIR_INFO, &mut k);
    k
}

/// The comparison code both ends show at pairing: h = SHA-256("fb-pair-sas-v1" || D_pub || P_pub ||
/// E_pub || C), raw 65-byte keys and the 16-byte code; the first 4 bytes of h, big-endian, modulo
/// 1,000,000, as 6 digits in two groups of three ("042 917").
pub fn pair_code(d_pub: &PublicKey, p_pub: &PublicKey, e_pub: &PublicKey, c: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(SAS_LABEL);
    for k in [d_pub, p_pub, e_pub] {
        h.update(pub_bytes(k));
    }
    h.update(c);
    let h: [u8; 32] = h.finalize().into();
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    let s = format!("{n:06}");
    format!("{} {}", &s[..3], &s[3..])
}

/// th = SHA-256("fb-kk-v1" || P_pub || D_pub || eP_pub || eD_pub), raw 65-byte keys.
pub fn transcript_hash(p: &PublicKey, d: &PublicKey, ep: &PublicKey, ed: &PublicKey) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(KK_LABEL);
    for k in [p, d, ep, ed] {
        h.update(pub_bytes(k));
    }
    h.finalize().into()
}

/// Both session keys, (k_pd, k_dp), from the desktop's side of the KK handshake:
/// ikm = ECDH(eD, eP) || ECDH(D, eP) || ECDH(eD, P), the mirror of the phone's
/// ECDH(eP, eD) || ECDH(eP, D) || ECDH(P, eD).
pub fn desktop_session_keys(
    d: &SecretKey,
    ed: &SecretKey,
    p_pub: &PublicKey,
    ep_pub: &PublicKey,
) -> ([u8; 32], [u8; 32]) {
    let mut ikm = Vec::with_capacity(96);
    ikm.extend_from_slice(&ecdh(ed, ep_pub));
    ikm.extend_from_slice(&ecdh(d, ep_pub));
    ikm.extend_from_slice(&ecdh(ed, p_pub));
    let th = transcript_hash(p_pub, &d.public_key(), ep_pub, &ed.public_key());
    split64(&ikm, &th)
}

/// The phone's side; only the tests use it, to play the phone.
#[cfg(test)]
pub fn phone_session_keys(
    p: &SecretKey,
    ep: &SecretKey,
    d_pub: &PublicKey,
    ed_pub: &PublicKey,
) -> ([u8; 32], [u8; 32]) {
    let mut ikm = Vec::with_capacity(96);
    ikm.extend_from_slice(&ecdh(ep, ed_pub));
    ikm.extend_from_slice(&ecdh(ep, d_pub));
    ikm.extend_from_slice(&ecdh(p, ed_pub));
    let th = transcript_hash(&p.public_key(), d_pub, &ep.public_key(), ed_pub);
    split64(&ikm, &th)
}

fn split64(ikm: &[u8], th: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let mut okm = [0u8; 64];
    hkdf(ikm, th, SESSION_INFO, &mut okm);
    let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
    a.copy_from_slice(&okm[..32]);
    b.copy_from_slice(&okm[32..]);
    (a, b)
}

/// The relay's challenge proof: ECDSA-P256-SHA256 over "fb-relay-proof-v1" || the challenge's
/// bytes, fixed 64-byte r||s.
pub fn proof(d: &SecretKey, challenge: &[u8]) -> [u8; 64] {
    let sk = p256::ecdsa::SigningKey::from(d);
    let mut msg = PROOF_LABEL.to_vec();
    msg.extend_from_slice(challenge);
    let sig: p256::ecdsa::Signature = sk.sign(&msg);
    let mut out = [0u8; 64];
    out.copy_from_slice(&sig.to_bytes());
    out
}

/// One direction of a session: counter nonces from 0, strictly in order.
pub struct Direction {
    key: [u8; 32],
    pub counter: u64,
}

impl Direction {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key, counter: 0 }
    }

    pub fn seal(&mut self, pt: &[u8]) -> Vec<u8> {
        let ct = seal(&self.key, &counter_nonce(self.counter), pt);
        self.counter += 1;
        ct
    }

    /// Any failure is final: the caller closes the session.
    pub fn open(&mut self, ct: &[u8]) -> Result<Vec<u8>, String> {
        let pt = open(&self.key, &counter_nonce(self.counter), ct)?;
        self.counter += 1;
        Ok(pt)
    }
}

/// A live session on one relay channel, desktop side.
pub struct Session {
    pub rx: Direction, // k_pd
    pub tx: Direction, // k_dp
}

impl Session {
    pub fn desktop(k_pd: [u8; 32], k_dp: [u8; 32]) -> Self {
        Self { rx: Direction::new(k_pd), tx: Direction::new(k_dp) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_nonce_is_big_endian() {
        assert_eq!(counter_nonce(0), [0u8; 12]);
        assert_eq!(counter_nonce(1)[11], 1);
        assert_eq!(counter_nonce(0x0102)[10..], [1, 2]);
    }

    #[test]
    fn both_sides_agree_and_directions_are_separate() {
        let (d, p, ep, ed) = (random_secret(), random_secret(), random_secret(), random_secret());
        let desk = desktop_session_keys(&d, &ed, &p.public_key(), &ep.public_key());
        let phone = phone_session_keys(&p, &ep, &d.public_key(), &ed.public_key());
        assert_eq!(desk, phone);
        assert_ne!(desk.0, desk.1);
    }

    #[test]
    fn proof_verifies_over_the_label_and_the_challenge() {
        use p256::ecdsa::signature::Verifier;
        let d = random_secret();
        let n = [7u8; 32];
        let sig = proof(&d, &n);
        let vk = p256::ecdsa::VerifyingKey::from(&d.public_key());
        let sig = p256::ecdsa::Signature::from_slice(&sig).unwrap();
        vk.verify(&[b"fb-relay-proof-v1".as_slice(), &n].concat(), &sig).unwrap();
        // Not over the bare challenge (the form the relay now refuses).
        assert!(vk.verify(&n, &sig).is_err());
    }

    #[test]
    fn pair_code_shape() {
        let (d, p, e) = (random_secret(), random_secret(), random_secret());
        let code = pair_code(&d.public_key(), &p.public_key(), &e.public_key(), &[1u8; 16]);
        assert_eq!(code.len(), 7);
        assert_eq!(&code[3..4], " ");
        assert!(code.chars().filter(|c| *c != ' ').all(|c| c.is_ascii_digit()));
        // Another phone (or ephemeral key, or pairing code) gives another code, as a rule.
        let other = pair_code(&d.public_key(), &random_secret().public_key(), &e.public_key(), &[1u8; 16]);
        let other_c = pair_code(&d.public_key(), &p.public_key(), &e.public_key(), &[2u8; 16]);
        assert!(other != code || other_c != code);
    }

    #[test]
    fn out_of_order_fails() {
        let k = [9u8; 32];
        let mut a = Direction::new(k);
        let mut b = Direction::new(k);
        let c0 = a.seal(b"zero");
        let c1 = a.seal(b"one");
        assert!(b.open(&c1).is_err());
        let mut b = Direction::new(k);
        assert_eq!(b.open(&c0).unwrap(), b"zero");
        assert_eq!(b.open(&c1).unwrap(), b"one");
    }
}
