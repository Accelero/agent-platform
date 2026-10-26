//! Sealing of session records. Cookie values are random handles; the database
//! holds only their hashes and AEAD-sealed payloads.

use std::collections::BTreeMap;
use std::sync::Arc;

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hermes_core::domain::names::validate_name;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::AuthError;

/// Which session store a record belongs to. Part of the associated data, so a
/// gateway record can never be opened as an administrator record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Administration,
    Gateway,
}

impl Purpose {
    pub fn name(self) -> &'static str {
        match self {
            Self::Administration => "administration",
            Self::Gateway => "gateway",
        }
    }
}

/// A sealed payload: key ID, 24-byte nonce and ciphertext with tag.
#[derive(Clone)]
pub struct Sealed {
    pub key_id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

/// XChaCha20-Poly1305 keyring. New records use the active key; older keys stay
/// readable until their sessions expire.
#[derive(Clone)]
pub struct Sealer {
    installation: Uuid,
    active: String,
    keys: Arc<BTreeMap<String, Zeroizing<Vec<u8>>>>,
}

/// 32 random bytes as lowercase hex.
pub fn random_handle() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Unavailable)?;
    Ok(bytes.iter().map(|v| format!("{v:02x}")).collect())
}

pub fn valid_handle(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

impl Sealer {
    pub fn new(
        installation: Uuid,
        active: String,
        keys: BTreeMap<String, Vec<u8>>,
    ) -> Result<Self, AuthError> {
        validate_name(&active).map_err(|_| AuthError::Configuration)?;
        if installation.is_nil()
            || keys.is_empty()
            || keys.len() > 16
            || !keys.contains_key(&active)
        {
            return Err(AuthError::Configuration);
        }
        for (id, key) in &keys {
            validate_name(id).map_err(|_| AuthError::Configuration)?;
            if key.len() != 32 {
                return Err(AuthError::Configuration);
            }
        }
        let keys = keys
            .into_iter()
            .map(|(id, key)| (id, Zeroizing::new(key)))
            .collect();
        Ok(Self {
            installation,
            active,
            keys: Arc::new(keys),
        })
    }

    fn aad(
        &self,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        key_id: &str,
    ) -> Result<Vec<u8>, AuthError> {
        if id_hash.len() != 32 || !matches!(kind, "flow" | "session") {
            return Err(AuthError::Configuration);
        }
        let mut aad = format!(
            "hermes-session-v1\0{}\0{}\0{kind}\0{key_id}\0",
            self.installation,
            purpose.name()
        )
        .into_bytes();
        aad.extend_from_slice(id_hash);
        Ok(aad)
    }

    pub fn seal(
        &self,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, AuthError> {
        if plaintext.is_empty() || plaintext.len() > 65520 {
            return Err(AuthError::Configuration);
        }
        let aad = self.aad(purpose, kind, id_hash, &self.active)?;
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|_| AuthError::Unavailable)?;
        let key = self.keys.get(&self.active).ok_or(AuthError::Unavailable)?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(key).map_err(|_| AuthError::Configuration)?;
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| AuthError::Unavailable)?;
        Ok(Sealed {
            key_id: self.active.clone(),
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    pub fn open(
        &self,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        sealed: &Sealed,
    ) -> Result<Zeroizing<Vec<u8>>, AuthError> {
        let nonce: [u8; 24] = sealed
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| AuthError::Evidence)?;
        if sealed.ciphertext.len() > 65536 {
            return Err(AuthError::Evidence);
        }
        let aad = self.aad(purpose, kind, id_hash, &sealed.key_id)?;
        let key = self.keys.get(&sealed.key_id).ok_or(AuthError::Evidence)?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(key).map_err(|_| AuthError::Configuration)?;
        cipher
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &sealed.ciphertext,
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| AuthError::Evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer(installation: Uuid, active: &str) -> Sealer {
        let keys = BTreeMap::from([
            ("key-a".to_owned(), vec![1; 32]),
            ("key-b".to_owned(), vec![2; 32]),
        ]);
        Sealer::new(installation, active.into(), keys).unwrap()
    }

    #[test]
    fn sealed_records_bind_installation_purpose_kind_and_record() {
        let installation = Uuid::new_v4();
        let sealer = sealer(installation, "key-a");
        let record = hash("record");
        let sealed = sealer
            .seal(Purpose::Administration, "session", &record, b"secret")
            .unwrap();
        assert_eq!(
            &*sealer
                .open(Purpose::Administration, "session", &record, &sealed)
                .unwrap(),
            b"secret"
        );
        let wrong = [
            sealer.open(Purpose::Gateway, "session", &record, &sealed),
            sealer.open(Purpose::Administration, "flow", &record, &sealed),
            sealer.open(Purpose::Administration, "session", &hash("other"), &sealed),
            self::sealer(Uuid::new_v4(), "key-a").open(
                Purpose::Administration,
                "session",
                &record,
                &sealed,
            ),
        ];
        for result in wrong {
            assert_eq!(result.err(), Some(AuthError::Evidence));
        }
    }

    #[test]
    fn rotation_keeps_old_records_readable() {
        let installation = Uuid::new_v4();
        let record = hash("record");
        let old = sealer(installation, "key-a")
            .seal(Purpose::Gateway, "flow", &record, b"secret")
            .unwrap();
        let rotated = sealer(installation, "key-b");
        assert_eq!(
            &*rotated
                .open(Purpose::Gateway, "flow", &record, &old)
                .unwrap(),
            b"secret"
        );
        assert_eq!(
            rotated
                .seal(Purpose::Gateway, "flow", &record, b"x")
                .unwrap()
                .key_id,
            "key-b"
        );
    }

    #[test]
    fn keyrings_are_bounded_and_complete() {
        let id = Uuid::new_v4();
        assert!(
            Sealer::new(
                id,
                "missing".into(),
                BTreeMap::from([("key-a".into(), vec![0; 32])])
            )
            .is_err()
        );
        assert!(
            Sealer::new(
                id,
                "key-a".into(),
                BTreeMap::from([("key-a".into(), vec![0; 31])])
            )
            .is_err()
        );
        assert!(
            Sealer::new(
                Uuid::nil(),
                "key-a".into(),
                BTreeMap::from([("key-a".into(), vec![0; 32])])
            )
            .is_err()
        );
    }
}
