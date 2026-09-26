//! FreeBank's release key, and the check that a release's SHA256SUMS was signed with it.
//!
//! Every release from v0.2.16 on carries SHA256SUMS.sig, made with
//! `ssh-keygen -Y sign -f <key> -n file SHA256SUMS`. The same check on the shell:
//! `ssh-keygen -Y verify -f allowed -I freebank-release -n file -s SHA256SUMS.sig < SHA256SUMS`.

use ssh_key::{HashAlg, PublicKey, SshSig};

/// The release signing key, pinned here and never fetched. Its fingerprint is
/// SHA256:1d0zm9Qb9ZtzDnQHH593fgjAkk7nPqMDG79XyWlyeeY; it is also published at
/// api.github.com/users/mblowes/ssh_signing_keys.
pub const RELEASE_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAi2C9Lpi3gHPva6tlbLE+wdF1Cer3uUnmwZYr6SeRjR";

/// The `-n` the releases are signed with. A signature made for anything else doesn't count.
pub const NAMESPACE: &str = "file";

/// Is `sig` (the armored SHA256SUMS.sig) a good signature by the release key over exactly `sums`?
/// On failure, a few plain words on why.
pub fn verify_sums(sums: &[u8], sig: &[u8]) -> Result<(), &'static str> {
    verify_with(RELEASE_KEY, sums, sig)
}

fn verify_with(key: &str, sums: &[u8], sig: &[u8]) -> Result<(), &'static str> {
    let key = PublicKey::from_openssh(key).map_err(|_| "the release key doesn't parse")?;
    let sig = SshSig::from_pem(sig).map_err(|_| "the signature file can't be read")?;
    if sig.public_key() != key.key_data() {
        return Err("it was signed by a different key");
    }
    if sig.namespace() != NAMESPACE {
        return Err("it was signed for another use");
    }
    // Reading the signature already refuses other hashes; this keeps it so.
    if !matches!(sig.hash_alg(), HashAlg::Sha512 | HashAlg::Sha256) {
        return Err("it uses a hash this app doesn't know");
    }
    key.verify(NAMESPACE, sums, &sig)
        .map_err(|_| "the signature doesn't match the checksums")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &[u8] = include_bytes!("../../testdata/v0.2.16/SHA256SUMS");
    const SIG: &[u8] = include_bytes!("../../testdata/v0.2.16/SHA256SUMS.sig");
    // A throwaway key (its private half was deleted) that signed the same SHA256SUMS.
    const OTHER_KEY: &str = include_str!("../../testdata/other-key/other.pub");
    const OTHER_SIG: &[u8] = include_bytes!("../../testdata/other-key/SHA256SUMS.sig");
    const OTHER_SIG_GIT: &[u8] = include_bytes!("../../testdata/other-key/SHA256SUMS.git.sig");
    const OTHER_SIG_SHA256: &[u8] = include_bytes!("../../testdata/other-key/SHA256SUMS.sha256.sig");

    #[test]
    fn pinned_key_is_the_published_one() {
        let key = PublicKey::from_openssh(RELEASE_KEY).unwrap();
        assert_eq!(
            key.fingerprint(HashAlg::Sha256).to_string(),
            "SHA256:1d0zm9Qb9ZtzDnQHH593fgjAkk7nPqMDG79XyWlyeeY"
        );
    }

    #[test]
    fn real_release_passes() {
        assert_eq!(verify_sums(SUMS, SIG), Ok(()));
    }

    #[test]
    fn changed_sums_fail() {
        let mut sums = SUMS.to_vec();
        sums[0] ^= 1;
        assert!(verify_sums(&sums, SIG).is_err());
        // One more byte counts too.
        let mut sums = SUMS.to_vec();
        sums.push(b'\n');
        assert!(verify_sums(&sums, SIG).is_err());
        assert!(verify_sums(b"", SIG).is_err());
    }

    #[test]
    fn other_key_fails() {
        assert_eq!(verify_sums(SUMS, OTHER_SIG), Err("it was signed by a different key"));
        assert_eq!(verify_sums(SUMS, OTHER_SIG_SHA256), Err("it was signed by a different key"));
        // The same signatures are good for their own key (so it is the key that failed above),
        // and sha256 signatures are accepted as well as ssh-keygen's default sha512.
        assert_eq!(verify_with(OTHER_KEY, SUMS, OTHER_SIG), Ok(()));
        assert_eq!(verify_with(OTHER_KEY, SUMS, OTHER_SIG_SHA256), Ok(()));
    }

    #[test]
    fn wrong_namespace_fails() {
        // Signed with -n git by the key we check against: only the namespace is wrong.
        assert_eq!(verify_with(OTHER_KEY, SUMS, OTHER_SIG_GIT), Err("it was signed for another use"));
        // The real signature with its namespace edited ("file" -> "fild") fails too.
        // (Re-armoring it unedited still passes, so the edit is what fails.)
        assert_eq!(verify_sums(SUMS, &armor(&unarmor(SIG))), Ok(()));
        let mut blob = unarmor(SIG);
        let at = blob.windows(8).position(|w| w == b"\0\0\0\x04file").unwrap() + 7;
        blob[at] = b'd';
        assert_eq!(verify_sums(SUMS, &armor(&blob)), Err("it was signed for another use"));
    }

    #[test]
    fn garbage_fails() {
        assert!(verify_sums(SUMS, b"").is_err());
        assert!(verify_sums(SUMS, b"not a signature").is_err());
        assert!(verify_sums(SUMS, &SIG[..SIG.len() / 2]).is_err());
        let pem = b"-----BEGIN SSH SIGNATURE-----\nAAAA\n-----END SSH SIGNATURE-----\n";
        assert!(verify_sums(SUMS, pem).is_err());
        // The signature bytes themselves flipped.
        let mut blob = unarmor(SIG);
        let last = blob.len() - 1;
        blob[last] ^= 1;
        assert_eq!(verify_sums(SUMS, &armor(&blob)), Err("the signature doesn't match the checksums"));
    }

    /// The raw sshsig blob inside an armored signature.
    fn unarmor(sig: &[u8]) -> Vec<u8> {
        use base64::Engine;
        let text = std::str::from_utf8(sig).unwrap();
        let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
        base64::engine::general_purpose::STANDARD.decode(body).unwrap()
    }

    /// Armor a blob as ssh-keygen does (70 characters a line).
    fn armor(blob: &[u8]) -> Vec<u8> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(blob);
        let mut out = String::from("-----BEGIN SSH SIGNATURE-----\n");
        for line in b64.as_bytes().chunks(70) {
            out.push_str(std::str::from_utf8(line).unwrap());
            out.push('\n');
        }
        out.push_str("-----END SSH SIGNATURE-----\n");
        out.into_bytes()
    }
}
