//! Actor key management: RSA-2048 keypairs generated at signup, encoded as
//! PKCS#8 PEM (ActivityPub publishes the SPKI `BEGIN PUBLIC KEY` form; the
//! private key stays server-side for HTTP-signature egress in Phase 5).

use anyhow::Result;
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey};
use rsa::RsaPrivateKey;

/// Generate a fresh 2048-bit RSA keypair, returning `(public_key_pem,
/// private_key_pem)` in PKCS#8 PEM. Callers persist both on the `actors` row;
/// `private_key_pem` must never be exposed.
pub fn generate_actor_keypair() -> Result<(String, String)> {
    let mut rng = rand::rngs::OsRng;
    let private = RsaPrivateKey::new(&mut rng, 2048)?;
    let public = rsa::RsaPublicKey::from(&private);
    let public_pem = public.to_public_key_pem(rsa::pkcs8::LineEnding::LF)?;
    let private_pem = private.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)?;
    Ok((public_pem, String::from(&*private_pem)))
}
