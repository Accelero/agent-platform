//! Encrypted, context-bound provider material. No plaintext secrets in records.
use std::{collections::BTreeMap, sync::Arc};

use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
pub use hermes_application::materials::Ciphertext as Sealed;
use hermes_application::materials::MaterialContext;
use hermes_application::{Error, InstallationScope};
use hermes_domain::validate_name;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Administration,
    Router,
}
impl Purpose {
    pub fn name(self) -> &'static str {
        match self {
            Self::Administration => "administration",
            Self::Router => "router",
        }
    }
    pub fn schema(self) -> &'static str {
        match self {
            Self::Administration => "manager_admin_auth",
            Self::Router => "manager_router_auth",
        }
    }
}

#[derive(Clone)]
pub struct Sealer {
    active: String,
    keys: Arc<BTreeMap<String, Zeroizing<Vec<u8>>>>,
}

pub fn random_handle() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
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
    pub fn new(active: String, keys: BTreeMap<String, Vec<u8>>) -> Result<Self, Error> {
        validate_name(&active)?;
        if keys.is_empty() || keys.len() > 16 || !keys.contains_key(&active) {
            return Err(Error::Invalid);
        }
        for (id, key) in &keys {
            validate_name(id)?;
            if key.len() != 32 {
                return Err(Error::Invalid);
            }
        }
        Ok(Self {
            active,
            keys: Arc::new(
                keys.into_iter()
                    .map(|(k, v)| (k, Zeroizing::new(v)))
                    .collect(),
            ),
        })
    }

    fn aad(
        scope: &InstallationScope,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        key_id: &str,
    ) -> Result<Vec<u8>, Error> {
        if id_hash.len() != 32 || !matches!(kind, "flow" | "session") {
            return Err(Error::Invalid);
        }
        let mut aad = format!(
            "hermes-auth-v1\0{}\0{}\0{}\0{}\0{}\0",
            scope.id,
            scope.epoch,
            purpose.name(),
            kind,
            key_id
        )
        .into_bytes();
        aad.extend_from_slice(id_hash);
        Ok(aad)
    }

    pub fn seal(
        &self,
        scope: &InstallationScope,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, Error> {
        self.encrypt(
            &Self::aad(scope, purpose, kind, id_hash, &self.active)?,
            plaintext,
        )
    }

    fn encrypt(&self, aad: &[u8], plaintext: &[u8]) -> Result<Sealed, Error> {
        if plaintext.is_empty() || plaintext.len() > 65520 {
            return Err(Error::Invalid);
        }
        let mut nonce = vec![0u8; 24];
        getrandom::fill(&mut nonce).map_err(|_| Error::Unavailable)?;
        let key = self.keys.get(&self.active).ok_or(Error::Unavailable)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::Invalid)?;
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(<[u8; 24]>::try_from(nonce.as_slice()).map_err(|_| Error::Invalid)?),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| Error::Unavailable)?;
        Ok(Sealed {
            key_id: self.active.clone(),
            nonce,
            ciphertext,
        })
    }

    pub fn open(
        &self,
        scope: &InstallationScope,
        purpose: Purpose,
        kind: &str,
        id_hash: &[u8],
        sealed: &Sealed,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.decrypt(
            &Self::aad(scope, purpose, kind, id_hash, &sealed.key_id)?,
            sealed,
        )
    }

    fn decrypt(&self, aad: &[u8], sealed: &Sealed) -> Result<Zeroizing<Vec<u8>>, Error> {
        if sealed.nonce.len() != 24 || sealed.ciphertext.len() > 65536 {
            return Err(Error::InvalidIdentityEvidence);
        }
        let key = self
            .keys
            .get(&sealed.key_id)
            .ok_or(Error::InvalidIdentityEvidence)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key).map_err(|_| Error::Invalid)?;
        cipher
            .decrypt(
                &XNonce::from(
                    <[u8; 24]>::try_from(sealed.nonce.as_slice()).map_err(|_| Error::Invalid)?,
                ),
                Payload {
                    msg: &sealed.ciphertext,
                    aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| Error::InvalidIdentityEvidence)
    }

    fn material_aad(context: &MaterialContext, key_id: &str) -> Result<Vec<u8>, Error> {
        context.validate()?;
        let mut aad = format!("hermes-bootstrap-material-v1\0{key_id}\0").into_bytes();
        aad.extend_from_slice(&serde_json::to_vec(context).map_err(|_| Error::Invalid)?);
        Ok(aad)
    }
    pub fn seal_material(
        &self,
        context: &MaterialContext,
        plaintext: &[u8],
    ) -> Result<Sealed, Error> {
        self.encrypt(&Self::material_aad(context, &self.active)?, plaintext)
    }
    pub fn open_material(
        &self,
        context: &MaterialContext,
        sealed: &Sealed,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.decrypt(&Self::material_aad(context, &sealed.key_id)?, sealed)
    }
}
