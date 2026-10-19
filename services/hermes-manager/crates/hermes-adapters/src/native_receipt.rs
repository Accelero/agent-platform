//! Verify the init-container completion message without exposing its key.
use crate::native_bootstrap::NativeBootstrap;
use hermes_application::{Error, materials::MaterialContext};
use hermes_domain::Identity;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn context(
    material: &MaterialContext,
    bootstrap: &NativeBootstrap,
    owner: &Identity,
) -> Result<Value, Error> {
    material.validate()?;
    owner.validate(&owner.issuer)?;
    Ok(
        json!({"schemaVersion":1,"installationId":material.installation_id,"instanceId":material.instance_id,"materialId":material.id,"owner":owner,"template":bootstrap.template}),
    )
}
pub fn verify(
    material: &MaterialContext,
    bootstrap: &NativeBootstrap,
    owner: &Identity,
    receipt: &str,
) -> Result<(), Error> {
    if !crate::session_crypto::valid_handle(&bootstrap.initialization_key) {
        return Err(Error::Invalid);
    }
    if receipt.len() > 4096 {
        return Err(Error::OwnershipConflict);
    }
    let envelope: Value = serde_json::from_str(receipt).map_err(|_| Error::OwnershipConflict)?;
    let fields = envelope.as_object().ok_or(Error::OwnershipConflict)?;
    if fields.len() != 2 || !fields.contains_key("record") || !fields.contains_key("signature") {
        return Err(Error::OwnershipConflict);
    }
    let record = &envelope["record"];
    let signature = envelope["signature"]
        .as_str()
        .ok_or(Error::OwnershipConflict)?;
    if signature.len() != 64
        || !signature
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::OwnershipConflict);
    }
    let bytes: Result<Vec<u8>, _> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&signature[i..i + 2], 16))
        .collect();
    let key: Result<Vec<u8>, _> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&bootstrap.initialization_key[i..i + 2], 16))
        .collect();
    let key = zeroize::Zeroizing::new(key.map_err(|_| Error::Invalid)?);
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| Error::Invalid)?;
    mac.update(b"hermes-home-completion-v1\0");
    mac.update(&serde_json::to_vec(record).map_err(|_| Error::Invalid)?);
    mac.verify_slice(&bytes.map_err(|_| Error::OwnershipConflict)?)
        .map_err(|_| Error::OwnershipConflict)?;
    let owner_digest = Sha256::digest(serde_json::to_vec(owner).map_err(|_| Error::Invalid)?);
    let owner_digest: String = owner_digest.iter().map(|v| format!("{v:02x}")).collect();
    let binding = json!({"schemaVersion":1,"installationId":material.installation_id,"instanceId":material.instance_id,"materialId":material.id,"ownerDigest":owner_digest});
    if record["complete"] != true || record["intent"]["binding"] != binding {
        return Err(Error::OwnershipConflict);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
