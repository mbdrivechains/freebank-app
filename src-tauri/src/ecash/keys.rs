//! The eCash wallets' keys, from the app's one set of 24 words (one app-wide seed).
//! Reworked after the v0.2.6 security review (H1, H2, L2): the eCash node gets only public keys and FreeBank signs.
//!
//! - **The eCash root** is BIP85's XPRV application at m/83696968'/32'/0' of the words' BIP32 root: a master key of its
//!   own, so the eCash keys are never those of a Bitcoin wallet made from the same words (any BIP85 tool's "XPRV,
//!   index 0" for the words gives it).
//! - Under it, BIP84: **the main eCash wallet** is account 0 (m/84'/c'/0'), **the bidding wallet** account 1, c being
//!   0 on eCash (betanet uses Bitcoin's formats: bc1 addresses) and 1 on regtest.
//!
//! The node gets each account as a public descriptor (`wpkh([root fingerprint/84h/ch/ah]xpub/branch/*)`), with the
//! BIP380 checksum worked out here, and watches it. Private keys exist only here, for the moment a payment is signed
//! (`sign.rs`): the main account's from the words, which the wallet passphrase opens; the bidding account's from a key
//! file only this user can read (bids go out with nobody there, by design).

use crate::seed::{bip85_entropy, hardened, wipe, words_root, Chain};
use bitcoin::bip32::{ChainCode, ChildNumber, DerivationPath, Fingerprint, Xpriv, Xpub};
use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::{Address, CompressedPublicKey, KnownHrp, NetworkKind, ScriptBuf};
use std::str::FromStr;
use zeroize::{Zeroize, Zeroizing};

/// Which of the app's eCash wallets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Account {
    Main,
    Bids,
    /// Another wallet's account, from words of Sparrow's or Electrum's (v0.7.3, `other.rs`): its path is its own.
    Other,
}

impl Account {
    pub fn index(self) -> u32 {
        match self {
            Account::Main | Account::Other => 0,
            Account::Bids => 1,
        }
    }

    /// The app's own accounts' path from the eCash root: m/84'/c'/a' (hardened). Another wallet's path is its own
    /// (`AccountKey::other`): asking for it here is refused, never answered with the main wallet's.
    fn origin(self, chain: Chain) -> Result<Vec<u32>, String> {
        match self {
            Account::Other => Err("Another wallet's path isn't one of FreeBank's own.".into()),
            _ => Ok(vec![84, coin(chain), self.index()]),
        }
    }
}

/// BIP44's coin type: Bitcoin's formats on eCash, testnet's on regtest.
pub fn coin(chain: Chain) -> u32 {
    match chain {
        Chain::Main => 0,
        Chain::Regtest | Chain::Test => 1,
    }
}

fn network_kind(chain: Chain) -> NetworkKind {
    match chain {
        Chain::Main => NetworkKind::Main,
        Chain::Regtest | Chain::Test => NetworkKind::Test,
    }
}

fn hrp(chain: Chain) -> KnownHrp {
    match chain {
        Chain::Main => KnownHrp::Mainnet,
        Chain::Regtest => KnownHrp::Regtest,
        Chain::Test => KnownHrp::Testnets,
    }
}

/// BIP85's XPRV application, index 0.
const ECASH_ROOT_PATH: [u32; 3] = [83_696_968, 32, 0];
/// The highest address index taken as this wallet's (re-review L-E): a node naming one far beyond would put coins where
/// a restore from the words, with any usual gap limit, never looks.
pub const MAX_INDEX: u32 = 100_000;

/// A BIP84 key in SLIP-132's form (v0.7.3, Sparrow and Electrum): zpub/zprv, or vpub/vprv with Bitcoin's test
/// prefixes. Electrum reads the script type (native segwit) from these version bytes; Sparrow takes them too.
fn slip132(data: &mut [u8; 78], kind: NetworkKind, private: bool) -> String {
    let v: u32 = match (kind, private) {
        (NetworkKind::Main, false) => 0x04b2_4746,
        (NetworkKind::Main, true) => 0x04b2_430c,
        (NetworkKind::Test, false) => 0x045f_1cf6,
        (NetworkKind::Test, true) => 0x045f_18bc,
    };
    data[..4].copy_from_slice(&v.to_be_bytes());
    let s = bitcoin::base58::encode_check(&data[..]);
    data.zeroize();
    s
}

/// The eCash root (a master key); wiped when dropped.
pub struct Root(Xpriv);

