//! API key cryptographic utilities — scrypt hashing and constant-time comparison.
//!
//! Parameters match the Node.js/Bun scrypt defaults exactly for database compatibility:
//! - N = 16384 (2^14)
//! - r = 8
//! - p = 1
//! - Key length = 64 bytes
//! - Salt = 16 random bytes
//! - Storage format: `<32-hex-salt>:<128-hex-hash>`

use rand::RngCore;
use scrypt::scrypt;
use subtle::ConstantTimeEq;

/// scrypt parameters matching Node.js defaults.
const SCRYPT_LOG_N: u8 = 14; // N = 2^14 = 16384
const SCRYPT_R: u32 = 8;
const SCRYPT_P: u32 = 1;
const SCRYPT_KEY_LEN: usize = 64;
const SALT_LEN: usize = 16;

/// Generate a random API key: 32 random bytes encoded as 64 hex characters.
pub fn generate_api_key() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(&bytes)
}

/// Hash an API key using scrypt with a random salt.
///
/// Returns the hash in the format `<salt_hex>:<hash_hex>` where:
/// - salt_hex is 32 hex chars (16 bytes)
/// - hash_hex is 128 hex chars (64 bytes)
pub fn hash_api_key(api_key: &str) -> Result<String, ScryptError> {
    let mut salt = [0u8; SALT_LEN];
    rand::rng().fill_bytes(&mut salt);
    hash_api_key_with_salt(api_key, &salt)
}

/// Hash an API key with a specific salt (used for testing).
fn hash_api_key_with_salt(api_key: &str, salt: &[u8]) -> Result<String, ScryptError> {
    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEY_LEN)
        .map_err(|_| ScryptError::InvalidParams("Failed to create scrypt params".to_string()))?;

    let mut output = vec![0u8; SCRYPT_KEY_LEN];
    scrypt(api_key.as_bytes(), salt, &params, &mut output)
        .map_err(|_| ScryptError::HashFailed("scrypt hash failed".to_string()))?;

    Ok(format!("{}:{}", hex::encode(salt), hex::encode(&output)))
}

/// Verify an API key against a stored scrypt hash using constant-time comparison.
///
/// The stored hash must be in the format `<salt_hex>:<hash_hex>`.
///
/// Also supports legacy SHA-256 hashes (plain hex, no colon) for migration.
pub fn verify_api_key(api_key: &str, stored_hash: &str) -> Result<bool, ScryptError> {
    // Check if this is a legacy SHA-256 hash (no colon separator)
    if !stored_hash.contains(':') {
        return verify_sha256_legacy(api_key, stored_hash);
    }

    let (salt_hex, hash_hex) = stored_hash
        .split_once(':')
        .ok_or_else(|| ScryptError::InvalidFormat("Missing ':' separator".to_string()))?;

    let salt = hex::decode(salt_hex)
        .map_err(|_| ScryptError::InvalidFormat("Invalid salt hex".to_string()))?;

    let stored_hash_bytes = hex::decode(hash_hex)
        .map_err(|_| ScryptError::InvalidFormat("Invalid hash hex".to_string()))?;

    if stored_hash_bytes.len() != SCRYPT_KEY_LEN {
        return Ok(false);
    }

    let params = scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P, SCRYPT_KEY_LEN)
        .map_err(|_| ScryptError::InvalidParams("Failed to create scrypt params".to_string()))?;

    let mut candidate = vec![0u8; SCRYPT_KEY_LEN];
    scrypt(api_key.as_bytes(), &salt, &params, &mut candidate)
        .map_err(|_| ScryptError::HashFailed("scrypt verification failed".to_string()))?;

    // Constant-time comparison
    Ok(candidate.ct_eq(&stored_hash_bytes).into())
}

/// Verify against a legacy SHA-256 hash (pre-scrypt migration).
fn verify_sha256_legacy(api_key: &str, stored_hash: &str) -> Result<bool, ScryptError> {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    // Constant-time comparison even for legacy hashes
    let a = hash.as_bytes();
    let b = stored_hash.as_bytes();
    if a.len() != b.len() {
        return Ok(false);
    }
    Ok(a.ct_eq(b).into())
}

/// Get the last 8 characters of an API key for display.
pub fn key_suffix(api_key: &str) -> String {
    if api_key.len() >= 8 {
        api_key[api_key.len() - 8..].to_string()
    } else {
        api_key.to_string()
    }
}

/// Hex encoding/decoding helpers (avoiding an extra dependency).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, ()> {
        if s.len() % 2 != 0 {
            return Err(());
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScryptError {
    #[error("Invalid scrypt parameters: {0}")]
    InvalidParams(String),
    #[error("Hash operation failed: {0}")]
    HashFailed(String),
    #[error("Invalid hash format: {0}")]
    InvalidFormat(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_api_key_length() {
        let key = generate_api_key();
        assert_eq!(key.len(), 64, "API key should be 64 hex chars (32 bytes)");
        // Verify it's valid hex
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generate_api_key_unique() {
        let k1 = generate_api_key();
        let k2 = generate_api_key();
        assert_ne!(k1, k2, "Two generated keys should be different");
    }

    #[test]
    fn hash_format() {
        let key = "test-api-key-12345678";
        let hash = hash_api_key(key).unwrap();

        // Should be salt:hash format
        let parts: Vec<&str> = hash.split(':').collect();
        assert_eq!(parts.len(), 2, "Hash should have salt:hash format");
        assert_eq!(parts[0].len(), 32, "Salt should be 32 hex chars (16 bytes)");
        assert_eq!(
            parts[1].len(),
            128,
            "Hash should be 128 hex chars (64 bytes)"
        );
    }

    #[test]
    fn hash_and_verify() {
        let key = "my-secret-api-key";
        let hash = hash_api_key(key).unwrap();
        assert!(verify_api_key(key, &hash).unwrap());
    }

    #[test]
    fn verify_wrong_key_fails() {
        let key = "correct-key";
        let hash = hash_api_key(key).unwrap();
        assert!(!verify_api_key("wrong-key", &hash).unwrap());
    }

    #[test]
    fn verify_consistent_with_known_salt() {
        let key = "test-key";
        let salt = [0u8; SALT_LEN]; // all-zeros salt for deterministic test
        let hash = hash_api_key_with_salt(key, &salt).unwrap();

        // Verify the same key + salt gives the same hash
        let hash2 = hash_api_key_with_salt(key, &salt).unwrap();
        assert_eq!(hash, hash2, "Same key + salt should produce same hash");

        // And it verifies
        assert!(verify_api_key(key, &hash).unwrap());
    }

    #[test]
    fn legacy_sha256_verify() {
        use sha2::{Digest, Sha256};

        let key = "legacy-key";
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        // Legacy hash has no colon — should be verified via SHA-256 path
        assert!(verify_api_key(key, &hash).unwrap());
        assert!(!verify_api_key("wrong-key", &hash).unwrap());
    }

    #[test]
    fn key_suffix_normal() {
        assert_eq!(key_suffix("abcdef1234567890"), "34567890");
    }

    #[test]
    fn key_suffix_short() {
        assert_eq!(key_suffix("abc"), "abc");
    }

    #[test]
    fn verify_invalid_format() {
        // Valid format but wrong data
        assert!(!verify_api_key("key", "not-hex:also-not-hex").unwrap_or(false));
    }
}
