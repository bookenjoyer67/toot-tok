//! Password hashing (argon2id per ARCHITECTURE.md §8).
//! Column comment `-- argon2id` in migration 0002 refers to this format.
//! Also hosts opaque-token hashing (sessions, email tokens) so the raw token
//! is the only plaintext that ever lives outside the DB.

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use sha2::{Digest, Sha256};

/// Hash a plaintext password into a PHC-format argon2id string.
pub fn hash_password(plaintext: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(plaintext.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Constant-time verification of a plaintext against a stored PHC hash.
pub fn verify_password(plaintext: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .map(|parsed| {
            Argon2::default()
                .verify_password(plaintext.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Hex-encoded SHA-256 of an opaque random token. Sessions and email tokens
/// store only this hash; the plaintext token is handed to the client / mailed
/// once and cannot be recovered from the DB.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trip() {
        let h = hash_password("hunter2").unwrap();
        assert!(h.starts_with("$argon2id$"), "must be argon2id PHC: {h}");
        assert!(verify_password("hunter2", &h));
        assert!(!verify_password("hunter3", &h));
    }
}