impl Drop for Root {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl Root {
    /// From the words' entropy (seed.enc, which the wallet passphrase opens).
    pub fn from_words(entropy: &[u8; 32], chain: Chain) -> Result<Root, String> {
        let mut words = words_root(entropy)?;
        let r = Self::from_words_root(&words, chain);
        wipe(&mut words);
        r
    }

    /// BIP85 XPRV: the entropy's first 32 bytes are the chain code, the next 32 the key.
    pub(crate) fn from_words_root(words: &Xpriv, chain: Chain) -> Result<Root, String> {
        let e = bip85_entropy(words, &ECASH_ROOT_PATH)?;
        let mut cc = [0u8; 32];
        cc.copy_from_slice(&e[..32]);
        let key = SecretKey::from_slice(&e[32..]).map_err(|_| "These recovery words give an eCash key FreeBank can't use.")?;
        Ok(Root(Xpriv {
            network: network_kind(chain),
            depth: 0,
            parent_fingerprint: Fingerprint::default(),
            child_number: ChildNumber::Normal { index: 0 },
            private_key: key,
            chain_code: ChainCode::from(cc),
        }))
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.0.fingerprint(&Secp256k1::new())
    }

    /// The eCash root as a master extended private key (xprv; tprv with test prefixes), for another wallet (v0.7.3):
    /// Sparrow's "Master Private Key (BIP32)" with native segwit's usual path gives the main eCash wallet, and any BIP85
    /// tool's "XPRV, index 0" from the words gives this same key.
    pub fn xprv(&self) -> Zeroizing<String> {
        Zeroizing::new(self.0.to_string())
    }

    /// An account's private key, with its public side.
    pub fn account(&self, chain: Chain, account: Account) -> Result<AccountKey, String> {
        let secp = Secp256k1::new();
        let origin = account.origin(chain)?;
        let path = hardened(&origin)?;
        let mut xprv = self.0.derive_priv(&secp, &path).map_err(|e| e.to_string())?;
        xprv.network = network_kind(chain);
        let public = AccountPub { fingerprint: self.fingerprint(), chain, account, xpub: Xpub::from_priv(&secp, &xprv), origin };
        Ok(AccountKey { xprv, public })
    }
}

/// An account's public side: what the node and ecash.json get.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountPub {
    /// The eCash root's fingerprint (the descriptors' and PSBTs' key origin).
    pub fingerprint: Fingerprint,
    pub chain: Chain,
    pub account: Account,
    pub xpub: Xpub,
    /// The account's path from the root, every step hardened: m/84'/c'/a' for the app's own (v0.7.3: m/0' for an
    /// Electrum segwit wallet's).
    pub origin: Vec<u32>,
}

impl AccountPub {
    /// m/84'/c'/a', as wallets ask for it.
    pub fn path(&self) -> String {
        let steps: Vec<String> = self.origin.iter().map(|i| format!("{i}'")).collect();
        format!("m/{}", steps.join("/"))
    }

    /// The account's public key as zpub (vpub with test prefixes), for a watch-only copy in Electrum or Sparrow.
    pub fn zpub(&self) -> String {
        slip132(&mut self.xpub.encode(), network_kind(self.chain), false)
    }

    /// m/84'/c'/a', as key origins write it.
    fn origin(&self) -> String {
        let steps: Vec<String> = self.origin.iter().map(|i| format!("{i}h")).collect();
        format!("[{}/{}]", self.fingerprint, steps.join("/"))
    }

    /// The public descriptor of a branch (0 receive, 1 change), checksummed, for `importdescriptors`.
    pub fn descriptor(&self, branch: u32) -> Result<String, String> {
        let body = format!("wpkh({}{}/{}/*)", self.origin(), self.xpub, branch);
        let sum = checksum(&body).ok_or("A descriptor had a character it can't hold.")?;
        Ok(format!("{}#{}", body, sum))
    }

    /// Receive and change in one descriptor (BIP389's `<0;1>`), as Sparrow imports and exports it (v0.7.3).
    pub fn descriptor_both(&self) -> Result<String, String> {
        let body = format!("wpkh({}{}/<0;1>/*)", self.origin(), self.xpub);
        let sum = checksum(&body).ok_or("A descriptor had a character it can't hold.")?;
        Ok(format!("{}#{}", body, sum))
    }

    pub fn pubkey(&self, branch: u32, index: u32) -> Result<CompressedPublicKey, String> {
        let secp = Secp256k1::new();
        let k = self
            .xpub
            .derive_pub(&secp, &[ChildNumber::Normal { index: branch }, ChildNumber::Normal { index }])
            .map_err(|e| e.to_string())?;
        Ok(CompressedPublicKey(k.public_key))
    }

