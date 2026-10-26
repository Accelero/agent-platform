//! The controller's vault: generates instance material, seals it to the
//! controller's own key and opens material and admin secrets.

use hermes_core::app::controller::{NewMaterial, Plain, Vault};
use hermes_core::app::error::AppError;
use hermes_core::domain::materials::MaterialPurpose;
use hermes_core::domain::secrets::SecretKind;
use sha2::{Digest, Sha256};
use ssh_key::private::{Ed25519Keypair, KeypairData};
use ssh_key::{LineEnding, PrivateKey};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::sealing::{PublicSealer, SecretOpener, material_frame};

/// Fills `bytes` from the operating system's random source.
pub fn fill_random(bytes: &mut [u8]) -> Result<(), AppError> {
    getrandom::fill(bytes).map_err(|_| AppError::unavailable())
}

/// 32 random bytes as 64 lowercase hexadecimal characters.
pub fn random_token() -> Result<Zeroizing<String>, AppError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    fill_random(&mut *bytes)?;
    Ok(Zeroizing::new(hex(&*bytes)))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest the execution API looks a credential up by.
pub fn credential_digest(token: &[u8]) -> String {
    hex(&Sha256::digest(token))
}

/// A new Ed25519 key: the OpenSSH private key and its public line.
pub fn ed25519_pair() -> Result<(Zeroizing<String>, String), AppError> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *seed).map_err(|_| AppError::unavailable())?;
    let key = PrivateKey::new(KeypairData::Ed25519(Ed25519Keypair::from_seed(&seed)), "")
        .map_err(|_| AppError::unavailable())?;
    let private = key
        .to_openssh(LineEnding::LF)
        .map_err(|_| AppError::unavailable())?;
    let public = key
        .public_key()
        .to_openssh()
        .map_err(|_| AppError::unavailable())?;
    Ok((Zeroizing::new(private.to_string()), public))
}

pub struct ControllerVault {
    opener: SecretOpener,
    sealer: PublicSealer,
}

impl ControllerVault {
    pub fn new(secret_key: [u8; 32]) -> Self {
        let opener = SecretOpener::new(secret_key);
        let sealer = PublicSealer::new(opener.public_key());
        Self { opener, sealer }
    }
}

impl Vault for ControllerVault {
    fn generate(&self, instance: Uuid, purpose: MaterialPurpose) -> Result<NewMaterial, AppError> {
        let (value, public) = match purpose {
            MaterialPurpose::DashboardSession | MaterialPurpose::Initialization => {
                (random_token()?, None)
            }
            MaterialPurpose::ExecutionCredential => {
                let token = random_token()?;
                let digest = credential_digest(token.as_bytes());
                (token, Some(digest))
            }
            MaterialPurpose::SshClient | MaterialPurpose::WorkerHost => {
                let (private, public) = ed25519_pair()?;
                (private, Some(public))
            }
        };
        Ok(NewMaterial {
            sealed: self
                .sealer
                .seal_framed(&material_frame(instance, purpose), value.as_bytes())?,
            public,
        })
    }

    fn open_material(
        &self,
        instance: Uuid,
        purpose: MaterialPurpose,
        sealed: &[u8],
    ) -> Result<Plain, AppError> {
        let value = self
            .opener
            .open_framed(&material_frame(instance, purpose), sealed)?;
        Ok(Plain::new(value.to_vec()))
    }

    fn open_secret(&self, name: &str, kind: SecretKind, sealed: &[u8]) -> Result<Plain, AppError> {
        let value = self.opener.open(name, kind, sealed)?;
        Ok(Plain::new(value.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::app::secrets::SecretSealer;

    #[test]
    fn material_opens_only_for_its_instance_and_purpose() {
        let vault = ControllerVault::new([3; 32]);
        let instance = Uuid::new_v4();
        let made = vault
            .generate(instance, MaterialPurpose::Initialization)
            .unwrap();
        let key = vault
            .open_material(instance, MaterialPurpose::Initialization, &made.sealed)
            .unwrap();
        assert_eq!(key.expose().len(), 64);
        assert!(key.expose().iter().all(u8::is_ascii_hexdigit));
        assert!(
            vault
                .open_material(
                    Uuid::new_v4(),
                    MaterialPurpose::Initialization,
                    &made.sealed
                )
                .is_err()
        );
        assert!(
            vault
                .open_material(instance, MaterialPurpose::DashboardSession, &made.sealed)
                .is_err()
        );
        assert!(
            ControllerVault::new([4; 32])
                .open_material(instance, MaterialPurpose::Initialization, &made.sealed)
                .is_err()
        );
    }

    #[test]
    fn each_purpose_has_the_shape_its_consumer_expects() {
        let vault = ControllerVault::new([3; 32]);
        let instance = Uuid::new_v4();
        for purpose in MaterialPurpose::ALL {
            let made = vault.generate(instance, purpose).unwrap();
            let plain = vault
                .open_material(instance, purpose, &made.sealed)
                .unwrap();
            let text = std::str::from_utf8(plain.expose()).unwrap();
            match purpose {
                MaterialPurpose::DashboardSession | MaterialPurpose::Initialization => {
                    assert_eq!(text.len(), 64);
                    assert_eq!(made.public, None);
                }
                MaterialPurpose::ExecutionCredential => {
                    assert_eq!(
                        made.public.as_deref(),
                        Some(credential_digest(text.as_bytes()).as_str())
                    );
                }
                MaterialPurpose::SshClient | MaterialPurpose::WorkerHost => {
                    let key = PrivateKey::from_openssh(text).unwrap();
                    assert_eq!(
                        key.public_key().to_openssh().unwrap(),
                        made.public.clone().unwrap()
                    );
                    assert!(made.public.unwrap().starts_with("ssh-ed25519 "));
                }
            }
            assert_ne!(
                vault.generate(instance, purpose).unwrap().sealed,
                made.sealed,
                "fresh each time"
            );
        }
    }

    #[test]
    fn admin_secrets_sealed_by_the_api_open_in_the_vault() {
        let vault = ControllerVault::new([5; 32]);
        let api = PublicSealer::new(SecretOpener::new([5; 32]).public_key());
        let sealed = api.seal("llm-key", SecretKind::Env, b"sk-value").unwrap();
        assert_eq!(
            vault
                .open_secret("llm-key", SecretKind::Env, &sealed)
                .unwrap()
                .expose(),
            b"sk-value"
        );
        let material = vault
            .generate(Uuid::new_v4(), MaterialPurpose::DashboardSession)
            .unwrap();
        assert!(
            vault
                .open_secret("llm-key", SecretKind::Env, &material.sealed)
                .is_err(),
            "frames never mix"
        );
    }
}
