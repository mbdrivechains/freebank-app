//! The app seed (v0.2.0 wallet). One set of 24 BIP39 English words, made from 256 bits of the
//! operating system's randomness, is the app's recovery phrase. The FreeBank wallet's HD seed comes from
//! the words by BIP85's HD-Seed WIF application, index 0:
//!
//!   words -> BIP39 seed (empty BIP39 passphrase) -> BIP32 root -> m/83696968'/2'/0'
//!         -> HMAC-SHA512(key "bip-entropy-from-k", message: that key) -> the first 32 bytes
//!
//! BIP85 wrote that application for this use: the 32 bytes, as a compressed WIF, are the hdseed of a
//! Bitcoin Core wallet, and freebankd is a Core 0.16 wallet with Bitcoin's WIF prefix (128) on main. So
//! any BIP85 tool's "WIF" output at index 0 is exactly the string the app hands to freebankd with
//! `sethdseed true "<WIF>"`: the words bring the wallet back without this app in one step. freebankd
//! then derives its keys the way Core always has: BIP32 from the 32-byte seed, m/0'/0'/k' to receive
//! and m/0'/1'/k' for change. So other wallets can't read FreeBank's addresses from the words
//! directly.
//!
//! The words' entropy is kept at `<app data>/wallet/seed.enc`, encrypted with the wallet passphrase:
//! Argon2id stretches the passphrase (its parameters and salt sit in the file's header), and
//! XChaCha20-Poly1305 seals the 32 bytes. The header also carries the HD seed's key id (what
//! getwalletinfo calls hdmasterkeyid; public), so the app can tell without the passphrase whether the
//! words belong to the wallet the node has. The whole header is authenticated with the sealed bytes.
//! Secrets live in `Zeroizing` buffers and are never logged or put in an error.

use argon2::{Algorithm, Argon2, Params, Version};
use bitcoin::base58;
use bitcoin::bip32::{ChainCode, ChildNumber, Xpriv};
use bitcoin::hashes::{hash160, sha512, Hash, HashEngine, Hmac, HmacEngine};
use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
use bitcoin::NetworkKind;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::Serialize;
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

/// The app's recovery words are always 24 (256 bits).
pub const WORD_COUNT: usize = 24;

/// BIP85's HD-Seed WIF application (2'), index 0'.
pub const BIP85_PATH: [u32; 3] = [83_696_968, 2, 0];
pub const BIP85_PATH_TEXT: &str = "m/83696968'/2'/0'";

/// base58Prefixes[PUBKEY_ADDRESS] on both of freebankd's networks: addresses start with X.
pub const PUBKEY_ADDRESS: u8 = 75;

/// The words' 256 bits.
pub type Entropy = Zeroizing<[u8; 32]>;

/// Which network the node runs; the private-key prefixes differ (freebankd's chainparams.cpp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    Main,
    Regtest,
}

impl Chain {
    /// getblockchaininfo's `chain`.
    pub fn from_name(name: &str) -> Result<Chain, String> {
        match name {
            "main" => Ok(Chain::Main),
            "regtest" => Ok(Chain::Regtest),
            other => Err(format!("FreeBank doesn't know the network \"{}\".", other)),
        }
    }

    /// base58Prefixes[SECRET_KEY]: 128 on main (chainparams.cpp:168), 239 on regtest (:286).
    pub fn wif_prefix(self) -> u8 {
        match self {
            Chain::Main => 128,
            Chain::Regtest => 239,
        }
    }

    /// base58Prefixes[EXT_SECRET_KEY]: xprv on main (chainparams.cpp:170), tprv on regtest (:288).
    pub fn xprv_version(self) -> [u8; 4] {
        match self {
            Chain::Main => [0x04, 0x88, 0xAD, 0xE4],
            Chain::Regtest => [0x04, 0x35, 0x83, 0x94],
        }
    }
}

// ---- Words ----

/// 32 bytes from the operating system's random source.
pub fn new_entropy() -> Entropy {
    let mut e = Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *e);
    e
}

/// The 24 words for 32 bytes of entropy.
pub fn words(entropy: &[u8; 32]) -> Zeroizing<Vec<String>> {
    let m = bip39::Mnemonic::from_entropy_in(bip39::Language::English, entropy).expect("32 bytes is a BIP39 entropy length");
    Zeroizing::new(m.words().map(String::from).collect())
}

/// Words as typed or pasted: lower case, split on spaces, commas and line breaks, with list numbering
/// ("1.", "12)", "(3)", "4:") dropped. When every word has a number and the numbers are 1 to n, the
/// words go in that order: a table of words copied out of Notes reads across its rows, not down.
fn tokens(text: &str) -> Vec<String> {
    let mut numbered: Vec<(Option<usize>, String)> = Vec::new();
    let mut number = None;
    for t in text.split(|c: char| c.is_whitespace() || c == ',') {
        let t = t.trim().to_lowercase();
        let n = t.trim_start_matches(['(', '#']).trim_end_matches(['.', ')', ':']);
        if t.is_empty() {
            continue;
        } else if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
            number = n.parse().ok();
        } else {
            numbered.push((number.take(), t));
        }
    }
    let mut order: Vec<usize> = numbered.iter().filter_map(|(n, _)| *n).collect();
    order.sort_unstable();
    if order.len() == numbered.len() && order.iter().enumerate().all(|(i, &n)| n == i + 1) {
        numbered.sort_by_key(|(n, _)| *n);
    }
    numbered.into_iter().map(|(_, t)| t).collect()
}