    pub fn script(&self, branch: u32, index: u32) -> Result<ScriptBuf, String> {
        Ok(ScriptBuf::new_p2wpkh(&self.pubkey(branch, index)?.wpubkey_hash()))
    }

    pub fn address(&self, branch: u32, index: u32) -> Result<String, String> {
        Ok(Address::p2wpkh(&self.pubkey(branch, index)?, hrp(self.chain)).to_string())
    }

    /// (branch, index) when `path` is under this account from its root: m/84'/c'/a'/branch/index, branch 0 or 1.
    pub fn place(&self, fingerprint: Fingerprint, path: &DerivationPath) -> Option<(u32, u32)> {
        if fingerprint != self.fingerprint {
            return None;
        }
        let want = hardened(&self.origin).ok()?;
        let n = want.len();
        let p: Vec<ChildNumber> = path.into_iter().copied().collect();
        if p.len() != n + 2 || p[..n] != want[..] {
            return None;
        }
        match (p[n], p[n + 1]) {
            (ChildNumber::Normal { index: b }, ChildNumber::Normal { index: i }) if b <= 1 && i <= MAX_INDEX => Some((b, i)),
            _ => None,
        }
    }

    /// From a descriptor with a key origin, as listunspent's and getaddressinfo's `desc` give it
    /// ("wpkh([fp/84h/0h/1h/0/5]02ab…)#xyz"): (branch, index) when it is this account's and its key is the one derived
    /// here for that place.
    pub fn place_of_desc(&self, desc: &str) -> Option<(u32, u32)> {
        let inner = desc.strip_prefix("wpkh([")?;
        let (origin, rest) = inner.split_once(']')?;
        let key = rest.split(')').next()?;
        let (fp, path) = origin.split_once('/')?;
        let fp = Fingerprint::from_str(fp).ok()?;
        let path = DerivationPath::from_str(&format!("m/{}", path.replace('h', "'"))).ok()?;
        let (b, i) = self.place(fp, &path)?;
        (self.pubkey(b, i).ok()?.to_string() == key).then_some((b, i))
    }

    /// ecash.json's form: the xpub and the root fingerprint.
    pub fn to_record(&self) -> (String, String) {
        (self.xpub.to_string(), self.fingerprint.to_string())
    }

    pub fn from_record(xpub: &str, fingerprint: &str, chain: Chain, account: Account) -> Result<AccountPub, String> {
        Ok(AccountPub {
            fingerprint: Fingerprint::from_str(fingerprint).map_err(|e| e.to_string())?,
            chain,
            account,
            xpub: Xpub::from_str(xpub).map_err(|e| e.to_string())?,
            origin: account.origin(chain)?,
        })
    }
}

/// An account's private key, wiped when dropped.
pub struct AccountKey {
    xprv: Xpriv,
    pub public: AccountPub,
}

impl Drop for AccountKey {
    fn drop(&mut self) {
        wipe(&mut self.xprv);
    }
}

impl AccountKey {
    pub fn secret(&self, branch: u32, index: u32) -> Result<SecretKey, String> {
        let secp = Secp256k1::new();
        let mut k = self
            .xprv
            .derive_priv(&secp, &[ChildNumber::Normal { index: branch }, ChildNumber::Normal { index }])
            .map_err(|e| e.to_string())?;
        let s = k.private_key;
        wipe(&mut k);
        Ok(s)
    }

    /// The account's private key as zprv (vprv with test prefixes): Electrum's "Use a master key" makes this wallet
    /// from it (v0.7.3).
    pub fn zprv(&self) -> Zeroizing<String> {
        Zeroizing::new(slip132(&mut self.xprv.encode(), self.xprv.network, true))
    }

