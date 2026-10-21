//! Reviewed personal workload renderer. No caller-supplied Pod/volume manifests.
use crate::{kubernetes::Manifest, native_bootstrap::NativeBootstrap, native_receipt};
use base64::{Engine, engine::general_purpose::STANDARD};
pub use hermes_application::commissioning::TemplateDefinition;
use hermes_application::{AssignmentView, Error, InstallationScope, materials::MaterialContext};
use hermes_domain::{
    Access,
    resources::{ResourceKind, ResourceSpec, Retention},
    validate_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use url::Url;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceEgress {
    pub namespace: String,
    pub labels: BTreeMap<String, String>,
    pub port: u16,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadConfig {
    pub control_namespace: String,
    pub controller_service_account: String,
    pub reconciler_role: String,
    pub public_origin: String,
    pub oidc_client: String,
    pub execution_port: u16,
    pub execution_ca_pem: String,
    pub oidc_ca_pem: Option<String>,
    pub service_egress: Vec<ServiceEgress>,
    pub public_https: bool,
}
impl WorkloadConfig {
    pub fn validate(&self) -> Result<(), Error> {
        for name in [
            &self.control_namespace,
            &self.controller_service_account,
            &self.reconciler_role,
        ] {
            validate_name(name)?;
        }
        let url = Url::parse(&self.public_origin).map_err(|_| Error::Invalid)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || self.execution_port == 0
            || self.oidc_client.is_empty()
            || self.oidc_client.len() > 255
            || !self.oidc_client.bytes().all(|b| b.is_ascii_graphic())
            || self.service_egress.len() > 32
        {
            return Err(Error::Invalid);
        }
        for peer in &self.service_egress {
            validate_name(&peer.namespace)?;
            if peer.labels.is_empty() || peer.labels.len() > 8 || peer.port == 0 {
                return Err(Error::Invalid);
            }
            for (key, value) in &peer.labels {
                if key.is_empty()
                    || key.len() > 253
                    || value.is_empty()
                    || value.len() > 63
                    || key.chars().any(char::is_control)
                    || value.chars().any(char::is_control)
                {
                    return Err(Error::Invalid);
                }
            }
        }
        certificate(&self.execution_ca_pem)?;
        if let Some(ca) = &self.oidc_ca_pem {
            certificate(ca)?;
        }
        Ok(())
    }
}
fn certificate(value: &str) -> Result<(), Error> {
    if value.len() > 65536 || value.contains("PRIVATE KEY") {
        return Err(Error::Invalid);
    }
    let mut input = std::io::BufReader::new(value.as_bytes());
    let mut count = 0;
    while let Some(item) = rustls_pemfile::read_one(&mut input).map_err(|_| Error::Invalid)? {
        let rustls_pemfile::Item::X509Certificate(cert) = item else {
            return Err(Error::Invalid);
        };
        let (remaining, certificate) =
            x509_parser::parse_x509_certificate(cert.as_ref()).map_err(|_| Error::Invalid)?;
        if !remaining.is_empty() || !certificate.is_ca() {
            return Err(Error::Invalid);
        }
        count += 1;
    }
    if count == 0 {
        return Err(Error::Invalid);
    }
    Ok(())
}

// Emit canonical integer quantities so a server round-trip does not change
// an authenticated manifest's intended fields. Definitions contain only whole
// milli-CPU/MiB/GiB, never floating-point quantities.
fn binary_quantity(mut value: u32, units: &[&str]) -> String {
    let mut unit = 0;
    while value.is_multiple_of(1024) && unit + 1 < units.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{value}{}", units[unit])
}

fn cpu_quantity(millis: u32) -> String {
    if millis.is_multiple_of(1000) {
        (millis / 1000).to_string()
    } else {
        format!("{millis}m")
    }
}
pub struct RenderedResource {
    pub key: String,
    pub spec: ResourceSpec,
    pub manifest: Manifest,
}
impl RenderedResource {
    /// Public template inspection cannot decode an opaque Secret manifest.
    pub fn deployment(&self) -> Result<k8s_openapi::api::apps::v1::Deployment, Error> {
        if self.spec.kind != ResourceKind::Deployment {
            return Err(Error::Invalid);
        }
        serde_json::from_value(self.manifest.value().clone()).map_err(|_| Error::Invalid)
    }
}

fn resource(
    kind: ResourceKind,
    name: &str,
    purpose: &str,
    parent: Option<(&str, &str)>,
    retention: Retention,
    material: Option<uuid::Uuid>,
    mut body: Value,
) -> Result<RenderedResource, Error> {
    body["apiVersion"] = json!(kind.api_version());
    body["kind"] = json!(kind.as_str());
    body["metadata"]["name"] = json!(name);
    if let Some((ns, _)) = parent {
        body["metadata"]["namespace"] = json!(ns);
    }
    let manifest = Manifest::new(body)?;
    let spec = ResourceSpec {
        kind,
        namespace: parent.map(|(ns, _)| ns.to_string()),
        namespace_uid: parent.map(|(_, uid)| uid.to_string()),
        name: name.into(),
        manifest_digest: manifest.digest()?,
        purpose: purpose.into(),
        retention,
        encrypted_material_ref: material,
    };
    spec.validate()?;
    Ok(RenderedResource {
        key: purpose.into(),
        spec,
        manifest,
    })
}
pub fn namespace(
    scope: &InstallationScope,
    assignment: &AssignmentView,
) -> Result<RenderedResource, Error> {
    assignment.identity.validate(&scope.issuer)?;
    if assignment.namespace != hermes_domain::instance_namespace(scope.id, assignment.instance_id) {
        return Err(Error::OwnershipConflict);
    }
    resource(
        ResourceKind::Namespace,
        &assignment.namespace,
        "namespace",
        None,
        Retention::Retain,
        None,
        json!({"metadata":{"labels":{"pod-security.kubernetes.io/enforce":"restricted","pod-security.kubernetes.io/enforce-version":"latest"}}}),
    )
}

pub fn render(
    scope: &InstallationScope,
    assignment: &AssignmentView,
    namespace_uid: &str,
    definition: &TemplateDefinition,
    config: &WorkloadConfig,
    material: &MaterialContext,
    bootstrap: &NativeBootstrap,
) -> Result<Vec<RenderedResource>, Error> {
    definition.validate()?;
    config.validate()?;
    assignment.identity.validate(&scope.issuer)?;
    material.validate()?;
    if assignment.access != Access::Enabled
        || material.purpose != crate::native_bootstrap::PURPOSE
        || material.installation_id != scope.id
        || material.instance_id != assignment.instance_id
        || material.epoch > scope.epoch
        || material.generation > assignment.desired_generation
        || assignment.namespace
            != hermes_domain::instance_namespace(scope.id, assignment.instance_id)
        || !hermes_domain::resources::valid_uid(namespace_uid)
    {
        return Err(Error::OwnershipConflict);
    }
    let ns = assignment.namespace.as_str();
    let mut output = vec![];
    let mut add = |kind, name, purpose, retention, reference, body| -> Result<(), Error> {
        output.push(resource(
            kind,
            name,
            purpose,
            Some((ns, namespace_uid)),
            retention,
            reference,
            body,
        )?);
        Ok(())
    };
    add(
        ResourceKind::RoleBinding,
        "instance-reconciler",
        "bootstrap-rbac",
        Retention::Retain,
        None,
        json!({"metadata":{},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":config.reconciler_role},"subjects":[{"kind":"ServiceAccount","name":config.controller_service_account,"namespace":config.control_namespace}]}),
    )?;
    for (name, purpose) in [("hermes", "hermes-account"), ("worker", "worker-account")] {
        add(
            ResourceKind::ServiceAccount,
            name,
            purpose,
            Retention::Retain,
            None,
            json!({"metadata":{},"automountServiceAccountToken":false}),
        )?;
    }
    add(
        ResourceKind::NetworkPolicy,
        "default-deny",
        "network-default",
        Retention::Retain,
        None,
        json!({"metadata":{},"spec":{"podSelector":{},"policyTypes":["Ingress","Egress"],"ingress":[],"egress":[]}}),
    )?;
    let mut egress = vec![
        json!({"to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"kube-system"}},"podSelector":{"matchLabels":{"k8s-app":"kube-dns"}}}],"ports":[{"protocol":"UDP","port":53},{"protocol":"TCP","port":53}]}),
        json!({"to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":config.control_namespace}},"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes-manager-api"}}}],"ports":[{"protocol":"TCP","port":config.execution_port}]}),
        json!({"to":[{"podSelector":{"matchLabels":{"app.kubernetes.io/component":"worker"}}}],"ports":[{"protocol":"TCP","port":2222}]}),
    ];
    for peer in &config.service_egress {
        egress.push(json!({"to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":peer.namespace}},"podSelector":{"matchLabels":peer.labels}}],"ports":[{"protocol":"TCP","port":peer.port}]}));
    }
    if config.public_https {
        egress.push(json!({"to":[{"ipBlock":{"cidr":"0.0.0.0/0","except":["0.0.0.0/8","10.0.0.0/8","100.64.0.0/10","127.0.0.0/8","169.254.0.0/16","172.16.0.0/12","192.0.0.0/24","192.168.0.0/16","198.18.0.0/15","224.0.0.0/4","240.0.0.0/4"]}}],"ports":[{"protocol":"TCP","port":443}]}));
    }
    add(
        ResourceKind::NetworkPolicy,
        "hermes-network",
        "hermes-network",
        Retention::Retain,
        None,
        json!({"metadata":{},"spec":{"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes"}},"policyTypes":["Ingress","Egress"],"ingress":[{"from":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":config.control_namespace}},"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes-router"}}},{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":config.control_namespace}},"podSelector":{"matchLabels":{"app.kubernetes.io/component":"hermes-controller"}}}],"ports":[{"protocol":"TCP","port":9119}]}],"egress":egress}}),
    )?;
    for (name, purpose, size) in [
        ("home", "home-storage", definition.home_gib),
        ("workspace", "workspace-storage", definition.workspace_gib),
    ] {
        add(
            ResourceKind::PersistentVolumeClaim,
            name,
            purpose,
            Retention::Retain,
            None,
            json!({"metadata":{},"spec":{"accessModes":["ReadWriteOnce"],"storageClassName":definition.storage_class,"resources":{"requests":{"storage":binary_quantity(size, &["Gi", "Ti", "Pi"])}}}}),
        )?;
    }
    let files = [
        (
            "dashboard-session-token",
            bootstrap.dashboard_session_token.as_bytes().to_vec(),
        ),
        (
            "execution-client-token",
            bootstrap.execution_client_token.as_bytes().to_vec(),
        ),
        ("init-key", bootstrap.initialization_key.as_bytes().to_vec()),
        ("id_ed25519", bootstrap.ssh_private_key.as_bytes().to_vec()),
        (
            "authorized_keys",
            bootstrap.ssh_public_key.as_bytes().to_vec(),
        ),
        (
            "native-config.json",
            serde_json::to_vec(&bootstrap.native_config).map_err(|_| Error::Invalid)?,
        ),
    ];
    let data: BTreeMap<_, _> = files
        .into_iter()
        .map(|(name, value)| (name, STANDARD.encode(value)))
        .collect();
    add(
        ResourceKind::Secret,
        "native-bootstrap",
        "native-bootstrap",
        Retention::Retain,
        Some(material.id),
        json!({"metadata":{},"type":"Opaque","immutable":true,"data":data}),
    )?;
    let context = native_receipt::context(material, bootstrap, &assignment.identity)?;
    add(
        ResourceKind::ConfigMap,
        "native-owner",
        "native-owner",
        Retention::Retain,
        None,
        json!({"metadata":{},"data":{"context.json":serde_json::to_string(&context).map_err(|_|Error::Invalid)?}}),
    )?;
    // Native managed scope overlays only administrator-selected leaves. It is
    // mounted read-only, independently of the owner's mutable config.yaml.
    let mut managed = definition.admin_config.clone();
    let mut terminal = bootstrap.native_config["terminal"]
        .as_object()
        .ok_or(Error::Invalid)?
        .clone();
    if let Some(approved) = managed.get("terminal").and_then(Value::as_object) {
        terminal.extend(approved.clone());
    }
    managed.insert("terminal".into(), Value::Object(terminal));
    // Connection authority belongs to infrastructure. The profile's
    // terminal.runtime_id stays editable and is checked by the manager.
    let plugins = managed.entry("plugins").or_insert_with(|| json!({}));
    let enabled = plugins
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .entry("enabled")
        .or_insert_with(|| json!([]));
    let enabled = enabled.as_array_mut().ok_or(Error::Invalid)?;
    if !enabled.iter().any(|name| name == "managed_execution") {
        enabled.push(json!("managed_execution"));
    }
    plugins
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .entry("disabled")
        .or_insert_with(|| json!([]));
    let entries = plugins
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .entry("entries")
        .or_insert_with(|| json!({}));
    let execution = entries
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .entry("managed_execution")
        .or_insert_with(|| json!({}));
    let settings = execution
        .as_object_mut()
        .ok_or(Error::Invalid)?
        .entry("settings")
        .or_insert_with(|| json!({}));
    for key in ["manager_url", "token_file", "ca_file", "ssh_key"] {
        settings.as_object_mut().ok_or(Error::Invalid)?.insert(
            key.into(),
            bootstrap.native_config["plugins"]["entries"]["managed_execution"]["settings"][key]
                .clone(),
        );
    }
    let managed = serde_json::to_string(&managed).map_err(|_| Error::Invalid)?;
    let mut trust = BTreeMap::from([
        ("ca.crt", config.execution_ca_pem.as_str()),
        ("config.yaml", managed.as_str()),
        (".env", "TERMINAL_ENV=managed_execution\n"),
    ]);
    if let Some(ca) = &config.oidc_ca_pem {
        trust.insert("oidc-ca.crt", ca);
    }
    add(
        ResourceKind::ConfigMap,
        "native-trust",
        "native-trust",
        Retention::Retain,
        None,
        json!({"metadata":{},"data":trust}),
    )?;
    let pod_labels = json!({"app.kubernetes.io/component":"hermes","agent-platform.io/installation":scope.id.to_string(),"agent-platform.io/instance":assignment.instance_id.to_string(),"agent-platform.io/policy-generation":assignment.desired_generation.to_string(),"agent-platform.io/namespace-uid":namespace_uid});
    let container_security = json!({"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}});
    let mut env = vec![
        json!({"name":"HOME","value":"/opt/data"}),
        json!({"name":"HERMES_HOME","value":"/opt/data"}),
        json!({"name":"HERMES_MANAGED_DIR","value":"/run/hermes/execution-ca"}),
        json!({"name":"HERMES_DASHBOARD_OIDC_ISSUER","value":scope.issuer}),
        json!({"name":"HERMES_DASHBOARD_OIDC_CLIENT_ID","value":config.oidc_client}),
        json!({"name":"HERMES_DASHBOARD_PUBLIC_URL","value":config.public_origin}),
        json!({"name":"HERMES_DASHBOARD_OIDC_SCOPES","value":"openid profile email"}),
        json!({"name":"HERMES_PERSONAL_INSTANCE_ID","value":assignment.instance_id.to_string()}),
        json!({"name":"API_SERVER_ENABLED","value":"false"}),
        json!({"name":"PYTHONDONTWRITEBYTECODE","value":"1"}),
    ];
    if config.oidc_ca_pem.is_some() {
        env.push(json!({"name":"HERMES_PERSONAL_OIDC_CA_FILE","value":"/run/hermes/execution-ca/oidc-ca.crt"}));
    }
    let cpu = cpu_quantity(definition.cpu_millis);
    let memory = binary_quantity(definition.memory_mib, &["Mi", "Gi", "Ti"]);
    let resources =
        json!({"requests":{"cpu":cpu,"memory":memory},"limits":{"cpu":cpu,"memory":memory}});
    let volumes = json!([
        {"name":"home","persistentVolumeClaim":{"claimName":"home"}}, {"name":"workspace","persistentVolumeClaim":{"claimName":"workspace"}},
        {"name":"tmp","emptyDir":{"sizeLimit":"256Mi"}},
        {"name":"owner","configMap":{"name":"native-owner"}}, {"name":"trust","configMap":{"name":"native-trust"}},
        {"name":"init","secret":{"secretName":"native-bootstrap","defaultMode":288,"items":[{"key":"init-key","path":"init-key"},{"key":"native-config.json","path":"native-config.json"}]}},
        {"name":"credentials","secret":{"secretName":"native-bootstrap","defaultMode":288,"items":[{"key":"dashboard-session-token","path":"dashboard-session-token"},{"key":"execution-client-token","path":"execution-client-token"},{"key":"id_ed25519","path":"id_ed25519"}]}},
    ]);
    add(
        ResourceKind::Deployment,
        "hermes",
        "hermes-workload",
        Retention::Delete,
        None,
        json!({"metadata":{},"spec":{"replicas":1,"strategy":{"type":"Recreate"},"selector":{"matchLabels":{"app.kubernetes.io/component":"hermes","agent-platform.io/instance":assignment.instance_id.to_string()}},"template":{"metadata":{"labels":pod_labels},"spec":{
            "serviceAccountName":"hermes","schedulingGates":[{"name":"agent-platform.io/native-start"}],"automountServiceAccountToken":false,"enableServiceLinks":false,"terminationGracePeriodSeconds":45,
            "securityContext":{"runAsNonRoot":true,"runAsUser":1000,"runAsGroup":1000,"fsGroup":1000,"fsGroupChangePolicy":"OnRootMismatch","seccompProfile":{"type":"RuntimeDefault"}},
            "initContainers":[{"name":"native-home","image":definition.image,"command":["/opt/hermes/.venv/bin/python","/usr/local/lib/personal-init.py"],"args":["--context","/run/hermes/owner/context.json"],"securityContext":container_security,"resources":resources,"terminationMessagePath":"/tmp/native-init-result.json","terminationMessagePolicy":"File","volumeMounts":[{"name":"home","mountPath":"/opt/data"},{"name":"tmp","mountPath":"/tmp"},{"name":"owner","mountPath":"/run/hermes/owner","readOnly":true},{"name":"init","mountPath":"/run/hermes/init","readOnly":true}]}],
            "containers":[{"name":"hermes","image":definition.image,"command":["/opt/hermes/.venv/bin/python","/usr/local/lib/personal-run.py"],"args":[],"env":env,"securityContext":container_security,"resources":resources,"ports":[{"name":"dashboard","containerPort":9119}],"startupProbe":{"tcpSocket":{"port":"dashboard"},"periodSeconds":5,"failureThreshold":60},"readinessProbe":{"tcpSocket":{"port":"dashboard"},"periodSeconds":5},"volumeMounts":[{"name":"home","mountPath":"/opt/data"},{"name":"workspace","mountPath":"/workspace"},{"name":"tmp","mountPath":"/tmp"},{"name":"owner","mountPath":"/run/hermes/owner","readOnly":true},{"name":"trust","mountPath":"/run/hermes/execution-ca","readOnly":true},{"name":"credentials","mountPath":"/run/hermes/credentials","readOnly":true}]}],"volumes":volumes
        }}}}),
    )?;
    add(
        ResourceKind::Service,
        "hermes",
        "hermes-service",
        Retention::Retain,
        None,
        json!({"metadata":{},"spec":{"selector":{"app.kubernetes.io/component":"hermes","agent-platform.io/instance":assignment.instance_id.to_string()},"ports":[{"name":"http","port":8080,"targetPort":"dashboard","protocol":"TCP"}]}}),
    )?;
    // Derive policy parameters from the very same reviewed template. There is
    // no independent image/environment/resource default for admission.
    let deployment_index = output
        .iter()
        .position(|r| r.spec.kind == ResourceKind::Deployment)
        .ok_or(Error::Invalid)?;
    let pod = &output[deployment_index].manifest.value()["spec"]["template"]["spec"];
    let policy = resource(
        ResourceKind::InstancePolicy,
        "instance-policy",
        "instance-policy",
        Some((ns, namespace_uid)),
        Retention::Retain,
        None,
        crate::admission::parameters(
            scope,
            assignment.instance_id,
            namespace_uid,
            assignment.desired_generation,
            pod,
        )?,
    )?;
    output.insert(deployment_index, policy);
    Ok(output)
}

#[cfg(test)]
mod tests;
