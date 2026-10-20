//! Explicit fresh-install scan. Never used as a fallback after registry loss.
use super::*;
use crate::postgres::FreshInventory;
use hermes_application::InstallationScope;
use k8s_openapi::api::core::v1::{Namespace, PersistentVolume};
use kube::api::ListParams;

impl KubernetesDriver {
    pub async fn fresh_inventory(
        &self,
        scope: &InstallationScope,
    ) -> Result<FreshInventory, Error> {
        timeout(Duration::from_secs(20), async {
            self.verify_cluster(&self.cluster_identity).await?;
            let namespaces: Api<Namespace> = Api::all(self.client.clone());
            let volumes: Api<PersistentVolume> = Api::all(self.client.clone());
            let parameters = ListParams::default().limit(4096);
            let namespaces = namespaces.list(&parameters).await.map_err(api_error)?;
            let volumes = volumes.list(&parameters).await.map_err(api_error)?;
            // An incomplete inventory cannot prove freshness.
            if namespaces.metadata.continue_.is_some_and(|s| !s.is_empty())
                || volumes.metadata.continue_.is_some_and(|s| !s.is_empty())
            {
                return Err(Error::RecoveryRequired);
            }
            let prefix = format!("hi-{}-", &scope.id.simple().to_string()[..24]);
            let installation = scope.id.to_string();
            let owned = |meta: &kube::core::ObjectMeta| {
                meta.labels.as_ref().and_then(|l| l.get(INSTALLATION)) == Some(&installation)
            };
            let count = namespaces
                .items
                .iter()
                .filter(|n| {
                    owned(&n.metadata)
                        || n.metadata
                            .name
                            .as_ref()
                            .is_some_and(|s| s.starts_with(&prefix))
                })
                .count()
                + volumes
                    .items
                    .iter()
                    .filter(|v| {
                        owned(&v.metadata)
                            || v.spec
                                .as_ref()
                                .and_then(|s| s.claim_ref.as_ref())
                                .and_then(|c| c.namespace.as_ref())
                                .is_some_and(|s| s.starts_with(&prefix))
                    })
                    .count();
            if count != 0 {
                return Err(Error::RecoveryRequired);
            }
            let digest = Sha256::digest(
                format!(
                    "fresh-v1\0{}\0{}\0{}\0{}",
                    scope.id,
                    self.cluster_identity,
                    namespaces.metadata.resource_version.unwrap_or_default(),
                    volumes.metadata.resource_version.unwrap_or_default()
                )
                .as_bytes(),
            );
            Ok(FreshInventory {
                cluster_identity: self.cluster_identity.clone(),
                digest: digest.iter().map(|v| format!("{v:02x}")).collect(),
                registered_instances: 0,
                unexplained_resources: 0,
            })
        })
        .await
        .map_err(|_| Error::Unavailable)?
    }
}
