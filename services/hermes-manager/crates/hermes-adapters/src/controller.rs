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
    lifecycle::{LifecyclePlan, LifecycleRepository, RetainedResource},
    namespace_fence::{Admission, NamespaceFenceRepository, NamespaceFenceState},
    native_children::{NativeChildAction, NativeChildRepository},
    native_stop::NativeStopRepository,
    resources::{OwnedResource, ResourceReconciler, ResourceRepository},
    resync::PeriodicResync,
    terminal::clients::{ExecutionClientRepository, ExecutionTokenHash, NATIVE_EXECUTION_PROFILE},
};
use hermes_domain::{
    Access,
    resources::{ResourceKind, ResourceSpec},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
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
        self.store.enqueue_due(&self.scope, 64).await?;
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
        let lifecycle = self.store.lifecycle_plan(&self.scope, claim).await?;
        if lifecycle.assignment.access != Access::Enabled
            || lifecycle
                .drain
                .as_ref()
                .is_some_and(|d| d.completed_at_millis.is_none())
        {
            return self.reconcile_stopping(claim, lifecycle).await;
        }
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
        // Commit the scoped hashed credential before any Secret containing the
        // corresponding raw bearer can be dispatched to the native application.
        self.store
            .register_under_current_claim(
                &self.scope,
                claim,
                &material.context,
                ExecutionTokenHash::from_token(&bootstrap.execution_client_token)?,
                NATIVE_EXECUTION_PROFILE,
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
        let bound_namespace = self
            .store
            .find_resource(&self.scope, claim, &namespace_manifest.spec)
            .await?
            .ok_or(Error::RecoveryRequired)?;
        self.store.renew(&self.scope, claim, 30).await?;
        let (_, state) = self
            .driver
            .current_namespace_fence(&bound_namespace)
            .await?;
        let already_open = state
            == Some(NamespaceFenceState {
                access: Access::Enabled,
                admission: Admission::Open,
                generation: claim.generation,
                epoch: claim.epoch,
            });
        if !already_open {
            self.fence_namespace(claim, &bound_namespace, Admission::Closed)
                .await?;
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
        let open_key = self
            .fence_namespace(claim, &namespace, Admission::Open)
            .await?;
        let deployment_rendered = desired
            .iter()
            .find(|d| d.spec.kind == ResourceKind::Deployment && d.spec.name == "hermes")
            .ok_or(Error::RecoveryRequired)?;
        let deployment = self
            .store
            .find_resource(&self.scope, claim, &deployment_rendered.spec)
            .await?
            .ok_or(Error::RecoveryRequired)?;
        self.store.renew(&self.scope, claim, 30).await?;
        let mut started = Vec::new();
        let mut pending_versions = Vec::new();
        for prepared in self
            .store
            .load_pending_native_children(&self.scope, claim, NativeChildAction::Start)
            .await?
        {
            self.store.renew(&self.scope, claim, 30).await?;
            match self.driver.apply_native_child(&prepared, claim).await {
                Ok(Some(result)) => {
                    let absent = result.absent;
                    self.store
                        .complete_native_child(&self.scope, claim, &prepared.input, result)
                        .await?;
                    if !absent {
                        started.push(prepared.input);
                    }
                }
                Ok(None) => {}
                Err(Error::PreconditionFailed) => pending_versions.push(prepared),
                Err(error) => return Err(error),
            }
        }
        let children = self
            .driver
            .discover_native_children(&deployment, Some(&deployment_rendered.manifest), claim)
            .await?;
        for child in children {
            self.store.renew(&self.scope, claim, 30).await?;
            let prepared = self
                .store
                .prepare_native_child(
                    &self.scope,
                    claim,
                    NativeChildAction::Start,
                    &open_key,
                    child,
                )
                .await?;
            if let Some(result) = self.driver.apply_native_child(&prepared, claim).await? {
                let absent = result.absent;
                // A previously completed start remains historical success if
                // its Pod vanished after this iteration's discovery. Do not
                // rewrite that first outcome or treat absence as readiness.
                if !absent || !prepared.completed {
                    self.store
                        .complete_native_child(&self.scope, claim, &prepared.input, result)
                        .await?;
                }
                if !absent {
                    started.push(prepared.input);
                }
            }
        }
        for prepared in pending_versions {
            self.store.renew(&self.scope, claim, 30).await?;
            if let Some(result) = self.driver.apply_native_child(&prepared, claim).await? {
                let absent = result.absent;
                self.store
                    .complete_native_child(&self.scope, claim, &prepared.input, result)
                    .await?;
                if !absent {
                    started.push(prepared.input);
                }
            }
        }
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
                started_children: &started,
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
    async fn fence_namespace(
        &self,
        claim: &OperationClaim,
        namespace: &OwnedResource,
        admission: Admission,
    ) -> Result<String, Error> {
        let fresh = |target: Admission| async move {
            self.store.renew(&self.scope, claim, 30).await?;
            let (version, _) = self.driver.current_namespace_fence(namespace).await?;
            let digest = Sha256::digest(version.resource_version.as_bytes());
            let suffix: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            let key = format!("fence-{}-{suffix}", target.as_str());
            let prepared = self
                .store
                .prepare_namespace_fence(&self.scope, claim, &key, target, version)
                .await?;
            self.store.renew(&self.scope, claim, 30).await?;
            let observed = self.driver.apply_namespace_fence(&prepared, claim).await?;
            self.store
                .complete_namespace_fence(&self.scope, claim, &key, observed)
                .await?;
            Ok::<String, Error>(key)
        };
        // Legacy retries could have left an uncertain CLOSE before a later
        // OPEN. Recover CLOSE first, even if that briefly closes admission on
        // this enabled generation. A stopping helper never dispatches OPEN.
        let targets: &[Admission] = if admission == Admission::Open {
            &[Admission::Closed, Admission::Open]
        } else {
            &[Admission::Closed]
        };
        for &target in targets {
            self.store.renew(&self.scope, claim, 30).await?;
            let pending = self
                .store
                .load_pending_namespace_fences(&self.scope, claim, target)
                .await?;
            let mut stale_versions = Vec::new();
            for prepared in pending {
                self.store.renew(&self.scope, claim, 30).await?;
                match self.driver.apply_namespace_fence(&prepared, claim).await {
                    Ok(observed) => {
                        self.store
                            .complete_namespace_fence(&self.scope, claim, &prepared.key, observed)
                            .await?
                    }
                    Err(Error::PreconditionFailed) => stale_versions.push(prepared),
                    Err(error) => return Err(error),
                }
            }
            if !stale_versions.is_empty() {
                // A new CAS intent establishes the desired state; only then
                // observe each old input's exact state without replaying its RV.
                fresh(target).await?;
                for prepared in stale_versions {
                    self.store.renew(&self.scope, claim, 30).await?;
                    let observed = self.driver.apply_namespace_fence(&prepared, claim).await?;
                    self.store
                        .complete_namespace_fence(&self.scope, claim, &prepared.key, observed)
                        .await?;
                }
            }
        }
        // Return a current verified state reference for child/stop preparation.
        fresh(admission).await
    }

    async fn retained_resource(
        &self,
        claim: &OperationClaim,
        retained: RetainedResource,
    ) -> Result<Option<OwnedResource>, Error> {
        if retained.owned.uid.is_some() {
            return Ok(Some(retained.owned));
        }
        let original = retained.original_rendering.ok_or(Error::RecoveryRequired)?;
        let material = if matches!(claim.kind.as_str(), "suspend" | "revoke")
            || (claim.kind == "resync"
                && self
                    .store
                    .lifecycle_plan(&self.scope, claim)
                    .await?
                    .assignment
                    .access
                    != Access::Enabled)
        {
            self.vault
                .read_retained(&self.scope, claim, &original.purpose)
                .await?
                .ok_or(Error::RecoveryRequired)?
        } else {
            self.vault
                .get_or_create(&self.scope, claim, &original.purpose, || {
                    Err(Error::RecoveryRequired)
                })
                .await?
        };
        if material.context.id != original.id || material.context.generation != original.generation
        {
            return Err(Error::RecoveryRequired);
        }
        let frozen: Vec<FrozenRendering> =
            serde_json::from_slice(&material.value).map_err(|_| Error::RecoveryRequired)?;
        let frozen = frozen
            .into_iter()
            .find(|r| r.spec == retained.owned.creation.spec)
            .ok_or(Error::RecoveryRequired)?;
        let manifest = Manifest::new(frozen.manifest)?;
        manifest.validate(&frozen.spec)?;
        let key = format!(
            "observe-{}-f{}",
            retained.owned.creation.id.simple(),
            claim.fence
        );
        let prepared = self
            .store
            .prepare_observation(&self.scope, claim, &key, retained.owned.creation.id)
            .await?;
        self.store.renew(&self.scope, claim, 30).await?;
        match self.driver.observe_reserved(&prepared, &manifest).await? {
            Some(observed) => {
                self.store
                    .bind_observation(&self.scope, claim, &key, observed)
                    .await?
            }
            None => {
                self.store
                    .observation_missing(&self.scope, claim, &key)
                    .await?;
                return Ok(None);
            }
        }
        self.store
            .find_resource(&self.scope, claim, &frozen.spec)
            .await
    }
    async fn reconcile_stopping(
        &self,
        claim: &OperationClaim,
        plan: LifecyclePlan,
    ) -> Result<ConditionReason, Error> {
        let Some(namespace) = plan.namespace else {
            return Ok(ConditionReason::Draining);
        };
        let Some(namespace) = self.retained_resource(claim, namespace).await? else {
            return Ok(ConditionReason::Draining);
        };
        let closed_key = self
            .fence_namespace(claim, &namespace, Admission::Closed)
            .await?;
        let Some(deployment) = plan.deployment else {
            return Ok(ConditionReason::Draining);
        };
        let Some(deployment) = self.retained_resource(claim, deployment).await? else {
            return Ok(ConditionReason::Draining);
        };
        let pending = self
            .store
            .load_pending_native_stops(&self.scope, claim)
            .await?;
        let had_pending = !pending.is_empty();
        let mut stale_versions = Vec::new();
        for prepared in pending {
            self.store.renew(&self.scope, claim, 30).await?;
            match self.driver.stop_native(&prepared, claim).await {
                Ok(observed) => {
                    self.store
                        .complete_native_stop(&self.scope, claim, &prepared.key, observed)
                        .await?
                }
                // The exact parent/namespace/digest was checked; only the old
                // CAS version prevents a write. Keep its outcome unknown until
                // a freshly committed CAS stop is observed below.
                Err(Error::PreconditionFailed) => stale_versions.push(prepared),
                Err(error) => return Err(error),
            }
        }
        if !had_pending || !stale_versions.is_empty() {
            self.store.renew(&self.scope, claim, 30).await?;
            let inspection = self.driver.inspect_native_stop(&deployment).await?;
            let suffix: String = Sha256::digest(inspection.resource_version.as_bytes())[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let key = format!("stop-native-{suffix}");
            let prepared = self
                .store
                .prepare_native_stop(&self.scope, claim, &key, &closed_key, inspection)
                .await?;
            self.store.renew(&self.scope, claim, 30).await?;
            let observed = self.driver.stop_native(&prepared, claim).await?;
            self.store
                .complete_native_stop(&self.scope, claim, &key, observed)
                .await?;
            for prepared in stale_versions {
                self.store.renew(&self.scope, claim, 30).await?;
                let observed = self.driver.stop_native(&prepared, claim).await?;
                self.store
                    .complete_native_stop(&self.scope, claim, &prepared.key, observed)
                    .await?;
            }
        }
        self.store.renew(&self.scope, claim, 30).await?;
        let mut pending_versions = Vec::new();
        for prepared in self
            .store
            .load_pending_native_children(&self.scope, claim, NativeChildAction::Stop)
            .await?
        {
            self.store.renew(&self.scope, claim, 30).await?;
            match self.driver.apply_native_child(&prepared, claim).await {
                Ok(Some(result)) => {
                    self.store
                        .complete_native_child(&self.scope, claim, &prepared.input, result)
                        .await?
                }
                Ok(None) => {}
                Err(Error::PreconditionFailed) => pending_versions.push(prepared),
                Err(error) => return Err(error),
            }
        }
        let known = self.store.native_children(&self.scope, claim).await?;
        for old in &known {
            self.store.renew(&self.scope, claim, 30).await?;
            let child = self
                .driver
                .observe_native_child(&deployment, old)
                .await?
                .unwrap_or_else(|| old.clone());
            let prepared = self
                .store
                .prepare_native_child(
                    &self.scope,
                    claim,
                    NativeChildAction::Stop,
                    &closed_key,
                    child,
                )
                .await?;
            if let Some(result) = self.driver.apply_native_child(&prepared, claim).await? {
                self.store
                    .complete_native_child(&self.scope, claim, &prepared.input, result)
                    .await?;
            }
        }
        // Registered UIDs remain ours after their ReplicaSet is collected.
        // Observe those first so an unrelated discovery conflict cannot prevent
        // finite retained-history cleanup or stopping a known terminating UID.
        let discovered = match self
            .driver
            .discover_native_children(&deployment, None, claim)
            .await
        {
            Ok(children) => children,
            Err(Error::OwnershipConflict) if !known.is_empty() => {
                return Ok(ConditionReason::Draining);
            }
            Err(error) => return Err(error),
        };
        for child in discovered {
            if known.iter().any(|old| old.pod_uid == child.pod_uid) {
                continue;
            }
            self.store.renew(&self.scope, claim, 30).await?;
            let prepared = self
                .store
                .prepare_native_child(
                    &self.scope,
                    claim,
                    NativeChildAction::Stop,
                    &closed_key,
                    child,
                )
                .await?;
            if let Some(result) = self.driver.apply_native_child(&prepared, claim).await? {
                self.store
                    .complete_native_child(&self.scope, claim, &prepared.input, result)
                    .await?;
            }
        }
        for prepared in pending_versions {
            self.store.renew(&self.scope, claim, 30).await?;
            if let Some(result) = self.driver.apply_native_child(&prepared, claim).await? {
                self.store
                    .complete_native_child(&self.scope, claim, &prepared.input, result)
                    .await?;
            }
        }
        // The original drain barrier also covers terminal execution. Do not
        // complete suspension or re-enable while execution draining is pending.
        Ok(ConditionReason::Draining)
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