    /// The bidding key file's form: the account xprv and the root fingerprint.
    pub fn to_file(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("{} {}", self.xprv, self.public.fingerprint))
    }

    pub fn from_file(text: &str, chain: Chain, account: Account) -> Result<AccountKey, String> {
        let text = Zeroizing::new(text.trim().to_string());
        let (x, fp) = text.split_once(' ').ok_or("The bidding key file can't be read.")?;
        let xprv = Xpriv::from_str(x).map_err(|_| "The bidding key file can't be read.")?;
        let secp = Secp256k1::new();
        let public = AccountPub {
            fingerprint: Fingerprint::from_str(fp).map_err(|_| "The bidding key file can't be read.")?,
            chain,
            account,
            xpub: Xpub::from_priv(&secp, &xprv),
            origin: account.origin(chain)?,
        };
        Ok(AccountKey { xprv, public })
    }

    /// Another wallet's account (v0.7.3, `other.rs`): `origin` (hardened steps) from its own master key.
    pub fn other(master: &Xpriv, chain: Chain, origin: &[u32]) -> Result<AccountKey, String> {
        let secp = Secp256k1::new();
        let mut xprv = master.derive_priv(&secp, &hardened(origin)?).map_err(|e| e.to_string())?;
        xprv.network = network_kind(chain);
        let public = AccountPub {
            fingerprint: master.fingerprint(&secp),
            chain,
            account: Account::Other,
            xpub: Xpub::from_priv(&secp, &xprv),
            origin: origin.to_vec(),
        };
        Ok(AccountKey { xprv, public })
    }
}