/// What the restore screen says while the words are typed.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WordsCheck {
    pub count: usize,
    /// Positions (from 1) of words that aren't recovery words.
    pub unknown: Vec<usize>,
    /// 24 known words whose checksum holds: they can be restored.
    pub ok: bool,
    /// 24 known words, but their checksum fails: one is wrong or out of place.
    pub checksum_failed: bool,
}

pub fn check_words(text: &str) -> WordsCheck {
    let t = Zeroizing::new(tokens(text));
    let english = bip39::Language::English;
    let unknown: Vec<usize> = t
        .iter()
        .enumerate()
        .filter(|(_, w)| english.find_word(w).is_none())
        .map(|(i, _)| i + 1)
        .collect();
    let complete = t.len() == WORD_COUNT && unknown.is_empty();
    let ok = complete && parse_words(text).is_ok();
    WordsCheck { count: t.len(), unknown, ok, checksum_failed: complete && !ok }
}

/// The entropy behind 24 recovery words, checked word by word and by the BIP39 checksum.
pub fn parse_words(text: &str) -> Result<Entropy, String> {
    let t = Zeroizing::new(tokens(text));
    if t.len() != WORD_COUNT {
        return Err(format!("FreeBank's recovery words are {} words; these are {}.", WORD_COUNT, t.len()));
    }
    let english = bip39::Language::English;
    if let Some(i) = t.iter().position(|w| english.find_word(w).is_none()) {
        return Err(format!("Word {} isn't one of the recovery words. Check its spelling.", i + 1));
    }
    let joined = Zeroizing::new(t.join(" "));
    let m = bip39::Mnemonic::parse_in_normalized(english, &joined)
        .map_err(|_| "These words don't fit together: one of them is wrong or out of place.".to_string())?;
    let (mut raw, len) = m.to_entropy_array();
    let mut e = Zeroizing::new([0u8; 32]);
    let ok = len == 32;
    if ok {
        e.copy_from_slice(&raw[..32]);
    }
    raw.zeroize();
    if !ok {
        return Err("These words don't make a 256-bit seed.".into());
    }
    Ok(e)
}

// ---- BIP32 and BIP85 ----

fn hmac_sha512(key: &[u8], msg: &[u8]) -> Zeroizing<[u8; 64]> {
    let mut engine = HmacEngine::<sha512::Hash>::new(key);
    engine.input(msg);
    Zeroizing::new(Hmac::<sha512::Hash>::from_engine(engine).to_byte_array())
}

fn hardened(path: &[u32]) -> Result<Vec<ChildNumber>, String> {
    path.iter()
        .map(|&i| ChildNumber::from_hardened_idx(i).map_err(|e| e.to_string()))
        .collect()
}

/// Wipe an extended key's secret parts before it is dropped. (secp256k1 keys don't wipe themselves;
/// this is the best the library allows.)
fn wipe(x: &mut Xpriv) {
    x.private_key.non_secure_erase();
    x.chain_code = ChainCode::from([0u8; 32]);
}

/// BIP85: derive `path` (all hardened) from `root`, then HMAC-SHA512 with key "bip-entropy-from-k".
pub fn bip85_entropy(root: &Xpriv, path: &[u32]) -> Result<Zeroizing<[u8; 64]>, String> {
    let secp = Secp256k1::new();
    let mut child = root.derive_priv(&secp, &hardened(path)?).map_err(|e| e.to_string())?;
    let k = Zeroizing::new(child.private_key.secret_bytes());
    wipe(&mut child);
    Ok(hmac_sha512(b"bip-entropy-from-k", &*k))
}

/// The BIP32 root of the words' BIP39 seed (empty BIP39 passphrase).
fn words_root(entropy: &[u8; 32]) -> Result<Xpriv, String> {
    let m = bip39::Mnemonic::from_entropy_in(bip39::Language::English, entropy).map_err(|e| e.to_string())?;
    let seed = Zeroizing::new(m.to_seed_normalized(""));
    Xpriv::new_master(NetworkKind::Main, &*seed).map_err(|e| e.to_string())
}

/// The FreeBank wallet's HD seed from the words: BIP85's HD-Seed WIF at index 0 (`hd_seed_from_root`).
pub fn freebank_hd_seed(entropy: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, String> {
    let mut root = words_root(entropy)?;
    let seed = hd_seed_from_root(&root);
    wipe(&mut root);
    seed
}

/// BIP85's HD-Seed WIF application at index 0 from a BIP32 root: the most significant 256 bits of the
/// entropy at m/83696968'/2'/0', the secret of the WIF a BIP85 tool shows. It must be a valid secp256k1
/// key; BIP85 says to fail hard otherwise (odds below 1 in 2^127).
pub fn hd_seed_from_root(root: &Xpriv) -> Result<Zeroizing<[u8; 32]>, String> {
    let full = bip85_entropy(root, &BIP85_PATH)?;
    let mut seed = Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&full[..32]);
    if SecretKey::from_slice(&*seed).is_err() {
        return Err(
            "These recovery words give a key FreeBank can't use (a one in 2^128 chance). Make new words.".into(),
        );
    }
    Ok(seed)
}

/// The master extended key freebankd makes from its 32-byte HD seed (Core's CExtKey::SetMaster:
/// BIP32 with the seed as input), the key `dumpwallet` prints as "extended private masterkey".
fn core_master(hd_seed: &[u8; 32]) -> Result<Xpriv, String> {
    Xpriv::new_master(NetworkKind::Main, hd_seed).map_err(|e| e.to_string())
}

