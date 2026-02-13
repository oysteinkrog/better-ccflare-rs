//! PKCE (Proof Key for Code Exchange) implementation — RFC 7636.
//!
//! Generates a cryptographic verifier and its SHA-256 challenge for
//! OAuth 2.0 authorization code flows.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};

/// A PKCE challenge/verifier pair.
#[derive(Debug, Clone)]
pub struct PkceChallenge {
    /// The random verifier string (base64url, 43 chars).
    pub verifier: String,
    /// SHA-256 hash of the verifier (base64url encoded).
    pub challenge: String,
}

/// Generate a PKCE challenge/verifier pair.
///
/// Uses 32 bytes (256 bits) of cryptographically secure random data,
/// base64url-encoded (no padding) to produce the verifier. The challenge
/// is the SHA-256 hash of the verifier, also base64url-encoded.
pub fn generate() -> PkceChallenge {
    // 32 bytes of random data
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);

    let verifier = URL_SAFE_NO_PAD.encode(bytes);

    // SHA-256 hash of the verifier bytes (not the raw random bytes)
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();

    let challenge = URL_SAFE_NO_PAD.encode(hash);

    PkceChallenge {
        verifier,
        challenge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_generates_valid_pair() {
        let pkce = generate();

        // Verifier: 32 bytes → 43 chars base64url (no padding)
        assert_eq!(pkce.verifier.len(), 43);

        // Challenge: SHA-256 hash → 32 bytes → 43 chars base64url
        assert_eq!(pkce.challenge.len(), 43);

        // Verifier and challenge must differ
        assert_ne!(pkce.verifier, pkce.challenge);
    }

    #[test]
    fn pkce_challenge_is_sha256_of_verifier() {
        let pkce = generate();

        // Recompute challenge from verifier
        let mut hasher = Sha256::new();
        hasher.update(pkce.verifier.as_bytes());
        let hash = hasher.finalize();
        let expected = URL_SAFE_NO_PAD.encode(hash);

        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn pkce_is_unique() {
        let a = generate();
        let b = generate();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.challenge, b.challenge);
    }

    #[test]
    fn pkce_verifier_is_url_safe() {
        let pkce = generate();
        // base64url: only A-Z, a-z, 0-9, -, _
        assert!(pkce
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert!(pkce
            .challenge
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }
}
