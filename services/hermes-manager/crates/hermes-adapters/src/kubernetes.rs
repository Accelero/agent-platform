//! Bounded Kubernetes ownership transport. Never infer a workstation kubeconfig.
mod namespace_fence;
mod native_children;
mod native_stop;
mod observation;
mod quantity;
mod readiness;
pub use readiness::NativeReadiness;

use crate::ownership::OwnershipSigner;
use hermes_application::{
    Action, Error,
    resources::{Creation, OwnedResource, PreparedResource, ResourceObservation, ResourceVersion},
};
use hermes_domain::resources::{ResourceKind, ResourceSpec, Retention};
use kube::{
    Api, Client, Config,
    api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions, PropagationPolicy},
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::timeout;

const INSTALLATION: &str = "agent-platform.io/installation";
const INSTANCE: &str = "agent-platform.io/instance";
const RESOURCE: &str = "agent-platform.io/resource";
const PROOF: &str = "agent-platform.io/creation-proof";
const INTENT: &str = "agent-platform.io/applied-intent";

#[derive(Clone)]
pub struct KubernetesDriver {
    client: Client,
    signer: OwnershipSigner,
    cluster_identity: String,
}

/// Reviewed renderer output only, never an HTTP-deserialized arbitrary Pod spec.
/// Deliberately no Debug: Secret manifests may contain bootstrap credentials.
pub struct Manifest(Value);
impl Manifest {
    pub(crate) fn value(&self) -> &Value {
        &self.0
    }
    pub fn new(value: Value) -> Result<Self, Error> {
        if !value.is_object()
            || serde_json::to_vec(&value)
                .map_err(|_| Error::Invalid)?
                .len()
                > 65536
        {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
    pub fn digest(&self) -> Result<String, Error> {
        let digest = Sha256::digest(serde_json::to_vec(&self.0).map_err(|_| Error::Invalid)?);
        Ok(digest.iter().map(|v| format!("{v:02x}")).collect())
    }
    pub(crate) fn validate(&self, spec: &ResourceSpec) -> Result<(), Error> {
        if self.0.get("apiVersion").and_then(Value::as_str) != Some(spec.kind.api_version())
            || self.0.get("kind").and_then(Value::as_str) != Some(spec.kind.as_str())
            || self.0.pointer("/metadata/name").and_then(Value::as_str) != Some(spec.name.as_str())
            || self
                .0
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                != spec.namespace.as_deref()
            || self.0.get("status").is_some()
            || self.digest()? != spec.manifest_digest
        {
            return Err(Error::OwnershipConflict);
        }
        let meta = self
            .0
            .get("metadata")
            .and_then(Value::as_object)
            .ok_or(Error::Invalid)?;
        if meta.keys().any(|key| {
            !matches!(
                key.as_str(),
                "name" | "namespace" | "labels" | "annotations"
            )
        }) {
            return Err(Error::Invalid);
        }
        for field in ["labels", "annotations"] {
            if let Some(fields) = meta.get(field) {
                let fields = fields.as_object().ok_or(Error::Invalid)?;
                if fields
                    .keys()
                    .any(|key| key.starts_with("agent-platform.io/"))
                {
                    return Err(Error::Invalid);
                }
            }
        }
        if spec.kind == ResourceKind::Secret && self.0.get("stringData").is_some() {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
fn api_error(error: kube::Error) -> Error {
    match error {
        kube::Error::Api(response) => match response.code {
            409 => Error::PreconditionFailed,
            401 | 403 => Error::Forbidden,
            400 | 422 => Error::Invalid,
            _ => Error::Unavailable,
        },
        _ => Error::Unavailable,
    }
}
impl KubernetesDriver {
    pub fn from_config(
        mut config: Config,
        signer: OwnershipSigner,
        cluster_identity: String,
    ) -> Result<Self, Error> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if config.cluster_url.scheme_str() != Some("https")
            || config.accept_invalid_certs
            || config.proxy_url.is_some()
            || config.auth_info.exec.is_some()
            || config.auth_info.auth_provider.is_some()
            || config.auth_info.impersonate.is_some()
            || config.auth_info.impersonate_groups.is_some()
            || config.auth_info.impersonate_uid.is_some()
            || config.auth_info.impersonate_user_extra.is_some()
            || !config.headers.is_empty()
            || cluster_identity.is_empty()
            || cluster_identity.len() > 255
        {
            return Err(Error::Invalid);
        }
        config.connect_timeout = Some(Duration::from_secs(2));
        config.read_timeout = Some(Duration::from_secs(5));
        config.write_timeout = Some(Duration::from_secs(5));
        // Uncertain writes are observed by the durable reconciler, never retried
        // invisibly by the HTTP layer.
        config.default_retry = false;
        Ok(Self {
            client: Client::try_from(config).map_err(|_| Error::Unavailable)?,
            signer,
            cluster_identity,
        })
    }
    pub fn incluster(signer: OwnershipSigner, cluster_identity: String) -> Result<Self, Error> {
        Self::from_config(
            Config::incluster().map_err(|_| Error::Unavailable)?,
            signer,
            cluster_identity,
        )
    }
    pub async fn verify_bootstrap(
        &self,
        policy: &crate::bootstrap::BootstrapPolicy,
    ) -> Result<(), Error> {
        policy.verify(&self.client).await
    }
    /// Read-only check of a previously bound UID and its exact approved fields.
    /// Unlike a resource-version observation, this also checks the applied intent.
    pub async fn verify_bound_manifest(
        &self,
        owned: &OwnedResource,
        manifest: &Manifest,
    ) -> Result<(), Error> {
        self.readiness_object(owned, Some(manifest)).await?;
        Ok(())
    }
    pub async fn verify_cluster(&self, registered_identity: &str) -> Result<(), Error> {
        if registered_identity != self.cluster_identity {
            return Err(Error::OwnershipConflict);
        }
        let namespaces: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
        let cluster = timeout(
            Duration::from_secs(6),
            namespaces.get_metadata("kube-system"),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        if cluster.metadata.uid.as_deref() != Some(registered_identity) {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    fn api(&self, spec: &ResourceSpec) -> Api<DynamicObject> {
        let (group, version) = spec
            .kind
            .api_version()
            .split_once('/')
            .unwrap_or(("", spec.kind.api_version()));
        let resource = ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk(group, version, spec.kind.as_str()),
            spec.kind.plural(),
        );
        match &spec.namespace {
            Some(ns) => Api::namespaced_with(self.client.clone(), ns, &resource),
            None => Api::all_with(self.client.clone(), &resource),
        }
    }
    fn context(&self, creation: &Creation) -> Result<(), Error> {
        if creation.cluster_identity != self.cluster_identity {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    fn prepared(&self, prepared: &PreparedResource) -> Result<(), Error> {
        if matches!(
            prepared.step.result,
            hermes_application::StepResult::Reconciled { .. }
        ) {
            return Err(Error::StaleFence);
        }
        let creation = &prepared.resource.creation;
        let spec = &creation.spec;
        let intent = &prepared.step.intent;
        intent.validate()?;
        spec.validate()?;
        self.context(creation)?;
        if intent.creation_id != creation.id
            || intent.kind != spec.kind.as_str()
            || intent.namespace != spec.namespace
            || intent.namespace_uid != spec.namespace_uid
            || intent.name != spec.name
            || intent.encrypted_material_ref != spec.encrypted_material_ref
            || (prepared.resource.uid.is_none() && intent.manifest_digest != spec.manifest_digest)
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(())
    }
    pub async fn current_version(&self, owned: &OwnedResource) -> Result<ResourceVersion, Error> {
        if owned.uid.is_none() || owned.state != "Bound" {
            return Err(Error::OwnershipConflict);
        }
        self.namespace(&owned.creation).await?;
        let spec = &owned.creation.spec;
        let object = timeout(Duration::from_secs(6), self.api(spec).get_opt(&spec.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?
            .ok_or(Error::OwnershipConflict)?;
        self.verify(owned, &object)?;
        Ok(ResourceVersion {
            uid: object.metadata.uid.ok_or(Error::OwnershipConflict)?,
            resource_version: object
                .metadata
                .resource_version
                .ok_or(Error::OwnershipConflict)?,
        })
    }
    async fn namespace(&self, creation: &Creation) -> Result<(), Error> {
        self.context(creation)?;
        self.verify_cluster(&creation.cluster_identity).await?;
        if let Some(namespace) = &creation.spec.namespace {
            let api: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(self.client.clone());
            let ns = timeout(Duration::from_secs(6), api.get_metadata(namespace))
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?;
            if ns.metadata.uid != creation.spec.namespace_uid
                || ns.metadata.deletion_timestamp.is_some()
                || ns
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(INSTALLATION))
                    != Some(&creation.installation_id.to_string())
                || ns
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(INSTANCE))
                    != Some(&creation.instance_id.to_string())
            {
                return Err(Error::OwnershipConflict);
            }
        }
        Ok(())
    }
    fn decorated(&self, creation: &Creation, manifest: &Manifest) -> Result<DynamicObject, Error> {
        let mut object: DynamicObject =
            serde_json::from_value(manifest.0.clone()).map_err(|_| Error::Invalid)?;
        let labels = object.metadata.labels.get_or_insert_with(BTreeMap::new);
        labels.insert(INSTALLATION.into(), creation.installation_id.to_string());
        labels.insert(INSTANCE.into(), creation.instance_id.to_string());
        labels.insert(RESOURCE.into(), creation.id.to_string());
        let annotations = object
            .metadata
            .annotations
            .get_or_insert_with(BTreeMap::new);
        annotations.insert(PROOF.into(), self.signer.sign(creation)?);
        annotations.insert(INTENT.into(), manifest.digest()?);
        Ok(object)
    }
    fn verify(&self, owned: &OwnedResource, object: &DynamicObject) -> Result<(), Error> {
        let creation = &owned.creation;
        let spec = &creation.spec;
        self.context(creation)?;
        if object.metadata.name.as_deref() != Some(spec.name.as_str())
            || object.metadata.namespace != spec.namespace
            || object.metadata.deletion_timestamp.is_some()
            || object
                .metadata
                .uid
                .as_deref()
                .is_none_or(|uid| !hermes_domain::resources::valid_uid(uid))
            || owned
                .uid
                .as_ref()
                .is_some_and(|uid| Some(uid) != object.metadata.uid.as_ref())
        {
            return Err(Error::OwnershipConflict);
        }
        let labels = object
            .metadata
            .labels
            .as_ref()
            .ok_or(Error::OwnershipConflict)?;
        if labels.get(INSTALLATION) != Some(&creation.installation_id.to_string())
            || labels.get(INSTANCE) != Some(&creation.instance_id.to_string())
            || labels.get(RESOURCE) != Some(&creation.id.to_string())
        {
            return Err(Error::OwnershipConflict);
        }
        let proof = object
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(PROOF))
            .ok_or(Error::OwnershipConflict)?;
        self.signer.verify(creation, proof)
    }
    fn observation(
        &self,
        prepared: &PreparedResource,
        object: &DynamicObject,
        manifest: &Manifest,
    ) -> Result<ResourceObservation, Error> {
        self.verify(&prepared.resource, object)?;
        let mut expected: DynamicObject =
            serde_json::from_value(manifest.0.clone()).map_err(|_| Error::Invalid)?;
        // Creation provenance is immutable across later policy/template updates.
        expected.metadata.labels = manifest
            .0
            .pointer("/metadata/labels")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::Invalid)?;
        let actual = serde_json::to_value(object).map_err(|_| Error::Invalid)?;
        let wanted = serde_json::to_value(expected).map_err(|_| Error::Invalid)?;
        if !contains_fields(&actual, &wanted)
            || object
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(INTENT))
                != Some(&manifest.digest()?)
        {
            return Err(Error::OwnershipConflict);
        }
        Ok(ResourceObservation {
            uid: object
                .metadata
                .uid
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            resource_version: object
                .metadata
                .resource_version
                .clone()
                .ok_or(Error::OwnershipConflict)?,
            manifest_digest: manifest.digest()?,
        })
    }
    pub async fn observe(
        &self,
        prepared: &PreparedResource,
        manifest: &Manifest,
    ) -> Result<Option<ResourceObservation>, Error> {
        self.prepared(prepared)?;
        manifest.validate(&step_spec(prepared))?;
        self.namespace(&prepared.resource.creation).await?;
        let object = timeout(
            Duration::from_secs(6),
            self.api(&prepared.resource.creation.spec)
                .get_opt(&prepared.resource.creation.spec.name),
        )
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(api_error)?;
        match object {
            Some(object) => Ok(Some(self.observation(prepared, &object, manifest)?)),
            None if prepared.resource.uid.is_some() => Err(Error::OwnershipConflict),
            None => Ok(None),
        }
    }
    /// Call only after committing this exact action intent and rechecking the
    /// operation lease. This method never adopts by name or regenerates material.
    pub async fn apply(
        &self,
        prepared: &PreparedResource,
        manifest: &Manifest,
    ) -> Result<ResourceObservation, Error> {
        self.prepared(prepared)?;
        if prepared.step.intent.action != Action::Apply
            || !matches!(prepared.resource.state.as_str(), "Reserved" | "Bound")
        {
            return Err(Error::OwnershipConflict);
        }
        manifest.validate(&step_spec(prepared))?;
        self.namespace(&prepared.resource.creation).await?;
        let spec = &prepared.resource.creation.spec;
        let api = self.api(spec);
        let existing = timeout(Duration::from_secs(6), api.get_opt(&spec.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
        let object = match existing {
            Some(object) => {
                self.verify(&prepared.resource, &object)?;
                if self.observation(prepared, &object, manifest).is_ok() {
                    return self.observation(prepared, &object, manifest);
                }
                // An unbound existing object must exactly match its original
                // creation before first UID adoption; never repair it to fit.
                if prepared.resource.uid.is_none() {
                    return Err(Error::OwnershipConflict);
                }
                if object.metadata.uid != prepared.step.intent.expected_uid
                    || object.metadata.resource_version
                        != prepared.step.intent.expected_resource_version
                {
                    return Err(Error::PreconditionFailed);
                }
                let mut desired = self.decorated(&prepared.resource.creation, manifest)?;
                desired.metadata.uid = object.metadata.uid;
                desired.metadata.resource_version = object.metadata.resource_version;
                timeout(
                    Duration::from_secs(6),
                    api.patch(&spec.name, &PatchParams::default(), &Patch::Merge(&desired)),
                )
                .await
                .map_err(|_| Error::Unavailable)?
                .map_err(api_error)?
            }
            None => {
                if prepared.resource.uid.is_some() || prepared.step.intent.expected_uid.is_some() {
                    return Err(Error::OwnershipConflict);
                }
                let desired = self.decorated(&prepared.resource.creation, manifest)?;
                let params = PostParams::default();
                timeout(Duration::from_secs(6), api.create(&params, &desired))
                    .await
                    .map_err(|_| Error::Unavailable)?
                    .map_err(api_error)?
            }
        };
        self.observation(prepared, &object, manifest)
    }
    pub async fn delete(&self, prepared: &PreparedResource) -> Result<(), Error> {
        self.prepared(prepared)?;
        if prepared.step.intent.action != Action::Delete
            || prepared.resource.state != "Retiring"
            || prepared.resource.creation.spec.retention != Retention::Delete
        {
            return Err(Error::Forbidden);
        }
        self.namespace(&prepared.resource.creation).await?;
        let spec = &prepared.resource.creation.spec;
        let api = self.api(spec);
        let object = timeout(Duration::from_secs(6), api.get_opt(&spec.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
        let Some(object) = object else { return Ok(()) };
        // A terminating object is still present. A later absence observation
        // must confirm deletion before the repository can finish retirement.
        let mut existing = object.clone();
        existing.metadata.deletion_timestamp = None;
        self.verify(&prepared.resource, &existing)?;
        if object.metadata.uid != prepared.step.intent.expected_uid
            || object.metadata.resource_version != prepared.step.intent.expected_resource_version
        {
            return Err(Error::PreconditionFailed);
        }
        let params = DeleteParams {
            preconditions: Some(Preconditions {
                uid: prepared.step.intent.expected_uid.clone(),
                resource_version: prepared.step.intent.expected_resource_version.clone(),
            }),
            propagation_policy: Some(if spec.kind == ResourceKind::Deployment {
                PropagationPolicy::Foreground
            } else {
                PropagationPolicy::Background
            }),
            ..DeleteParams::default()
        };
        timeout(Duration::from_secs(6), api.delete(&spec.name, &params))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
        Ok(())
    }
    pub async fn absent(&self, prepared: &PreparedResource) -> Result<bool, Error> {
        self.prepared(prepared)?;
        if prepared.step.intent.action != Action::Delete {
            return Err(Error::Invalid);
        }
        self.namespace(&prepared.resource.creation).await?;
        let spec = &prepared.resource.creation.spec;
        let object = timeout(Duration::from_secs(6), self.api(spec).get_opt(&spec.name))
            .await
            .map_err(|_| Error::Unavailable)?
            .map_err(api_error)?;
        match object {
            None => Ok(true),
            Some(mut object) => {
                object.metadata.deletion_timestamp = None;
                self.verify(&prepared.resource, &object)?;
                Ok(false)
            }
        }
    }
}
fn step_spec(prepared: &PreparedResource) -> ResourceSpec {
    let mut spec = prepared.resource.creation.spec.clone();
    spec.manifest_digest = prepared.step.intent.manifest_digest.clone();
    spec
}
/// Kubernetes defaults are allowed, but every intended field and array element
/// must still match. Arbitrary annotations alone cannot attest a changed spec.
fn contains_fields(actual: &Value, wanted: &Value) -> bool {
    let kind = wanted.get("kind").and_then(Value::as_str);
    contains_fields_at(actual, wanted, kind, &mut Vec::new())
}

/// Explicit context for already-reviewed PodSpec fragments (Pod/ReplicaSet
/// readiness). Generic JSON fragments never receive quantity interpretation.
fn contains_pod_spec_fields(actual: &Value, wanted: &Value) -> bool {
    contains_fields_at(actual, wanted, Some("Pod"), &mut vec!["spec"])
}

fn contains_fields_at<'a>(
    actual: &Value,
    wanted: &'a Value,
    kind: Option<&str>,
    path: &mut Vec<&'a str>,
) -> bool {
    match wanted {
        Value::Object(fields) => actual.as_object().is_some_and(|a| {
            fields.iter().all(|(k, v)| {
                path.push(k);
                let matches = match a.get(k) {
                    Some(value) => contains_fields_at(value, v, kind, path),
                    // API encoding omits optional empty slices (for example
                    // NetworkPolicy ingress/egress and container args). An
                    // absent object is deliberately NOT equivalent to {}:
                    // an empty namespaceSelector has distinct semantics.
                    None => v.as_array().is_some_and(Vec::is_empty),
                };
                path.pop();
                matches
            })
        }),
        Value::Array(fields) => actual.as_array().is_some_and(|a| {
            path.push("[]");
            let matches = a.len() == fields.len()
                && a.iter()
                    .zip(fields)
                    .all(|(a, w)| contains_fields_at(a, w, kind, path));
            path.pop();
            matches
        }),
        Value::Null => actual.is_null(),
        _ => actual == wanted || quantity::equivalent(kind, path, actual, wanted),
    }
}

#[async_trait::async_trait]
impl hermes_application::resources::ResourceDriver for KubernetesDriver {
    type Manifest = Manifest;
    async fn current_version(&self, owned: &OwnedResource) -> Result<ResourceVersion, Error> {
        KubernetesDriver::current_version(self, owned).await
    }
    async fn observe(
        &self,
        prepared: &hermes_application::resources::PreparedObservation,
        manifest: &Manifest,
    ) -> Result<Option<ResourceObservation>, Error> {
        self.observe_reserved(prepared, manifest).await
    }
    async fn apply(
        &self,
        prepared: &PreparedResource,
        manifest: &Manifest,
    ) -> Result<ResourceObservation, Error> {
        KubernetesDriver::apply(self, prepared, manifest).await
    }
    async fn delete(&self, prepared: &PreparedResource) -> Result<(), Error> {
        KubernetesDriver::delete(self, prepared).await
    }
    async fn absent(&self, prepared: &PreparedResource) -> Result<bool, Error> {
        KubernetesDriver::absent(self, prepared).await
    }
}