/// A compressed WIF with FreeBank's prefix for `chain`, as `sethdseed` takes it.
pub fn wif(hd_seed: &[u8; 32], chain: Chain) -> Zeroizing<String> {
    let mut data = Zeroizing::new([0u8; 34]);
    data[0] = chain.wif_prefix();
    data[1..33].copy_from_slice(hd_seed);
    data[33] = 1; // compressed
    Zeroizing::new(base58::encode_check(&*data))
}

/// Hash160 of the HD seed's compressed public key: the wallet's key id for it.
pub fn key_id(hd_seed: &[u8; 32]) -> Result<[u8; 20], String> {
    let secp = Secp256k1::new();
    let mut sk = SecretKey::from_slice(hd_seed).map_err(|_| "Not a valid key.".to_string())?;
    let pk = PublicKey::from_secret_key(&secp, &sk).serialize();
    sk.non_secure_erase();
    Ok(hash160::Hash::hash(&pk).to_byte_array())
}

/// A key id as Core prints it (getwalletinfo's hdmasterkeyid): the 20 bytes reversed, in hex.
pub fn key_id_hex(id: &[u8; 20]) -> String {
    let mut r = *id;
    r.reverse();
    hex::encode(r)
}

/// An extended private key in base58, with `version` (Core serialises depth, parent fingerprint and
/// child number as zero for a master key).
fn serialize_xprv(version: [u8; 4], chain_code: &[u8; 32], key: &[u8; 32]) -> Zeroizing<String> {
    let mut raw = Zeroizing::new([0u8; 78]);
    raw[..4].copy_from_slice(&version);
    // depth 0, parent fingerprint 0, child number 0: bytes 4..13 stay zero
    raw[13..45].copy_from_slice(chain_code);
    raw[45] = 0;
    raw[46..78].copy_from_slice(key);
    Zeroizing::new(base58::encode_check(&*raw))
}

/// The FreeBank wallet's master xprv (tprv on regtest), the one freebankd derives from its HD seed.
pub fn master_xprv(hd_seed: &[u8; 32], chain: Chain) -> Result<Zeroizing<String>, String> {
    let mut m = core_master(hd_seed)?;
    let key = Zeroizing::new(m.private_key.secret_bytes());
    let cc = Zeroizing::new(m.chain_code.to_bytes());
    wipe(&mut m);
    Ok(serialize_xprv(chain.xprv_version(), &cc, &key))
}

/// The address freebankd gives key `index` of the HD seed: m/0'/0'/index' to receive, m/0'/1'/index'
/// for change, as P2PKH with FreeBank's prefix (the app asks for "legacy" addresses).
pub fn address(hd_seed: &[u8; 32], internal: bool, index: u32) -> Result<String, String> {
    let secp = Secp256k1::new();
    let mut m = core_master(hd_seed)?;
    let child = m.derive_priv(&secp, &hardened(&[0, internal as u32, index])?);
    wipe(&mut m);
    let mut child = child.map_err(|e| e.to_string())?;
    let pk = PublicKey::from_secret_key(&secp, &child.private_key).serialize();
    wipe(&mut child);
    let mut data = vec![PUBKEY_ADDRESS];
    data.extend_from_slice(hash160::Hash::hash(&pk).as_byte_array());
    Ok(base58::encode_check(&data))
}

// ---- The seed file ----
//
// Layout (integers little-endian), 146 bytes:
//   0   8  magic "FBKSEED\0"
//   8   1  version (1)
//   9   1  key derivation (1 = Argon2id, version 0x13)
//  10   4  Argon2 memory, KiB
//  14   4  Argon2 passes
//  18   4  Argon2 lanes
//  22  32  salt
//  54  24  XChaCha20 nonce
//  78  20  the HD seed's key id (Hash160, natural byte order)
//  98  48  the sealed entropy: 32 bytes, then the 16-byte Poly1305 tag
// Bytes 0..98 are the associated data: changing any of them breaks the tag.

const MAGIC: &[u8; 8] = b"FBKSEED\0";
const VERSION: u8 = 1;
const KDF_ARGON2ID: u8 = 1;
const HEADER_LEN: usize = 98;
pub const FILE_LEN: usize = HEADER_LEN + 32 + 16;

/// Argon2id's cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

/// 64 MiB and three passes (RFC 9106's second recommended setting, on one lane): a fraction of a
/// second on a desktop, for each unlock of the file.
pub const KDF: Kdf = Kdf { m_kib: 64 * 1024, t: 3, p: 1 };

/// What a file may ask for before the tag has been checked: a changed header can't make the app
/// allocate gigabytes or spin for minutes.
fn kdf_in_bounds(k: Kdf) -> bool {
    (1..=16).contains(&k.p) && (1..=16).contains(&k.t) && k.m_kib >= 8 * k.p && k.m_kib <= 1024 * 1024
}

fn derive_key(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<Zeroizing<[u8; 32]>, String> {
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| e.to_string())?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut *key)
        .map_err(|e| e.to_string())?;
    Ok(key)
}

