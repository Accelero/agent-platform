//! Durable commissioning worker. No process-local queue or user command replay.
use crate::{
    bootstrap::BootstrapPolicy,
    kubernetes::{KubernetesDriver, Manifest, NativeReadiness},
    materials::MaterialVault,
    native_probe::NativeBackendProbe,
    postgres::PgStore,
    workloads::{self, RenderedResource, WorkloadConfig},
};
use hermes_application::{
    Error, InstallationScope, OperationClaim, OperationQueue,
    commissioning::{CommissioningRepository, ConditionReason, Schedule},
    resources::{ResourceReconciler, ResourceRepository},
};
use hermes_domain::resources::{ResourceKind, ResourceSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenRendering {
    spec: ResourceSpec,
    manifest: Value,
}
pub struct Controller {
    store: PgStore,
    driver: KubernetesDriver,
    vault: MaterialVault<PgStore>,
    scope: InstallationScope,
    config: WorkloadConfig,
    execution_url: String,
    bootstrap: BootstrapPolicy,
    probe: NativeBackendProbe,
    worker: Uuid,
}
impl Controller {
    pub fn new(
        store: PgStore,
        driver: KubernetesDriver,
        vault: MaterialVault<PgStore>,
        scope: InstallationScope,
        config: WorkloadConfig,
        execution_url: String,
    ) -> Result<Self, Error> {
        config.validate()?;
        let url = url::Url::parse(&execution_url).map_err(|_| Error::Invalid)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.path() != "/execution"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        let bootstrap = BootstrapPolicy::new(&scope, &config)?;
        Ok(Self {
            store,
            driver,
            vault,
            scope,
            config,
            execution_url,
            bootstrap,
            probe: NativeBackendProbe::new()?,
            worker: Uuid::new_v4(),
        })
    }
    pub async fn verify_startup(&self) -> Result<(), Error> {
        self.store.check_registry(&self.scope).await?;
        self.driver
            .verify_cluster(&self.store.cluster_identity(&self.scope).await?)
            .await?;
        self.driver.verify_bootstrap(&self.bootstrap).await
    }
    /// One bounded claim at a time. Concurrent controller processes use the
    /// database lease/fence, not a leader flag or an in-memory assignment cache.
    pub async fn step(&self) -> Result<bool, Error> {
        let Some(claim) = self.store.claim(&self.scope, self.worker, 30).await? else {
            return Ok(false);
        };
        let outcome = self.reconcile(&claim).await;
        let schedule = match outcome {
            Ok(reason) => Schedule::Waiting { reason, seconds: 5 },
            Err(error @ (Error::StaleFence | Error::Maintenance | Error::RecoveryRequired)) => {
                return Err(error);
            }
            Err(Error::OwnershipConflict) => Schedule::Blocked {
                reason: ConditionReason::OwnershipConflict,
            },
            Err(Error::NativeAuthInvalid) => Schedule::Blocked {
                reason: ConditionReason::NativeAuthInvalid,
            },
            Err(Error::TemplateUnavailable) => Schedule::Blocked {
                reason: ConditionReason::TemplateUnavailable,
            },
            Err(Error::Invalid) => Schedule::Blocked {
                reason: ConditionReason::AdmissionUnavailable,
            },
            Err(_) => Schedule::Retry {
                reason: ConditionReason::DependencyUnavailable,
                seconds: 5,
            },
        };
        // Resource creation is not readiness. Native receipt/backend/storage and
        // effective execution-policy verification must precede route publication.
        self.store.schedule(&self.scope, &claim, schedule).await?;
        Ok(true)
    }
    async fn reconcile(&self, claim: &OperationClaim) -> Result<ConditionReason, Error> {
        self.driver.verify_bootstrap(&self.bootstrap).await?;
        self.store.renew(&self.scope, claim, 30).await?;
        let plan = self.store.commission_plan(&self.scope, claim).await?;
        let (material, bootstrap) = self
            .vault
            .native_bootstrap(
                &self.scope,
                claim,
                plan.assignment.template.clone(),
                &self.execution_url,
            )
            .await?;
        let resources =
            ResourceReconciler::new(self.store.clone(), self.driver.clone(), self.scope.clone());
        let namespace_manifest = self
            .freeze(
                claim,
                "namespace",
                vec![workloads::namespace(&self.scope, &plan.assignment)?],
            )
            .await?
            .pop()
            .ok_or(Error::RecoveryRequired)?;
        let namespace = resources
            .ensure(
                claim,
                &namespace_manifest.key,
                namespace_manifest.spec.clone(),
                &namespace_manifest.manifest,
            )
            .await?;
        if plan
            .namespace_uid
            .as_ref()
            .is_some_and(|uid| uid != &namespace.uid)
        {
            return Err(Error::OwnershipConflict);
        }
        let desired = workloads::render(
            &self.scope,
            &plan.assignment,
            &namespace.uid,
            &plan.template,
            &self.config,
            &material.context,
            &bootstrap,
        )?;
        // Freeze the complete set atomically before the first child call. A
        // crash cannot combine an old admission policy with a newly rendered
        // Deployment, or a new Secret with an old workload expectation.
        let desired = self.freeze(claim, "instance", desired).await?;
        for desired in &desired {
            resources
                .ensure(claim, &desired.key, desired.spec.clone(), &desired.manifest)
                .await?;
        }
        let namespace = self
            .store
            .find_resource(&self.scope, claim, &namespace_manifest.spec)
            .await?
            .ok_or(Error::RecoveryRequired)?;
        let mut observed = Vec::new();
        for (kind, name) in [
            ResourceKind::PersistentVolumeClaim,
            ResourceKind::PersistentVolumeClaim,
            ResourceKind::Deployment,
        ]
        .into_iter()
        .zip(["home", "workspace", "hermes"])
        {
            let rendered = desired
                .iter()
                .find(|d| d.spec.kind == kind && d.spec.name == name)
                .ok_or(Error::RecoveryRequired)?;
            let bound = self
                .store
                .find_resource(&self.scope, claim, &rendered.spec)
                .await?
                .ok_or(Error::RecoveryRequired)?;
            observed.push((bound, &rendered.manifest));
        }
        self.store.renew(&self.scope, claim, 30).await?;
        let reason = self
            .driver
            .native_readiness(NativeReadiness {
                namespace: &namespace,
                storage: [
                    (&observed[0].0, observed[0].1),
                    (&observed[1].0, observed[1].1),
                ],
                deployment: (&observed[2].0, observed[2].1),
                epoch: self.scope.epoch,
                generation: claim.generation,
                material: &material.context,
                bootstrap: &bootstrap,
                owner: &plan.assignment.identity,
            })
            .await?;
        if reason != ConditionReason::ExecutionPolicyPending {
            return Ok(reason);
        }
        let service = desired
            .iter()
            .find(|d| d.spec.kind == ResourceKind::Service && d.spec.name == "hermes")
            .ok_or(Error::RecoveryRequired)?;
        let bound = self
            .store
            .find_resource(&self.scope, claim, &service.spec)
            .await?
            .ok_or(Error::RecoveryRequired)?;
        self.store.renew(&self.scope, claim, 30).await?;
        self.driver
            .verify_bound_manifest(&bound, &service.manifest)
            .await?;
        self.store.renew(&self.scope, claim, 30).await?;
        self.probe.verify(&plan.assignment.namespace).await?;
        Ok(ConditionReason::ExecutionPolicyPending)
    }
    async fn freeze(
        &self,
        claim: &OperationClaim,
        label: &str,
        desired: Vec<RenderedResource>,
    ) -> Result<Vec<RenderedResource>, Error> {
        // Persist the complete first rendering, including encrypted Secret
        // bytes, before any Kubernetes action. A restart or software/config
        // change cannot silently alter an outstanding creation's input.
        let purpose = format!("render-{label}-g{}", claim.generation);
        let material = self
            .vault
            .get_or_create(&self.scope, claim, &purpose, || {
                let frozen: Vec<_> = desired
                    .iter()
                    .map(|d| FrozenRendering {
                        spec: d.spec.clone(),
                        manifest: d.manifest.value().clone(),
                    })
                    .collect();
                Ok(Zeroizing::new(
                    serde_json::to_vec(&frozen).map_err(|_| Error::Invalid)?,
                ))
            })
            .await?;
        let frozen: Vec<FrozenRendering> =
            serde_json::from_slice(&material.value).map_err(|_| Error::RecoveryRequired)?;
        if frozen.len() != desired.len() || material.context.generation != claim.generation {
            return Err(Error::OwnershipConflict);
        }
        frozen
            .into_iter()
            .zip(desired)
            .map(|(frozen, desired)| {
                let mut same_target = frozen.spec.clone();
                same_target.manifest_digest = desired.spec.manifest_digest.clone();
                if same_target != desired.spec {
                    return Err(Error::OwnershipConflict);
                }
                let manifest = Manifest::new(frozen.manifest)?;
                manifest.validate(&frozen.spec)?;
                Ok(RenderedResource {
                    key: desired.key,
                    spec: frozen.spec,
                    manifest,
                })
            })
            .collect()
    }
}
