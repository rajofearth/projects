//! What makes a downloaded exe trustworthy: a signature file next to it on the release, signed
//! with the release key whose public half is compiled in below. The signed message names the
//! version and the exe's SHA-256, so a signature can't be moved to another file or replayed
//! under a newer version number to downgrade an install.
//!
//! The `release-sign` tool includes this file too, so CI signs exactly what the app checks.

use ed25519_dalek::{Signature, VerifyingKey};

/// The release key's public half. The private half lives only in the repository's
/// `RELEASE_SIGNING_KEY` secret; `release-sign verify` in CI fails the release if they differ.
pub const PUBLIC_KEY: [u8; 32] = [0x78, 0x1a, 0x0e, 0x4f, 0x7d, 0x18, 0xf3, 0xd4, 0x90, 0x6f, 0x19, 0x78, 0xd0, 0xc6, 0xfd, 0xd1, 0x3a, 0x63, 0x31, 0x74, 0xf4, 0x6c, 0xbf, 0x2b, 0x28, 0x97, 0x5c, 0x0d, 0x61, 0x5d, 0x02, 0xe5];

/// The release asset names. Version-less, so
/// `releases/latest/download/projects-windows-x64.exe` always serves the newest build.
pub const ASSET_EXE: &str = "projects-windows-x64.exe";
pub const ASSET_SIG: &str = "projects-windows-x64.exe.sig";

/// The bytes the signature covers.
pub fn message(version: &str, sha256_hex: &str) -> String {
    format!("projects-update-v1\n{version}\n{sha256_hex}\n")
}

/// A `.sig` file: three lines, the version, the exe's SHA-256 in lowercase hex, and the
/// Ed25519 signature over [`message`] in lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigFile {
    pub version: String,
    pub sha256: String,
    pub signature: [u8; 64],
}

impl SigFile {
    pub fn parse(text: &str) -> Result<SigFile, String> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let (Some(version), Some(sha256), Some(sig), None) = (lines.next(), lines.next(), lines.next(), lines.next()) else {
            return Err("the signature file isn't three lines".into());
        };
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the signature file's checksum isn't SHA-256 hex".into());
        }
        let bytes = from_hex(sig).filter(|b| b.len() == 64).ok_or("the signature isn't 64 bytes of hex")?;
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&bytes);
        Ok(SigFile { version: version.to_string(), sha256: sha256.to_ascii_lowercase(), signature })
    }

    #[allow(dead_code)] // the release-sign tool writes .sig files with it
    pub fn render(&self) -> String {
        format!("{}\n{}\n{}\n", self.version, self.sha256, to_hex(&self.signature))
    }

    /// Checks the signature against `key`. Strict verification rejects the malleable and
    /// small-order encodings plain verification lets through.
    pub fn verify(&self, key: &[u8; 32]) -> Result<(), String> {
        let key = VerifyingKey::from_bytes(key).map_err(|_| "the release key is malformed".to_string())?;
        key.verify_strict(message(&self.version, &self.sha256).as_bytes(), &Signature::from_bytes(&self.signature))
            .map_err(|_| "the signature doesn't match the release key".to_string())
    }
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(key: &SigningKey, version: &str, sha: &str) -> SigFile {
        let sig = key.sign(message(version, sha).as_bytes());
        SigFile { version: version.into(), sha256: sha.into(), signature: sig.to_bytes() }
    }

    #[test]
    fn a_signature_round_trips_through_its_file_and_verifies() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let file = signed(&key, "0.2.0", &"ab".repeat(32));
        let back = SigFile::parse(&file.render()).unwrap();
        assert_eq!(back, file);
        assert!(back.verify(&key.verifying_key().to_bytes()).is_ok());
    }

    #[test]
    fn a_changed_version_or_checksum_fails_and_so_does_another_key() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut file = signed(&key, "0.2.0", &"ab".repeat(32));
        let public = key.verifying_key().to_bytes();
        file.version = "9.0.0".into();
        assert!(file.verify(&public).is_err());
        let mut file = signed(&key, "0.2.0", &"ab".repeat(32));
        file.sha256 = "cd".repeat(32);
        assert!(file.verify(&public).is_err());
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert!(signed(&key, "0.2.0", &"ab".repeat(32)).verify(&other).is_err());
    }

    #[test]
    fn malformed_files_are_refused() {
        assert!(SigFile::parse("0.2.0\nabc\n00").is_err());
        assert!(SigFile::parse(&format!("0.2.0\n{}\nzz", "ab".repeat(32))).is_err());
        assert!(SigFile::parse(&format!("0.2.0\n{}\n{}\nextra", "ab".repeat(32), "00".repeat(64))).is_err());
    }
}
