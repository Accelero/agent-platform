//! Authenticate an immutable creation intent, including cluster and namespace UID.
use hermes_application::{Error, resources::Creation};
use hermes_domain::{resources::valid_uid, validate_name};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct OwnershipSigner {
    active: String,
    keys: Arc<BTreeMap<String, Zeroizing<Vec<u8>>>>,
}
impl OwnershipSigner {
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
                    .map(|(id, key)| (id, Zeroizing::new(key)))
                    .collect(),
            ),
        })
    }
    fn mac(&self, key_id: &str, creation: &Creation) -> Result<Hmac<Sha256>, Error> {
        creation.spec.validate()?;
        if creation.id.is_nil()
            || creation.installation_id.is_nil()
            || creation.instance_id.is_nil()
            || creation.operation_id.is_nil()
            || creation.epoch < 1
            || creation.generation < 1
            || !valid_uid(&creation.cluster_identity)
        {
            return Err(Error::Invalid);
        }
        let key = self.keys.get(key_id).ok_or(Error::OwnershipConflict)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| Error::Invalid)?;
        mac.update(b"agent-platform-resource-creation-v1\0");
        mac.update(key_id.as_bytes());
        mac.update(b"\0");
        mac.update(&serde_json::to_vec(creation).map_err(|_| Error::Invalid)?);
        Ok(mac)
    }
    pub fn sign(&self, creation: &Creation) -> Result<String, Error> {
        let tag = self.mac(&self.active, creation)?.finalize().into_bytes();
        Ok(format!(
            "v1.{}.{}",
            self.active,
            tag.iter().map(|v| format!("{v:02x}")).collect::<String>()
        ))
    }
    pub fn verify(&self, creation: &Creation, value: &str) -> Result<(), Error> {
        if value.len() > 132 {
            return Err(Error::OwnershipConflict);
        }
        let mut parts = value.split('.');
        if parts.next() != Some("v1") {
            return Err(Error::OwnershipConflict);
        }
        let key = parts.next().ok_or(Error::OwnershipConflict)?;
        let tag = parts.next().ok_or(Error::OwnershipConflict)?;
        if parts.next().is_some()
            || tag.len() != 64
            || !tag
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::OwnershipConflict);
        }
        let bytes: Result<Vec<u8>, _> = (0..64)
            .step_by(2)
            .map(|i| u8::from_str_radix(&tag[i..i + 2], 16))
            .collect();
        self.mac(key, creation)?
            .verify_slice(&bytes.map_err(|_| Error::OwnershipConflict)?)
            .map_err(|_| Error::OwnershipConflict)
    }
}
