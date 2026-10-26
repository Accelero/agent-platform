//! Pure renderers: the instance object set, the managed Hermes configuration
//! and the installation's static admission policies and controller RBAC. No
//! Kubernetes client and no I/O: the same inputs give the same objects, so a
//! restarted controller never changes running Pods.

pub mod admission;
pub mod config;
pub mod instance;
pub mod worker;

use std::collections::BTreeMap;
use std::fmt;

use hermes_core::domain::installation::Installation;
use hermes_core::domain::names::validate_name;
use hermes_core::domain::storage::{DataIdentity, StorageSources};
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// Labels every rendered object carries.
pub const MANAGED_BY: &str = "app.kubernetes.io/managed-by";
pub const MANAGER: &str = "hermes-manager";
pub const INSTALLATION_LABEL: &str = "agent-platform.io/installation";
pub const INSTANCE_LABEL: &str = "agent-platform.io/instance";
pub const COMPONENT_LABEL: &str = "app.kubernetes.io/component";
/// Deployment annotation: the desired generation it was applied for.
pub const GENERATION_ANNOTATION: &str = "agent-platform.io/desired-generation";
/// Pod template annotation: digest of every input that needs a restart.
pub const DIGEST_ANNOTATION: &str = "agent-platform.io/config-digest";

/// One rendered Kubernetes object. Its body may hold credentials, so it is
/// never printed.
#[derive(Clone, PartialEq)]
pub struct Object {
    pub api_version: &'static str,
    pub kind: &'static str,
    pub namespace: Option<String>,
    pub name: String,
    pub body: Value,
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}/{}",
            self.kind,
            self.namespace.as_deref().unwrap_or("-"),
            self.name
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RenderError {
    /// The inputs do not describe a renderable instance.
    #[error("invalid render input")]
    Invalid,
}

/// Pods in the manager's namespace an instance may talk to or hear from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Peers {
    /// Labels of the gateway Pods (they reach native Hermes).
    pub gateway: BTreeMap<String, String>,
    /// Labels of the controller Pods (probes of Hermes and workers).
    pub controller: BTreeMap<String, String>,
    /// Labels of the API Pods (the execution API).
    pub api: BTreeMap<String, String>,
}

/// An in-cluster service instances may reach, such as a model gateway.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceEgress {
    pub namespace: String,
    pub labels: BTreeMap<String, String>,
    pub port: u16,
}

/// Installation-wide rendering inputs, from the chart's values.
#[derive(Debug, Clone)]
pub struct RenderConfig {
    pub installation: Installation,
    pub sources: StorageSources,
    pub data_identity: DataIdentity,
    /// The manager's own namespace.
    pub control_namespace: String,
    /// The controller's ServiceAccount, bound into every instance namespace.
    pub controller_service_account: String,
    /// The ClusterRole the controller gets inside instance namespaces.
    pub instance_role: String,
    pub peers: Peers,
    /// `https://…/execution` as the plugin reaches it.
    pub execution_url: String,
    pub execution_port: u16,
    /// PEM bundle that verifies the execution API's certificate.
    pub execution_ca_pem: String,
    /// PEM bundle of a private identity-provider CA, if any.
    pub oidc_ca_pem: Option<String>,
    /// The native dashboard's OIDC client.
    pub native_client_id: String,
    /// The public origin users reach their Hermes at.
    pub public_origin: String,
    pub service_egress: Vec<ServiceEgress>,
    /// Allow HTTPS to public addresses (model providers).
    pub public_https: bool,
}

impl RenderConfig {
    pub fn validate(&self) -> Result<(), RenderError> {
        let names = [
            &self.control_namespace,
            &self.controller_service_account,
            &self.instance_role,
        ];
        let urls = [&self.execution_url, &self.public_origin];
        let valid = names.iter().all(|n| validate_name(n).is_ok())
            && urls
                .iter()
                .all(|u| u.starts_with("https://") && !u.contains(char::is_whitespace))
            && self.execution_port != 0
            && !self.native_client_id.is_empty()
            && self.native_client_id.len() <= 255
            && self.native_client_id.bytes().all(|b| b.is_ascii_graphic())
            && pem_certificates(&self.execution_ca_pem)
            && self.oidc_ca_pem.as_deref().is_none_or(pem_certificates)
            && [&self.peers.gateway, &self.peers.controller, &self.peers.api]
                .iter()
                .all(|labels| valid_labels(labels))
            && self.service_egress.len() <= 32
            && self.service_egress.iter().all(|e| {
                validate_name(&e.namespace).is_ok() && valid_labels(&e.labels) && e.port != 0
            });
        valid.then_some(()).ok_or(RenderError::Invalid)
    }
}

fn valid_labels(labels: &BTreeMap<String, String>) -> bool {
    !labels.is_empty()
        && labels.len() <= 8
        && labels.iter().all(|(key, value)| {
            !key.is_empty()
                && key.len() <= 253
                && !value.is_empty()
                && value.len() <= 63
                && !key.chars().any(char::is_control)
                && !value.chars().any(char::is_control)
        })
}

/// Only certificates, never a private key.
fn pem_certificates(pem: &str) -> bool {
    pem.len() <= 65_536
        && pem.contains("-----BEGIN CERTIFICATE-----")
        && !pem.contains("PRIVATE KEY")
}