/// BIP380's descriptor checksum (Core's `DescriptorChecksum`): eight characters, or None for a character outside
/// the descriptor alphabet.
pub fn checksum(desc: &str) -> Option<String> {
    const INPUT: &str =
        "0123456789()[],'/*abcdefgh@:$%{}IJKLMNOPQRSTUVWXYZ&+-.;<=>?!^_|~ijklmnopqrstuvwxyzABCDEFGH`#\"\\ ";
    const CHARS: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    fn polymod(c: u64, v: u64) -> u64 {
        const G: [u64; 5] = [0xf5dee51989, 0xa9fdca3312, 0x1bab10e32d, 0x3706b1677a, 0x644d626ffd];
        let top = c >> 35;
        let mut c = ((c & 0x7ffffffff) << 5) ^ v;
        for (i, g) in G.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                c ^= g;
            }
        }
        c
    }
    let (mut c, mut cls, mut count) = (1u64, 0u64, 0);
    for ch in desc.chars() {
        let pos = INPUT.find(ch)? as u64;
        c = polymod(c, pos & 31);
        cls = cls * 3 + (pos >> 5);
        count += 1;
        if count == 3 {
            c = polymod(c, cls);
            cls = 0;
            count = 0;
        }
    }
    if count > 0 {
        c = polymod(c, cls);
    }
    for _ in 0..8 {
        c = polymod(c, 0);
    }
    c ^= 1;
    Some((0..8).map(|j| CHARS[((c >> (5 * (7 - j))) & 31) as usize] as char).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BIP85's published root (its test vectors' master key).
    const BIP85_ROOT: &str = "xprv9s21ZrQH143K2LBWUUQRFXhucrQqBpKdRRxNVq2zBqsx8HVqFk2uYo8kmbaLLHRdqtQpUm98uKfu3vca1LqdGhUtyoFnCNkfmXRyPXLjbKb";

    #[test]
    fn the_ecash_root_is_bip85s_xprv_at_index_0() {
        // BIP85's published XPRV vector for this root.
        let words = Xpriv::from_str(BIP85_ROOT).unwrap();
        let r = Root::from_words_root(&words, Chain::Main).unwrap();
        assert_eq!(
            r.0.to_string(),
            "xprv9s21ZrQH143K2srSbCSg4m4kLvPMzcWydgmKEnMmoZUurYuBuYG46c6P71UGXMzmriLzCCBvKQWBUv3vPB3m1SATMhp3uEjXHJ42jFg7myX"
        );
    }

    /// BIP84's published vectors ("abandon" x11 "about", no passphrase): account 0's zprv and zpub, and its first
    /// address, as Electrum and Sparrow show them.
    #[test]
    fn slip132_keys_are_bip84s() {
        let seed = hex_to_bytes("5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4");
        let master = Xpriv::new_master(NetworkKind::Main, &seed).unwrap();
        let fake = Root(master);
        let acct = fake.account(Chain::Main, Account::Main).unwrap();
        assert_eq!(&*acct.zprv(), "zprvAdG4iTXWBoARxkkzNpNh8r6Qag3irQB8PzEMkAFeTRXxHpbF9z4QgEvBRmfvqWvGp42t42nvgGpNgYSJA9iefm1yYNZKEm7z6qUWCroSQnE");
        assert_eq!(acct.public.zpub(), "zpub6rFR7y4Q2AijBEqTUquhVz398htDFrtymD9xYYfG1m4wAcvPhXNfE3EfH1r1ADqtfSdVCToUG868RvUUkgDKf31mGDtKsAYz2oz2AGutZYs");
        assert_eq!(acct.public.address(0, 0).unwrap(), "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
        assert_eq!(acct.public.path(), "m/84'/0'/0'");
        // Test prefixes: vpub/vprv, and the xprv of the root is a tprv.
        let t = Root(Xpriv::new_master(NetworkKind::Test, &seed).unwrap());
        let ta = t.account(Chain::Test, Account::Main).unwrap();
        assert!(ta.public.zpub().starts_with("vpub") && ta.zprv().starts_with("vprv"));
        assert_eq!(ta.public.path(), "m/84'/1'/0'");
        assert!(t.xprv().starts_with("tprv") && fake.xprv().starts_with("xprv"));
    }

    fn hex_to_bytes(h: &str) -> Vec<u8> {
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn accounts_and_descriptors() {
        let words = Xpriv::from_str(BIP85_ROOT).unwrap();
        let r = Root::from_words_root(&words, Chain::Main).unwrap();
        let fp = r.fingerprint();
        let main = r.account(Chain::Main, Account::Main).unwrap();
        let bids = r.account(Chain::Main, Account::Bids).unwrap();
        assert_ne!(main.public.xpub, bids.public.xpub);
        let d = main.public.descriptor(0).unwrap();
        assert!(d.starts_with(&format!("wpkh([{}/84h/0h/0h]xpub", fp)), "{d}");
        assert!(d.contains("/0/*)#") && !d.contains("prv"), "public only: {d}");
        // The private side gives the same key as the public side, place by place.
        let secp = Secp256k1::new();
        for (b, i) in [(0, 0), (1, 7)] {
            let pk = main.public.pubkey(b, i).unwrap();
            assert_eq!(pk.0, main.secret(b, i).unwrap().public_key(&secp));
        }
        // Places: this account's paths, nothing else.
        let p = DerivationPath::from_str("m/84'/0'/0'/1/7").unwrap();
        assert_eq!(main.public.place(fp, &p), Some((1, 7)));
        assert_eq!(bids.public.place(fp, &p), None, "another account");
        assert_eq!(main.public.place(Fingerprint::default(), &p), None, "another root");
        assert_eq!(main.public.place(fp, &DerivationPath::from_str("m/84'/0'/0'/2/7").unwrap()), None);
        let desc = format!("wpkh([{}/84h/0h/0h/1/7]{})#abc", fp, main.public.pubkey(1, 7).unwrap());
        assert_eq!(main.public.place_of_desc(&desc), Some((1, 7)));
        let wrong = format!("wpkh([{}/84h/0h/0h/1/7]{})#abc", fp, main.public.pubkey(1, 8).unwrap());
        assert_eq!(main.public.place_of_desc(&wrong), None, "a key that isn't the one at that place");
        // The bidding key file round-trips; the record too.
        let back = AccountKey::from_file(&bids.to_file(), Chain::Main, Account::Bids).unwrap();
        assert_eq!(back.public, bids.public);
        let (x, f) = main.public.to_record();
        assert_eq!(AccountPub::from_record(&x, &f, Chain::Main, Account::Main).unwrap(), main.public);
        // Regtest: coin type 1, tpub, bcrt1.
        let rt = Root::from_words_root(&words, Chain::Regtest).unwrap().account(Chain::Regtest, Account::Main).unwrap();
        assert!(rt.public.descriptor(0).unwrap().contains("/84h/1h/0h]tpub"));
        assert!(rt.public.address(0, 0).unwrap().starts_with("bcrt1q"));
    }

    #[test]
    fn the_ecash_keys_are_not_bitcoins_for_the_same_words() {
        // BIP84's vector words: their Bitcoin account 0 address must not be the eCash wallet's (security review L2).
        let m = bip39::Mnemonic::parse_in(
            bip39::Language::English,
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
        )
        .unwrap();
        let words = Xpriv::new_master(NetworkKind::Main, &m.to_seed_normalized("")).unwrap();
        let main = Root::from_words_root(&words, Chain::Main).unwrap().account(Chain::Main, Account::Main).unwrap();
        assert_ne!(main.public.address(0, 0).unwrap(), "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
    }

    #[test]
    fn checksums_match_bip380() {
        assert_eq!(checksum("raw(deadbeef)").as_deref(), Some("89f8spxm"));
        assert_eq!(checksum("addr(mkmZxiEcEd8ZqjQWVZuC6so5dFMKEFpN2j)").as_deref(), Some("02wpgw69"));
        assert_eq!(checksum("raw(é)"), None);
    }
}