/// Seal the words' entropy under the passphrase. `key_id` is the HD seed's (public) key id.
pub fn seal(entropy: &[u8; 32], key_id: &[u8; 20], passphrase: &str, kdf: Kdf) -> Result<Vec<u8>, String> {
    if passphrase.is_empty() {
        return Err("Please enter your wallet passphrase.".into());
    }
    if !kdf_in_bounds(kdf) {
        return Err("Those key-stretching settings are out of range.".into());
    }
    let mut salt = [0u8; 32];
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let mut file = Vec::with_capacity(FILE_LEN);
    file.extend_from_slice(MAGIC);
    file.push(VERSION);
    file.push(KDF_ARGON2ID);
    for v in [kdf.m_kib, kdf.t, kdf.p] {
        file.extend_from_slice(&v.to_le_bytes());
    }
    file.extend_from_slice(&salt);
    file.extend_from_slice(&nonce);
    file.extend_from_slice(key_id);
    debug_assert_eq!(file.len(), HEADER_LEN);
    let key = derive_key(passphrase, &salt, kdf)?;
    let sealed = XChaCha20Poly1305::new(Key::from_slice(&*key))
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: entropy, aad: &file })
        .map_err(|_| "Couldn't seal the recovery words.".to_string())?;
    file.extend_from_slice(&sealed);
    Ok(file)
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpenError {
    /// The passphrase doesn't open it (or the sealed part was changed: the two look the same).
    WrongPassphrase,
    /// Not a seed file this app can read.
    Damaged(String),
}

impl OpenError {
    pub fn for_ui(&self) -> String {
        match self {
            OpenError::WrongPassphrase => "That passphrase doesn't open FreeBank's copy of your recovery words.".into(),
            OpenError::Damaged(why) => format!("FreeBank's copy of your recovery words can't be read: {}", why),
        }
    }
}

struct Header {
    kdf: Kdf,
    salt: [u8; 32],
    nonce: [u8; 24],
    key_id: [u8; 20],
}

fn parse_header(file: &[u8]) -> Result<Header, OpenError> {
    let damaged = |why: &str| OpenError::Damaged(why.to_string());
    if file.len() < HEADER_LEN || &file[..8] != MAGIC {
        return Err(damaged("it isn't a FreeBank seed file."));
    }
    if file[8] != VERSION || file[9] != KDF_ARGON2ID {
        return Err(damaged("it was written by a newer FreeBank."));
    }
    if file.len() != FILE_LEN {
        return Err(damaged("it is the wrong size."));
    }
    let u32_at = |i: usize| u32::from_le_bytes([file[i], file[i + 1], file[i + 2], file[i + 3]]);
    let kdf = Kdf { m_kib: u32_at(10), t: u32_at(14), p: u32_at(18) };
    if !kdf_in_bounds(kdf) {
        return Err(damaged("its settings are out of range."));
    }
    let mut h = Header { kdf, salt: [0; 32], nonce: [0; 24], key_id: [0; 20] };
    h.salt.copy_from_slice(&file[22..54]);
    h.nonce.copy_from_slice(&file[54..78]);
    h.key_id.copy_from_slice(&file[78..98]);
    Ok(h)
}

/// The HD seed's key id in a seed file, read without the passphrase.
pub fn file_key_id(file: &[u8]) -> Result<[u8; 20], OpenError> {
    parse_header(file).map(|h| h.key_id)
}

/// Open a seed file: the words' entropy and the HD seed's key id. Checks that the entropy still gives
/// that key id.
pub fn open(file: &[u8], passphrase: &str) -> Result<(Entropy, [u8; 20]), OpenError> {
    let h = parse_header(file)?;
    let key = derive_key(passphrase, &h.salt, h.kdf).map_err(OpenError::Damaged)?;
    let plain = Zeroizing::new(
        XChaCha20Poly1305::new(Key::from_slice(&*key))
            .decrypt(
                XNonce::from_slice(&h.nonce),
                Payload { msg: &file[HEADER_LEN..], aad: &file[..HEADER_LEN] },
            )
            .map_err(|_| OpenError::WrongPassphrase)?,
    );
    if plain.len() != 32 {
        return Err(OpenError::Damaged("it holds the wrong number of bytes.".into()));
    }
    let mut e = Zeroizing::new([0u8; 32]);
    e.copy_from_slice(&plain);
    let hd = freebank_hd_seed(&e).map_err(OpenError::Damaged)?;
    if key_id(&hd).map_err(OpenError::Damaged)? != h.key_id {
        return Err(OpenError::Damaged("its words don't match the key id it was saved with.".into()));
    }
    Ok((e, h.key_id))
}

/// `words` are this wallet's recovery words: their key id is the one FreeBank's sealed copy of the words records (read
/// without the passphrase). For turning "Approve sends on my phone" off without the phone (v0.2.5); the caller checks
/// the node's own key id too (`words_key_id_hex`), as the sealed copy's header could have been swapped.
pub fn words_are_this_wallets(app_dir: &Path, words: &str) -> Result<bool, String> {
    let file = read_file(&seed_path(app_dir))?.ok_or("This wallet has no recovery words in FreeBank to check them against.")?;
    let recorded = file_key_id(&file).map_err(|_| "FreeBank's copy of the recovery words can't be read.".to_string())?;
    Ok(words_key_id_hex(words)? == Some(key_id_hex(&recorded)))
}

/// The key id of the wallet that `words` make, as getwalletinfo's hdmasterkeyid shows it; None if they aren't 24
/// recovery words.
pub fn words_key_id_hex(words: &str) -> Result<Option<String>, String> {
    let Ok(entropy) = parse_words(words) else { return Ok(None) };
    let hd = freebank_hd_seed(&entropy)?;
    Ok(Some(key_id_hex(&key_id(&hd)?)))
}

/// The key id FreeBank's sealed copy of the words records (hdmasterkeyid's form), if there is a readable copy.
pub fn sealed_key_id_hex(app_dir: &Path) -> Option<String> {
    let file = read_file(&seed_path(app_dir)).ok()??;
    file_key_id(&file).ok().map(|id| key_id_hex(&id))
}

/// `<app data>/wallet/seed.enc`
pub fn seed_path(app_dir: &Path) -> PathBuf {
    app_dir.join("wallet").join("seed.enc")
}

