//! Approved resource identities; names alone never establish ownership.
use crate::{Invalid, validate_name, validate_sha256};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    Namespace,
    RoleBinding,
    ServiceAccount,
    PersistentVolumeClaim,
    Secret,
    ConfigMap,
    NetworkPolicy,
    Service,
    Deployment,
    Pod,
}
impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Namespace => "Namespace",
            Self::RoleBinding => "RoleBinding",
            Self::ServiceAccount => "ServiceAccount",
            Self::PersistentVolumeClaim => "PersistentVolumeClaim",
            Self::Secret => "Secret",
            Self::ConfigMap => "ConfigMap",
            Self::NetworkPolicy => "NetworkPolicy",
            Self::Service => "Service",
            Self::Deployment => "Deployment",
            Self::Pod => "Pod",
        }
    }
    pub fn api_version(self) -> &'static str {
        match self {
            Self::RoleBinding => "rbac.authorization.k8s.io/v1",
            Self::NetworkPolicy => "networking.k8s.io/v1",
            Self::Deployment => "apps/v1",
            _ => "v1",
        }
    }
    pub fn plural(self) -> &'static str {
        match self {
            Self::Namespace => "namespaces",
            Self::RoleBinding => "rolebindings",
            Self::ServiceAccount => "serviceaccounts",
            Self::PersistentVolumeClaim => "persistentvolumeclaims",
            Self::Secret => "secrets",
            Self::ConfigMap => "configmaps",
            Self::NetworkPolicy => "networkpolicies",
            Self::Service => "services",
            Self::Deployment => "deployments",
            Self::Pod => "pods",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Retain,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceSpec {
    pub kind: ResourceKind,
    pub namespace: Option<String>,
    pub namespace_uid: Option<String>,
    pub name: String,
    pub manifest_digest: String,
    pub purpose: String,
    pub retention: Retention,
    pub encrypted_material_ref: Option<Uuid>,
}
impl ResourceSpec {
    pub fn validate(&self) -> Result<(), Invalid> {
        validate_name(&self.name)?;
        validate_name(&self.purpose)?;
        validate_sha256(&self.manifest_digest)?;
        if self.kind == ResourceKind::Namespace {
            if self.namespace.is_some()
                || self.namespace_uid.is_some()
                || self.retention != Retention::Retain
            {
                return Err(Invalid::Identifier);
            }
        } else {
            validate_name(self.namespace.as_deref().ok_or(Invalid::Identifier)?)?;
            if self
                .namespace_uid
                .as_deref()
                .is_none_or(|uid| !valid_uid(uid))
            {
                return Err(Invalid::Identifier);
            }
        }
        if self.kind == ResourceKind::PersistentVolumeClaim && self.retention != Retention::Retain {
            return Err(Invalid::Identifier);
        }
        if self.kind == ResourceKind::Secret && self.encrypted_material_ref.is_none() {
            return Err(Invalid::Identifier);
        }
        if self.encrypted_material_ref.is_some_and(|id| id.is_nil()) {
            return Err(Invalid::Identifier);
        }
        Ok(())
    }
}
pub fn valid_uid(uid: &str) -> bool {
    !uid.is_empty() && uid.len() <= 255 && uid.bytes().all(|b| b.is_ascii_graphic())
}
