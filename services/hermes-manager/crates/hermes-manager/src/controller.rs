//! Controller-only configuration and process lifecycle. No implicit kubeconfig.
#[cfg(test)]
mod tests;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use hermes_adapters::{
    auth_config::read, controller::Controller, kubernetes::KubernetesDriver,
    materials::MaterialVault, ownership::OwnershipSigner, postgres::PgStore,
    session_crypto::Sealer, workloads::WorkloadConfig,
};
use hermes_application::{Error, InstallationScope};
use kube::config::{KubeConfigOptions, Kubeconfig};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configuration {
    installation: Installation,
    database_url_file: PathBuf,
    material_encryption_keyring_file: PathBuf,
    ownership_keyring_file: PathBuf,
    kubernetes: Kubernetes,
    cluster_identity: String,
    workload: WorkloadConfig,
    execution_url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Installation {
    id: Uuid,
    epoch: i64,
    issuer: String,
}

#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "camelCase", deny_unknown_fields)]
enum Kubernetes {
    InCluster {},
    Kubeconfig { file: PathBuf, context: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Keyring {
    active_key_id: String,
    key_files: BTreeMap<String, PathBuf>,
}

impl Keyring {
    async fn load(path: &Path) -> Result<(String, BTreeMap<String, Vec<u8>>), Error> {
        let keyring: Self =
            serde_json::from_slice(&read(path, 16 * 1024).await?).map_err(|_| Error::Invalid)?;
        if keyring.key_files.is_empty()
            || keyring.key_files.len() > 16
            || !keyring.key_files.contains_key(&keyring.active_key_id)
        {
            return Err(Error::Invalid);
        }
        let mut keys = BTreeMap::new();
        for (id, path) in keyring.key_files {
            hermes_domain::validate_name(&id)?;
            let bytes = read(&path, 32).await?;
            if bytes.len() != 32 {
                return Err(Error::Invalid);
            }
            keys.insert(id, bytes.to_vec());
        }
        Ok((keyring.active_key_id, keys))
    }
}

impl Kubernetes {
    async fn driver(
        self,
        signer: OwnershipSigner,
        cluster_identity: String,
    ) -> Result<KubernetesDriver, Error> {
        match self {
            Self::InCluster {} => KubernetesDriver::incluster(signer, cluster_identity),
            Self::Kubeconfig { file, context } => {
                if context.is_empty() || context.len() > 255 {
                    return Err(Error::Invalid);
                }
                let bytes = read(&file, 256 * 1024).await?;
                let config =
                    Kubeconfig::from_yaml(std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?)
                        .map_err(|_| Error::Invalid)?;
                // Reject executable/impersonating configuration before any
                // library config or client construction. No auth plugin is run.
                validate_kubeconfig(&config, &context)?;
                let config = kube::Config::from_custom_kubeconfig(
                    config,
                    &KubeConfigOptions {
                        context: Some(context),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|_| Error::Invalid)?;
                KubernetesDriver::from_config(config, signer, cluster_identity)
            }
        }
    }
}

fn validate_kubeconfig(config: &Kubeconfig, context: &str) -> Result<(), Error> {
    let contexts: Vec<_> = config
        .contexts
        .iter()
        .filter(|c| c.name == context)
        .collect();
    let [context] = contexts.as_slice() else {
        return Err(Error::Invalid);
    };
    let context = context.context.as_ref().ok_or(Error::Invalid)?;
    let clusters: Vec<_> = config
        .clusters
        .iter()
        .filter(|c| c.name == context.cluster)
        .collect();
    let [cluster] = clusters.as_slice() else {
        return Err(Error::Invalid);
    };
    let cluster = cluster.cluster.as_ref().ok_or(Error::Invalid)?;
    let user = context.user.as_ref().ok_or(Error::Invalid)?;
    let users: Vec<_> = config
        .auth_infos
        .iter()
        .filter(|u| &u.name == user)
        .collect();
    let [user] = users.as_slice() else {
        return Err(Error::Invalid);
    };
    let user = user.auth_info.as_ref().ok_or(Error::Invalid)?;
    if cluster.insecure_skip_tls_verify == Some(true)
        || cluster.proxy_url.is_some()
        || user.exec.is_some()
        || user.auth_provider.is_some()
        || user.impersonate.is_some()
        || user.impersonate_uid.is_some()
        || user.impersonate_groups.is_some()
        || user.impersonate_user_extra.is_some()
    {
        return Err(Error::Invalid);
    }
    // Parsing an explicit file must not make referenced credentials depend on
    // the process working directory. Mounted references use absolute paths.
    for path in [
        cluster.certificate_authority.as_deref(),
        user.token_file.as_deref(),
        user.client_certificate.as_deref(),
        user.client_key.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !Path::new(path).is_absolute() {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

/// Opens only an existing installation. Migration/fresh inventory initialization
/// are explicit operator actions; controller startup never creates a registry.
pub async fn run(path: &Path) -> Result<(), Error> {
    let config: Configuration =
        serde_json::from_slice(&read(path, 64 * 1024).await?).map_err(|_| Error::Invalid)?;
    let scope = InstallationScope {
        id: config.installation.id,
        epoch: config.installation.epoch,
        issuer: config.installation.issuer,
    };
    hermes_domain::validate_issuer(&scope.issuer)?;
    if scope.id.is_nil()
        || scope.epoch < 1
        || !hermes_domain::resources::valid_uid(&config.cluster_identity)
    {
        return Err(Error::Invalid);
    }
    config.workload.validate()?;
    let database_bytes = read(&config.database_url_file, 16 * 1024).await?;
    let database_url = std::str::from_utf8(&database_bytes).map_err(|_| Error::Invalid)?;
    let store = PgStore::connect(database_url.trim()).await?;
    store.check_registry(&scope).await?;
    let (material_id, material_keys) =
        Keyring::load(&config.material_encryption_keyring_file).await?;
    let (ownership_id, ownership_keys) = Keyring::load(&config.ownership_keyring_file).await?;
    // HMAC ownership proofs and material encryption use independent key bytes.
    if material_keys
        .values()
        .any(|key| ownership_keys.values().any(|other| key == other))
    {
        return Err(Error::Invalid);
    }
    let signer = OwnershipSigner::new(ownership_id, ownership_keys)?;
    let driver = config
        .kubernetes
        .driver(signer, config.cluster_identity)
        .await?;
    let vault = MaterialVault::new(store.clone(), Sealer::new(material_id, material_keys)?);
    let controller = Controller::new(
        store,
        driver,
        vault,
        scope,
        config.workload,
        config.execution_url,
    )?;
    // A signal cancels in-flight I/O. Any committed intent remains uncertain in
    // the journal and is observed by the next database-fenced claimant.
    tokio::select! {
        result = async {
            controller.verify_startup().await?;
            worker_loop(&controller).await
        } => result,
        result = shutdown_signal() => result,
    }
}

async fn worker_loop(controller: &Controller) -> Result<(), Error> {
    let mut previous_error = None;
    loop {
        match controller.step().await {
            Ok(true) => {
                previous_error = None;
                tokio::task::yield_now().await;
                continue;
            }
            Ok(false) => previous_error = None,
            Err(error @ (Error::Unavailable | Error::Maintenance | Error::StaleFence)) => {
                if previous_error.as_ref() != Some(&error) {
                    eprintln!("hermes-manager controller: {error}");
                }
                previous_error = Some(error);
            }
            Err(error) => return Err(error),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn shutdown_signal() -> Result<(), Error> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).map_err(|_| Error::Unavailable)?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| Error::Unavailable),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .map_err(|_| Error::Unavailable)
}
