//! Sealing: anonymous sealed boxes (X25519 + XSalsa20-Poly1305) to the
//! controller's key. The API holds only the public key, so it can store admin
//! secrets but never read them back. The controller seals instance material
//! to its own key the same way. Every value is bound to what it was sealed
//! for, so a sealed value moved to another row does not open.

use crypto_box::aead::OsRng;
use crypto_box::{PublicKey, SecretKey};
use hermes_core::app::error::AppError;
use hermes_core::app::secrets::SecretSealer;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::secrets::SecretKind;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The binding of an admin secret: its name and kind.
fn secret_frame(name: &str, kind: SecretKind) -> String {
    format!("hermes-admin-secret-v1\0{name}\0{}\0", kind.as_str())
}

/// The binding of instance material: its instance and purpose.
pub(crate) fn material_frame(instance: Uuid, purpose: MaterialPurpose) -> String {
    format!("hermes-material-v1\0{instance}\0{}\0", purpose.as_str())
}

/// The API side: seals to the controller's 32-byte X25519 public key.
pub struct PublicSealer {
    key: PublicKey,
}

impl PublicSealer {
    pub fn new(public_key: [u8; 32]) -> Self {
        Self {
            key: PublicKey::from(public_key),
        }
    }

    pub(crate) fn seal_framed(&self, frame: &str, value: &[u8]) -> Result<Vec<u8>, AppError> {
        let mut plaintext = Zeroizing::new(frame.as_bytes().to_vec());
        plaintext.extend_from_slice(value);
        self.key
            .seal(&mut OsRng, &plaintext)
            .map_err(|_| AppError::unavailable())
    }
}

impl SecretSealer for PublicSealer {
    fn seal(&self, name: &str, kind: SecretKind, value: &[u8]) -> Result<Vec<u8>, AppError> {
        self.seal_framed(&secret_frame(name, kind), value)
    }
}

/// The controller side: opens values only for what they were sealed for.
pub struct SecretOpener {
    key: SecretKey,
}

impl SecretOpener {
    pub fn new(secret_key: [u8; 32]) -> Self {
        Self {
            key: SecretKey::from(secret_key),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        *self.key.public_key().as_bytes()
    }

    pub fn open(
        &self,
        name: &str,
        kind: SecretKind,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, AppError> {
        self.open_framed(&secret_frame(name, kind), sealed)
    }

    pub(crate) fn open_framed(
        &self,
        frame: &str,
        sealed: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, AppError> {
        let plaintext = Zeroizing::new(
            self.key
                .unseal(sealed)
                .map_err(|_| AppError::unavailable())?,
        );
        let prefix = frame.as_bytes();
        if !plaintext.starts_with(prefix) {
            return Err(AppError::unavailable());
        }
        Ok(Zeroizing::new(plaintext[prefix.len()..].to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opener() -> SecretOpener {
        SecretOpener::new([7; 32])
    }

    #[test]
    fn only_the_controller_key_opens_and_only_for_the_same_secret() {
        let opener = opener();
        let sealer = PublicSealer::new(opener.public_key());
        let sealed = sealer.seal("llm-key", SecretKind::Env, b"sk-test").unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"sk-test"));
        assert_eq!(
            &*opener.open("llm-key", SecretKind::Env, &sealed).unwrap(),
            b"sk-test"
        );
        assert!(opener.open("other-key", SecretKind::Env, &sealed).is_err());
        assert!(opener.open("llm-key", SecretKind::File, &sealed).is_err());
        assert!(
            SecretOpener::new([8; 32])
                .open("llm-key", SecretKind::Env, &sealed)
                .is_err()
        );
        let again = sealer.seal("llm-key", SecretKind::Env, b"sk-test").unwrap();
        assert_ne!(sealed, again, "sealing is randomized");
    }
}
