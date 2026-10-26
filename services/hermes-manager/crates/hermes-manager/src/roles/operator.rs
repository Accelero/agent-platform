//! Operator commands: explicit, audited, never automatic.

use std::path::Path;

use hermes_core::app::access::BootstrapAdministrator;
use hermes_core::app::actor::Actor;
use hermes_core::app::envelope::{CommandEnvelope, IdempotencyKey};
use hermes_core::app::error::AppError;
use hermes_core::domain::ErrorKind;
use hermes_core::domain::identity::Identity;
use uuid::Uuid;

use crate::config::{Config, read};
use crate::faults;

/// Applies pending migrations. Never initializes an installation.
pub async fn migrate(config: &Config) -> Result<(), AppError> {
    Ok(config.store().await?.migrate().await?)
}

/// Verifies schema, logged tables and installation identity.
pub async fn check(config: &Config) -> Result<(), AppError> {
    config.store().await?.check(&config.installation).await
}

/// Grants the first administrator, or with `recover` restores a managing
/// grant. The identity comes from a file and is never printed.
pub async fn grant_administrator(
    config: &Config,
    identity_file: &Path,
    recover: bool,
) -> Result<(), AppError> {
    let identity: Identity = serde_json::from_slice(&read(identity_file, 4096).await?)
        .map_err(|_| AppError::invalid())?;
    let store = config.store().await?;
    store.check(&config.installation).await?;
    let envelope = CommandEnvelope::new(store, config.settings()?, faults::hook());
    // A fresh key per invocation: each run is a separate operator decision.
    let key = IdempotencyKey::parse(&Uuid::new_v4().to_string())?;
    let command = BootstrapAdministrator { identity, recover };
    envelope
        .execute(&Actor::Operator, Uuid::new_v4(), &key, &command)
        .await?;
    println!("Administrator grant updated.");
    Ok(())
}

/// Records the installation and this cluster's identity in an empty, migrated
/// registry. Runs with the controller's Kubernetes credentials.
pub async fn initialize(config: &Config) -> Result<(), AppError> {
    let section = config.controller.as_ref().ok_or_else(AppError::invalid)?;
    let client = section.client().await?;
    let cluster = hermes_kube::cluster_identity(&client)
        .await
        .map_err(|_| AppError::unavailable())?;
    config
        .store()
        .await?
        .initialize(&config.installation, &cluster)
        .await?;
    println!("Installation initialized.");
    Ok(())
}

/// After a restore onto a new cluster: verifies that the cluster holds no
/// namespace of this installation, then records its identity and clears the
/// namespace bindings so the controller recreates them.
pub async fn rebind_cluster(config: &Config) -> Result<(), AppError> {
    let section = config.controller.as_ref().ok_or_else(AppError::invalid)?;
    let store = config.store().await?;
    store.check(&config.installation).await?;
    let client = section.client().await?;
    let cluster = hermes_kube::cluster_identity(&client)
        .await
        .map_err(|_| AppError::unavailable())?;
    if cluster == store.cluster_uid().await? {
        println!("Cluster identity unchanged; nothing to rebind.");
        return Ok(());
    }
    let existing = hermes_kube::owned_namespaces(&client, config.installation.id)
        .await
        .map_err(|_| AppError::unavailable())?;
    if !existing.is_empty() {
        return Err(AppError::new(ErrorKind::Conflict, "namespaces_exist"));
    }
    let cleared = store.rebind_cluster(&cluster).await?;
    println!("Cluster identity rebound; {cleared} namespace bindings cleared.");
    Ok(())
}

/// Writes a new X25519 key pair: the private key for the controller, the
/// public key for the API. Never overwrites a file.
pub async fn generate_sealing_key(
    private_key_file: &Path,
    public_key_file: &Path,
) -> Result<(), AppError> {
    let mut key = zeroize::Zeroizing::new([0u8; 32]);
    hermes_native::materials::fill_random(&mut *key)?;
    let public = hermes_native::sealing::SecretOpener::new(*key).public_key();
    write_new(private_key_file, &key[..], 0o600)?;
    write_new(public_key_file, &public, 0o644)?;
    println!("Sealing key pair written.");
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), AppError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|_| AppError::invalid())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| AppError::unavailable())
}