/// Make `dir` (and its parents) and keep it private to this user.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {}", dir.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Couldn't protect {}: {}", dir.display(), e))?;
    }
    Ok(())
}

/// Write `bytes` to `path` atomically, readable by this user only (0600): a new file beside it is
/// written and flushed, then renamed over `path`, then the folder is flushed. `path` is either the old
/// file or the new one, never half of one.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = path.parent().ok_or("A file needs a folder.")?;
    private_dir(dir)?;
    let name = path.file_name().ok_or("A file needs a name.")?.to_string_lossy().into_owned();
    let tmp = dir.join(format!(".{}.tmp-{:08x}", name, rand::random::<u32>()));
    let result = (|| {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Couldn't write {}: {}", path.display(), e));
    }
    Ok(())
}

/// The seed file's bytes, or None when there is none.
pub fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Couldn't read {}: {}", path.display(), e)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbered_words_go_by_their_numbers() {
        let w = |t: &str| super::tokens(t).join(" ");
        assert_eq!(w("abandon ability, able\nabout"), "abandon ability able about");
        // Copied back out of a table in Notes: across the rows, numbers kept.
        assert_eq!(w("1. abandon\t3. able\n2. ability\t4. about"), "abandon ability able about");
        assert_eq!(w("(2) ability #1 abandon 3: able"), "abandon ability able");
        // Numbers that don't run 1 to n, or a word without one: the words stay as they came.
        assert_eq!(w("1. abandon 3. ability"), "abandon ability");
        assert_eq!(w("2. ability abandon"), "ability abandon");
    }

    use super::*;
    use std::str::FromStr;

    /// Cheap Argon2 settings, so the tests that seal many times stay quick in debug builds.
    const QUICK: Kdf = Kdf { m_kib: 64, t: 1, p: 1 };

    fn unhex<const N: usize>(s: &str) -> [u8; N] {
        hex::decode(s).unwrap().try_into().unwrap()
    }

    /// BIP39's published English vectors (trezor/python-mnemonic vectors.json, the 24-word ones), with
    /// the vectors' passphrase "TREZOR": entropy, words, seed, BIP32 root.
    const BIP39: [(&str, &str, &str, &str); 4] = [
        (
            "0000000000000000000000000000000000000000000000000000000000000000",
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art",
            "bda85446c68413707090a52022edd26a1c9462295029f2e60cd7c4f2bbd3097170af7a4d73245cafa9c3cca8d561a7c3de6f5d4a10be8ed2a5e608d68f92fcc8",
            "xprv9s21ZrQH143K32qBagUJAMU2LsHg3ka7jqMcV98Y7gVeVyNStwYS3U7yVVoDZ4btbRNf4h6ibWpY22iRmXq35qgLs79f312g2kj5539ebPM",
        ),
        (
            "8080808080808080808080808080808080808080808080808080808080808080",
            "letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic avoid letter advice cage absurd amount doctor acoustic bless",
            "c0c519bd0e91a2ed54357d9d1ebef6f5af218a153624cf4f2da911a0ed8f7a09e2ef61af0aca007096df430022f7a2b6fb91661a9589097069720d015e4e982f",
            "xprv9s21ZrQH143K3CSnQNYC3MqAAqHwxeTLhDbhF43A4ss4ciWNmCY9zQGvAKUSqVUf2vPHBTSE1rB2pg4avopqSiLVzXEU8KziNnVPauTqLRo",
        ),
        (
            "68a79eaca2324873eacc50cb9c6eca8cc68ea5d936f98787c60c7ebc74e6ce7c",
            "hamster diagram private dutch cause delay private meat slide toddler razor book happy fancy gospel tennis maple dilemma loan word shrug inflict delay length",
            "64c87cde7e12ecf6704ab95bb1408bef047c22db4cc7491c4271d170a1b213d20b385bc1588d9c7b38f1b39d415665b8a9030c9ec653d75e65f847d8fc1fc440",
            "xprv9s21ZrQH143K2XTAhys3pMNcGn261Fi5Ta2Pw8PwaVPhg3D8DWkzWQwjTJfskj8ofb81i9NP2cUNKxwjueJHHMQAnxtivTA75uUFqPFeWzk",
        ),
        (
            "f585c11aec520db57dd353c69554b21a89b20fb0650966fa0a9d6f74fd989d8f",
            "void come effort suffer camp survey warrior heavy shoot primary clutch crush open amazing screen patrol group space point ten exist slush involve unfold",
            "01f5bced59dec48e362f2c45b5de68b9fd6c92c6634f44d6d40aab69056506f0e35524a518034ddc1192e1dacd32c1ed3eaa3c3b131c88ed8e7e54c49a5d0998",
            "xprv9s21ZrQH143K39rnQJknpH1WEPFJrzmAqqasiDcVrNuk926oizzJDDQkdiTvNPr2FYDYzWgiMiC63YmfPAa2oPyNB23r2g7d1yiK6WpqaQS",
        ),
    ];

    #[test]
    fn bip39_published_vectors() {
        for (entropy, phrase, seed, root) in BIP39 {
            let e: [u8; 32] = unhex(entropy);
            assert_eq!(words(&e).join(" "), phrase);
            assert_eq!(*parse_words(phrase).unwrap(), e);
            let m = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, phrase).unwrap();
            let s = m.to_seed_normalized("TREZOR");
            assert_eq!(hex::encode(s), seed);
            assert_eq!(Xpriv::new_master(NetworkKind::Main, &s).unwrap().to_string(), root);
        }
    }

    /// BIP85's published root key and vectors.
    const BIP85_ROOT: &str =
        "xprv9s21ZrQH143K2LBWUUQRFXhucrQqBpKdRRxNVq2zBqsx8HVqFk2uYo8kmbaLLHRdqtQpUm98uKfu3vca1LqdGhUtyoFnCNkfmXRyPXLjbKb";

    #[test]
    fn bip85_published_vectors() {
        let root = Xpriv::from_str(BIP85_ROOT).unwrap();
        let secp = Secp256k1::new();
        // The general derivation vectors: the derived key and its entropy.
        for (path, key, entropy) in [
            (
                [83_696_968u32, 0, 0],
                "cca20ccb0e9a90feb0912870c3323b24874b0ca3d8018c4b96d0b97c0e82ded0",
                "efecfbccffea313214232d29e71563d941229afb4338c21f9517c41aaa0d16f00b83d2a09ef747e7a64e8e2bd5a14869e693da66ce94ac2da570ab7ee48618f7",
            ),
            (
                [83_696_968, 0, 1],
                "503776919131758bb7de7beb6c0ae24894f4ec042c26032890c29359216e21ba",
                "70c6e3e8ebee8dc4c0dbba66076819bb8c09672527c4277ca8729532ad711872218f826919f6b67218adde99018a6df9095ab2b58d803b5b93ec9802085a690e",
            ),
        ] {
            let child = root.derive_priv(&secp, &hardened(&path).unwrap()).unwrap();
            assert_eq!(hex::encode(child.private_key.secret_bytes()), key);
            assert_eq!(hex::encode(*bip85_entropy(&root, &path).unwrap()), entropy);
        }
        // HEX, 64 bytes, index 0: a longer path through the same derivation.
        assert_eq!(
            hex::encode(*bip85_entropy(&root, &[83_696_968, 128_169, 64, 0]).unwrap()),
            "492db4698cf3b73a5a24998aa3e9d7fa96275d85724a91e71aa2d645442f878555d078fd1f1f67e368976f04137b1f7a0d19232136ca50c44614af72b5582a5c"
        );
        // HD-Seed WIF, index 0: the application FreeBank uses, through FreeBank's own function. The
        // spec's derived entropy is the WIF's secret, and the WIF is the string sethdseed takes.
        assert_eq!(BIP85_PATH_TEXT, "m/83696968'/2'/0'");
        let k = hd_seed_from_root(&root).unwrap();
        assert_eq!(hex::encode(*k), "7040bb53104f27367f317558e78a994ada7296c6fde36a364e5baf206e502bb1");
        assert_eq!(wif(&k, Chain::Main).as_str(), "Kzyv4uF39d4Jrw2W7UryTHwZr1zQVNk4dAFyqE6BuMrMh1Za7uhp");
        // XPRV: chain code first, then the key (the vector's "derived entropy"), depth and numbers
        // zero; checks the xprv encoding.
        let e = bip85_entropy(&root, &[83_696_968, 32, 0]).unwrap();
        assert_eq!(hex::encode(&e[32..]), "ead0b33988a616cf6a497f1c169d9e92562604e38305ccd3fc96f2252c177682");
        let (cc, key): ([u8; 32], [u8; 32]) = (e[..32].try_into().unwrap(), e[32..].try_into().unwrap());
        assert_eq!(
            serialize_xprv(Chain::Main.xprv_version(), &cc, &key).as_str(),
            "xprv9s21ZrQH143K2srSbCSg4m4kLvPMzcWydgmKEnMmoZUurYuBuYG46c6P71UGXMzmriLzCCBvKQWBUv3vPB3m1SATMhp3uEjXHJ42jFg7myX"
        );
    }

    /// The FreeBank HD seed and its WIF for two BIP39 vectors' words, worked out independently (Python's
    /// hashlib and hmac, hardened BIP32 and base58 by hand, checked first against BIP85's and BIP39's
    /// published vectors): what a BIP85 tool shows as "WIF, index 0" for these words.
    #[test]
    fn freebank_seed_matches_an_independent_derivation() {
        for (entropy, want, want_wif) in [
            (
                "0000000000000000000000000000000000000000000000000000000000000000",
                "81c422304ab9e00fc7e9e3cb2070a8c25f7f133c99635ab245b3031a93988059",
                "L1ZxbPCLXScgvJpwWWRbouNHsqJ93d8Qpmnh5UnfXhzcdssCq9Hs",
            ),
            (
                "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "a457be59a31e721bebf5cb364641bd501ea0fbdcc2d505396a2b7a3d4b181214",
                "L2jAut3sSVhdEkbj9DLwgoq4JGiUN3GMPvK4fngxrX5EGifzvazf",
            ),
        ] {
            let seed = freebank_hd_seed(&unhex(entropy)).unwrap();
            assert_eq!(hex::encode(*seed), want);
            assert_eq!(wif(&seed, Chain::Main).as_str(), want_wif);
        }
    }

    /// What freebankd v0.2.16 itself reported for the first vector's words ("abandon … art"): given
    /// their BIP85 WIF with `sethdseed true "L1Zxb…"` on a main-network wallet, getwalletinfo's
    /// hdmasterkeyid, the first getnewaddress "" legacy (m/0'/0'/0'), the first getrawchangeaddress
    /// legacy (m/0'/1'/0'), and dumpwallet's "extended private masterkey". dumpwallet also printed the
    /// same WIF back on its hdmaster=1 line: the node's own export of its HD seed is the BIP85 WIF.
    #[test]
    fn matches_what_freebankd_reports() {
        let s = freebank_hd_seed(&[0u8; 32]).unwrap();
        assert_eq!(wif(&s, Chain::Main).as_str(), "L1ZxbPCLXScgvJpwWWRbouNHsqJ93d8Qpmnh5UnfXhzcdssCq9Hs");
        assert_eq!(key_id_hex(&key_id(&s).unwrap()), "fdb8f3fdd6e08d95ba16b69353ab7bd1885e8c6c");
        assert_eq!(address(&s, false, 0).unwrap(), "XUv5bxnYSS86fYX4wDMGmw4Wq55eUqypXg");
        assert_eq!(address(&s, true, 0).unwrap(), "XBkhUkqegMDR9V5NEjqJouoJrhNPfCkd1z");
        assert_eq!(
            master_xprv(&s, Chain::Main).unwrap().as_str(),
            "xprv9s21ZrQH143K42GpPhaqumQWobdt6mzmNSuJig7Y7ALBQuEvrDRQRBV2iKbP5hvZFKm5rSuFQUATrgDTJH3QUHXmZhN3tRtmHtaY1ctjmBG"
        );
    }

    #[test]
    fn wif_and_xprv_prefixes() {
        let s: [u8; 32] = unhex("81c422304ab9e00fc7e9e3cb2070a8c25f7f133c99635ab245b3031a93988059");
        let main = wif(&s, Chain::Main);
        let reg = wif(&s, Chain::Regtest);
        assert!(main.starts_with('K') || main.starts_with('L'), "{}", main.len());
        assert!(reg.starts_with('c'));
        let d = base58::decode_check(&main).unwrap();
        assert_eq!((d.len(), d[0], &d[1..33], d[33]), (34, 128, &s[..], 1));
        assert_eq!(base58::decode_check(&reg).unwrap()[0], 239);
        assert!(master_xprv(&s, Chain::Main).unwrap().starts_with("xprv"));
        assert!(master_xprv(&s, Chain::Regtest).unwrap().starts_with("tprv"));
        // The master xprv is BIP32's root of the 32 bytes.
        assert_eq!(master_xprv(&s, Chain::Main).unwrap().as_str(), Xpriv::new_master(NetworkKind::Main, &s).unwrap().to_string());
        assert_eq!(Chain::from_name("main"), Ok(Chain::Main));
        assert_eq!(Chain::from_name("regtest"), Ok(Chain::Regtest));
        assert!(Chain::from_name("test").is_err());
    }

    #[test]
    fn addresses_are_freebank_p2pkh() {
        let s: [u8; 32] = unhex("81c422304ab9e00fc7e9e3cb2070a8c25f7f133c99635ab245b3031a93988059");
        let a = address(&s, false, 0).unwrap();
        assert!(a.starts_with('X'), "{}", a);
        assert_eq!(base58::decode_check(&a).unwrap()[0], PUBKEY_ADDRESS);
        assert_ne!(a, address(&s, false, 1).unwrap());
        assert_ne!(a, address(&s, true, 0).unwrap());
        // Core's key id is shown reversed.
        let id = key_id(&s).unwrap();
        let shown = key_id_hex(&id);
        assert_eq!(shown.len(), 40);
        assert_eq!(hex::decode(&shown).unwrap().into_iter().rev().collect::<Vec<u8>>(), id.to_vec());
    }

    #[test]
    fn typed_words_are_checked() {
        let (_, phrase, _, _) = BIP39[2];
        let e = parse_words(phrase).unwrap();
        // Case, commas, extra spaces and numbering don't matter.
        let numbered: String = phrase.split(' ').enumerate().map(|(i, w)| format!("{}. {},\n", i + 1, w.to_uppercase())).collect();
        assert_eq!(*parse_words(&numbered).unwrap(), *e);
        assert_eq!(check_words(&numbered), WordsCheck { count: 24, unknown: vec![], ok: true, checksum_failed: false });

        // A word that isn't in the list, by its position.
        let bad = phrase.replacen("diagram", "diagrom", 1);
        assert!(parse_words(&bad).unwrap_err().contains("Word 2 "));
        assert_eq!(check_words(&bad), WordsCheck { count: 24, unknown: vec![2], ok: false, checksum_failed: false });
        // Two words swapped: all known, the checksum fails.
        let swapped = phrase.replacen("hamster diagram", "diagram hamster", 1);
        assert!(parse_words(&swapped).unwrap_err().contains("don't fit together"));
        assert_eq!(check_words(&swapped), WordsCheck { count: 24, unknown: vec![], ok: false, checksum_failed: true });
        // Too few, and twelve-word phrases: FreeBank's are 24.
        let short: Vec<&str> = phrase.split(' ').take(23).collect();
        assert!(parse_words(&short.join(" ")).unwrap_err().contains("24 words; these are 23"));
        assert_eq!(check_words(&short.join(" ")).count, 23);
        assert!(!check_words(&short.join(" ")).ok);
        assert!(parse_words("legal winner thank year wave sausage worth useful legal winner thank yellow").is_err());
        assert_eq!(check_words(""), WordsCheck { count: 0, unknown: vec![], ok: false, checksum_failed: false });
    }

    #[test]
    fn new_entropy_differs_each_time() {
        let a = new_entropy();
        let b = new_entropy();
        assert_ne!(*a, *b);
        assert_ne!(*a, [0u8; 32]);
        assert_eq!(words(&a).len(), 24);
        assert_eq!(*parse_words(&words(&a).join(" ")).unwrap(), *a);
    }

    fn sealed_file(pass: &str, kdf: Kdf) -> (Entropy, [u8; 20], Vec<u8>) {
        let e = new_entropy();
        let id = key_id(&freebank_hd_seed(&e).unwrap()).unwrap();
        let f = seal(&e, &id, pass, kdf).unwrap();
        (e, id, f)
    }

    #[test]
    fn seed_file_round_trip() {
        let (e, id, f) = sealed_file("correct horse", QUICK);
        assert_eq!(f.len(), FILE_LEN);
        assert_eq!(&f[..8], b"FBKSEED\0");
        // The key id is readable without the passphrase; the entropy isn't in the clear.
        assert_eq!(file_key_id(&f).unwrap(), id);
        assert!(!f.windows(32).any(|w| w == &e[..]));
        let (opened, oid) = open(&f, "correct horse").unwrap();
        assert_eq!((*opened, oid), (*e, id));
        // Each seal has its own salt and nonce.
        let again = seal(&e, &id, "correct horse", QUICK).unwrap();
        assert_ne!(f[22..78], again[22..78]);
        assert!(seal(&e, &id, "", QUICK).is_err());
    }

    #[test]
    fn seed_file_with_the_real_settings() {
        let (e, _, f) = sealed_file("correct horse", KDF);
        assert_eq!(u32::from_le_bytes(f[10..14].try_into().unwrap()), 65536);
        assert_eq!(*open(&f, "correct horse").unwrap().0, *e);
        assert_eq!(open(&f, "correct horse!").unwrap_err(), OpenError::WrongPassphrase);
    }

    #[test]
    fn seed_file_wrong_passphrase() {
        let (_, _, f) = sealed_file("correct horse", QUICK);
        for wrong in ["", "correct hors", "Correct horse", "correct horse "] {
            assert_eq!(open(&f, wrong).unwrap_err(), OpenError::WrongPassphrase, "{:?}", wrong);
        }
        let msg = OpenError::WrongPassphrase.for_ui();
        assert!(!msg.contains("correct"));
    }

    #[test]
    fn seed_file_tampering_is_caught() {
        let (_, _, f) = sealed_file("correct horse", QUICK);
        // Every byte of the header and the sealed part is covered: flip each one.
        for i in 0..FILE_LEN {
            let mut t = f.clone();
            t[i] ^= 0x01;
            assert!(open(&t, "correct horse").is_err(), "byte {} changed but the file still opened", i);
        }
        // A key id swapped for another is caught (the header is authenticated).
        let mut t = f.clone();
        t[78..98].copy_from_slice(&[7u8; 20]);
        assert_eq!(open(&t, "correct horse").unwrap_err(), OpenError::WrongPassphrase);
        // Truncated, lengthened, or not a seed file at all.
        assert!(matches!(open(&f[..FILE_LEN - 1], "correct horse"), Err(OpenError::Damaged(_))));
        let mut longer = f.clone();
        longer.push(0);
        assert!(matches!(open(&longer, "correct horse"), Err(OpenError::Damaged(_))));
        assert!(matches!(open(b"not a seed file", "correct horse"), Err(OpenError::Damaged(_))));
        assert!(matches!(file_key_id(b"FBKSEED"), Err(OpenError::Damaged(_))));
        // A header asking for 4 GiB of memory is refused before any work is done.
        let mut greedy = f.clone();
        greedy[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
        let started = std::time::Instant::now();
        assert!(matches!(open(&greedy, "correct horse"), Err(OpenError::Damaged(_))));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        // A newer version.
        let mut newer = f.clone();
        newer[8] = 2;
        assert!(open(&newer, "correct horse").unwrap_err().for_ui().contains("newer FreeBank"));
    }

    #[test]
    fn seed_file_on_disk() {
        let d = std::env::temp_dir().join(format!("fbseed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let path = seed_path(&d);
        assert_eq!(read_file(&path).unwrap(), None);
        let (e, _, f) = sealed_file("correct horse", QUICK);
        write_private(&path, &f).unwrap();
        assert_eq!(read_file(&path).unwrap().as_deref(), Some(&f[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
        // Replacing it leaves one file and no temporary ones behind.
        let (_, _, g) = sealed_file("other horse", QUICK);
        write_private(&path, &g).unwrap();
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["seed.enc".to_string()]);
        assert!(open(&read_file(&path).unwrap().unwrap(), "correct horse").is_err());
        assert_ne!(*open(&read_file(&path).unwrap().unwrap(), "other horse").unwrap().0, *e);
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// v0.2.5: turning "Approve sends on my phone" off with the recovery words takes this wallet's words.
    #[test]
    fn the_words_must_be_this_wallets() {
        let dir = std::env::temp_dir().join(format!("fb-words-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No sealed copy yet: nothing to check against.
        assert!(words_are_this_wallets(&dir, "abandon").is_err());
        let e = new_entropy();
        let id = key_id(&freebank_hd_seed(&e).unwrap()).unwrap();
        write_private(&seed_path(&dir), &seal(&e, &id, "correct horse", QUICK).unwrap()).unwrap();
        let mine = words(&e).join(" ");
        assert_eq!(words_are_this_wallets(&dir, &mine), Ok(true));
        let other = words(&new_entropy()).join(" ");
        assert_eq!(words_are_this_wallets(&dir, &other), Ok(false));
        assert_eq!(words_are_this_wallets(&dir, "not words at all"), Ok(false));
        // The node's form of the same key id, for the second check.
        assert_eq!(words_key_id_hex(&mine).unwrap(), Some(key_id_hex(&id)));
        assert_eq!(sealed_key_id_hex(&dir), Some(key_id_hex(&id)));
        assert_eq!(words_key_id_hex("not words").unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
