//! The installation's controller RBAC and static admission policies. The
//! chart installs them; the controller can write neither. Parameters are
//! rendered into the policies as literals from the installation values.
//!
//! What they enforce, whatever the controller renders:
//! - only the controller writes instance namespaces, only with the prefix and
//!   labels, and never deletes them;
//! - only the controller (and the cluster's own controllers) write inside
//!   instance namespaces, and the controller writes nowhere else except its
//!   leader lease, namespaces and PersistentVolumes;
//! - Pods are non-root with the data identity, unprivileged, without host
//!   access or tokens, with digest-pinned images and approved volume types;
//! - PersistentVolumes stay under approved roots and bind into instance
//!   namespaces with `Retain`;
//! - RoleBindings bind only the instance role to the controller;
//! - Services stay cluster-internal.

use hermes_core::domain::storage::SourceKind;
use serde_json::{Value, json};

use crate::{INSTALLATION_LABEL, MANAGED_BY, MANAGER, Object, RenderConfig, RenderError};

const POLICY_API: &str = "admissionregistration.k8s.io/v1";

fn literal(value: &str) -> String {
    // CEL single-quoted string; inputs are validated names, paths and hosts.
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn object(
    api_version: &'static str,
    kind: &'static str,
    name: &str,
    namespace: Option<&str>,
    mut body: Value,
) -> Object {
    body["apiVersion"] = json!(api_version);
    body["kind"] = json!(kind);
    body["metadata"]["name"] = json!(name);
    body["metadata"]["labels"] = json!({MANAGED_BY: "helm", "app.kubernetes.io/part-of": MANAGER});
    if let Some(namespace) = namespace {
        body["metadata"]["namespace"] = json!(namespace);
    }
    Object {
        api_version,
        kind,
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        body,
    }
}

fn validation(expression: String, message: &str) -> Value {
    json!({"expression": expression, "message": message})
}

/// The controller's user name.
pub fn controller_user(config: &RenderConfig) -> String {
    format!(
        "system:serviceaccount:{}:{}",
        config.control_namespace, config.controller_service_account
    )
}

/// Name prefix of every installation-wide object.
fn base(config: &RenderConfig) -> String {
    format!(
        "{}-{}",
        MANAGER,
        &config.installation.id.simple().to_string()[..12]
    )
}

fn policy(
    config: &RenderConfig,
    suffix: &str,
    rules: Value,
    match_conditions: Value,
    validations: Vec<Value>,
    owned_only: bool,
) -> [Object; 2] {
    let name = format!("{}-{suffix}", base(config));
    let policy = object(
        POLICY_API,
        "ValidatingAdmissionPolicy",
        &name,
        None,
        json!({"metadata": {}, "spec": {
            "failurePolicy": "Fail",
            "matchConstraints": {"resourceRules": rules},
            "matchConditions": match_conditions,
            "validations": validations,
        }}),
    );
    let mut resources = json!({});
    if owned_only {
        resources = json!({"namespaceSelector": {"matchLabels": {INSTALLATION_LABEL: config.installation.id.to_string()}}});
    }
    let binding = object(
        POLICY_API,
        "ValidatingAdmissionPolicyBinding",
        &name,
        None,
        json!({"metadata": {}, "spec": {
            "policyName": name,
            "validationActions": ["Deny"],
            "matchResources": resources,
        }}),
    );
    [policy, binding]
}

fn rule(groups: &[&str], resources: &[&str], operations: &[&str]) -> Value {
    json!({"apiGroups": groups, "apiVersions": ["*"], "resources": resources, "operations": operations})
}

/// The controller's RBAC: cluster rights to bootstrap instance namespaces and
/// static volumes, the per-namespace instance role, and its leader lease.
pub fn rbac(config: &RenderConfig) -> Result<Vec<Object>, RenderError> {
    config.validate()?;
    let controller_role = format!("{}-controller", base(config));
    let subject = json!([{"kind": "ServiceAccount", "name": config.controller_service_account, "namespace": config.control_namespace}]);
    Ok(vec![
        object(
            "rbac.authorization.k8s.io/v1",
            "ClusterRole",
            &controller_role,
            None,
            json!({"metadata": {}, "rules": [
                {"apiGroups": [""], "resources": ["namespaces"], "verbs": ["get", "list", "watch", "create", "patch"]},
                {"apiGroups": [""], "resources": ["persistentvolumes"], "verbs": ["get", "list", "watch", "create", "patch"]},
                {"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["rolebindings"], "verbs": ["get", "create", "patch"]},
                {"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["clusterroles"], "verbs": ["bind"], "resourceNames": [config.instance_role]},
            ]}),
        ),
        object(
            "rbac.authorization.k8s.io/v1",
            "ClusterRoleBinding",
            &controller_role,
            None,
            json!({"metadata": {}, "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": controller_role}, "subjects": subject}),
        ),
        object(
            "rbac.authorization.k8s.io/v1",
            "ClusterRole",
            &config.instance_role,
            None,
            json!({"metadata": {}, "rules": [
                {"apiGroups": [""], "resources": ["secrets", "configmaps", "services", "serviceaccounts", "persistentvolumeclaims"],
                 "verbs": ["get", "list", "watch", "create", "patch"]},
                {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list", "watch", "create", "patch", "delete"]},
                {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get", "list", "watch", "create", "patch", "delete"]},
                {"apiGroups": ["apps"], "resources": ["replicasets"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": [""], "resources": ["events"], "verbs": ["list"]},
                {"apiGroups": ["networking.k8s.io"], "resources": ["networkpolicies"], "verbs": ["get", "list", "watch", "create", "patch"]},
                {"apiGroups": ["rbac.authorization.k8s.io"], "resources": ["rolebindings"], "verbs": ["get", "patch"]},
            ]}),
        ),
        object(
            "rbac.authorization.k8s.io/v1",
            "Role",
            &controller_role,
            Some(&config.control_namespace),
            json!({"metadata": {}, "rules": [
                {"apiGroups": ["coordination.k8s.io"], "resources": ["leases"], "verbs": ["get", "create", "update", "patch"]},
            ]}),
        ),
        object(
            "rbac.authorization.k8s.io/v1",
            "RoleBinding",
            &controller_role,
            Some(&config.control_namespace),
            json!({"metadata": {}, "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": controller_role}, "subjects": subject}),
        ),
    ])
}

/// The static admission policies and their bindings.
pub fn policies(config: &RenderConfig) -> Result<Vec<Object>, RenderError> {
    config.validate()?;
    let controller = literal(&controller_user(config));
    let id = config.installation.id.to_string();
    let prefix = literal(&format!("{}-", config.installation.namespace_prefix()));
    let user = "request.userInfo.username";
    let mut objects = Vec::new();

    objects.extend(policy(
        config,
        "namespaces",
        json!([rule(&[""], &["namespaces"], &["CREATE", "UPDATE", "DELETE"])]),
        json!([]),
        vec![
            validation(
                format!("request.operation == 'DELETE' || !object.metadata.name.startsWith({prefix}) || {user} == {controller}"),
                "only the manager controller writes instance namespaces",
            ),
            validation(
                format!("{user} != {controller} || request.operation != 'DELETE'"),
                "the manager controller never deletes namespaces",
            ),
            validation(
                format!(
                    "{user} != {controller} || request.operation == 'DELETE' || \
                     (object.metadata.name.startsWith({prefix}) && has(object.metadata.labels) && \
                      object.metadata.labels[{installation}] == {id} && \
                      object.metadata.labels[{managed}] == {manager} && \
                      object.metadata.labels['pod-security.kubernetes.io/enforce'] == 'restricted' && \
                      {instance} in object.metadata.labels && \
                      object.metadata.name == {prefix} + object.metadata.labels[{instance}].replace('-', ''))",
                    installation = literal(INSTALLATION_LABEL),
                    id = literal(&id),
                    managed = literal(MANAGED_BY),
                    manager = literal(MANAGER),
                    instance = literal(crate::INSTANCE_LABEL),
                ),
                "instance namespaces carry the installation's prefix and labels",
            ),
        ],
        false,
    ));

    // The cluster's own components bind volumes, schedule Pods and run them.
    let system = format!(
        "{user} == {controller} || {user} == 'system:kube-controller-manager' || {user} == 'system:kube-scheduler' || \
         {user} == 'system:apiserver' || {user}.startsWith('system:serviceaccount:kube-system:') || \
         {user}.startsWith('system:node:')"
    );
    objects.extend(policy(
        config,
        "owned-writes",
        json!([rule(&["*"], &["*"], &["CREATE", "UPDATE", "DELETE"])]),
        // A namespaceSelector never skips cluster-scoped objects; only
        // namespaced ones and the namespaces themselves are in scope.
        json!([
            {"name": "namespaced", "expression": "(has(request.namespace) && request.namespace != '') || request.resource.resource == 'namespaces'"},
            {"name": "not-events", "expression": "request.resource.resource != 'events'"},
        ]),
        vec![validation(
            system,
            "only the manager controller writes in instance namespaces",
        )],
        true,
    ));

    objects.extend(policy(
        config,
        "controller-scope",
        json!([rule(&["*"], &["*"], &["CREATE", "UPDATE", "DELETE"])]),
        json!([{"name": "controller", "expression": format!("{user} == {controller}")}]),
        vec![validation(
            format!(
                "request.resource.resource in ['namespaces', 'persistentvolumes'] || \
                 (has(request.namespace) && request.namespace.startsWith({prefix})) || \
                 (has(request.namespace) && request.namespace == {control} && \
                  request.resource.group == 'coordination.k8s.io' && request.resource.resource == 'leases')",
                control = literal(&config.control_namespace),
            ),
            "the manager controller writes only in instance namespaces",
        )],
        false,
    ));

    let identity = config.data_identity;
    let containers = "(has(object.spec.initContainers) ? object.spec.containers + object.spec.initContainers : object.spec.containers)";
    objects.extend(policy(
        config,
        "pods",
        json!([rule(&[""], &["pods", "pods/ephemeralcontainers"], &["CREATE", "UPDATE"])]),
        json!([]),
        vec![
            validation(
                format!(
                    "has(object.spec.securityContext) && object.spec.securityContext.runAsNonRoot == true && \
                     object.spec.securityContext.runAsUser == {uid} && object.spec.securityContext.runAsGroup == {gid}",
                    uid = identity.uid,
                    gid = identity.gid,
                ),
                "Pods run as the installation's non-root data identity",
            ),
            // A container's own security context overrides the Pod's.
            validation(
                format!(
                    "{containers}.all(c, !has(c.securityContext) || \
                     ((!has(c.securityContext.runAsUser) || c.securityContext.runAsUser == {uid}) && \
                     (!has(c.securityContext.runAsGroup) || c.securityContext.runAsGroup == {gid}) && \
                     (!has(c.securityContext.runAsNonRoot) || c.securityContext.runAsNonRoot == true)))",
                    uid = identity.uid,
                    gid = identity.gid,
                ),
                "containers keep the Pod's data identity",
            ),
            validation(
                format!(
                    "{containers}.all(c, has(c.securityContext) && c.securityContext.allowPrivilegeEscalation == false && \
                     (!has(c.securityContext.privileged) || c.securityContext.privileged == false) && \
                     has(c.securityContext.capabilities) && has(c.securityContext.capabilities.drop) && \
                     'ALL' in c.securityContext.capabilities.drop && \
                     (!has(c.securityContext.capabilities.add) || size(c.securityContext.capabilities.add) == 0))"
                ),
                "containers are unprivileged and drop every capability",
            ),
            validation(
                format!("{containers}.all(c, c.image.matches('^[^@\\\\s]+@sha256:[a-f0-9]{{64}}$'))"),
                "images are pinned by digest",
            ),
            validation(
                "!(has(object.spec.hostNetwork) && object.spec.hostNetwork) && !(has(object.spec.hostPID) && object.spec.hostPID) && \
                 !(has(object.spec.hostIPC) && object.spec.hostIPC)"
                    .into(),
                "Pods use no host namespaces",
            ),
            validation(
                "has(object.spec.automountServiceAccountToken) && object.spec.automountServiceAccountToken == false".into(),
                "Pods carry no Kubernetes token",
            ),
            validation(
                "!has(object.spec.volumes) || object.spec.volumes.all(v, has(v.persistentVolumeClaim) || has(v.secret) || \
                 has(v.configMap) || has(v.emptyDir) || \
                 (has(v.projected) && v.projected.sources.all(s, has(s.secret) || has(s.configMap))))"
                    .into(),
                "volumes are claims, Secrets, ConfigMaps or emptyDirs only",
            ),
            validation(
                "!has(object.spec.ephemeralContainers) || size(object.spec.ephemeralContainers) == 0".into(),
                "no ephemeral containers",
            ),
        ],
        true,
    ));

    let mut approved = Vec::new();
    for source in config.sources.iter() {
        match &source.kind {
            SourceKind::LocalPath { node, root } => approved.push(format!(
                "(has(object.spec.local) && object.spec.local.path.startsWith({root}) && \
                 has(object.spec.nodeAffinity) && object.spec.nodeAffinity.required.nodeSelectorTerms.all(t, \
                 t.matchExpressions.all(e, e.key == 'kubernetes.io/hostname' && e.operator == 'In' && e.values == [{node}])))",
                root = literal(&format!("{}/", root.trim_end_matches('/'))),
                node = literal(node),
            )),
            SourceKind::Nfs { server, root } => approved.push(format!(
                "(has(object.spec.nfs) && object.spec.nfs.server == {server} && object.spec.nfs.path.startsWith({root}))",
                server = literal(server),
                root = literal(&format!("{}/", root.trim_end_matches('/'))),
            )),
            SourceKind::StorageClass { .. } => {}
        }
    }
    let approved = if approved.is_empty() {
        "false".to_owned()
    } else {
        approved.join(" || ")
    };
    objects.extend(policy(
        config,
        "volumes",
        json!([rule(&[""], &["persistentvolumes"], &["CREATE", "UPDATE"])]),
        json!([{"name": "controller", "expression": format!("{user} == {controller}")}]),
        vec![
            validation(
                format!(
                    "object.spec.persistentVolumeReclaimPolicy == 'Retain' && has(object.spec.claimRef) && \
                     object.spec.claimRef.namespace.startsWith({prefix}) && \
                     (!has(object.spec.storageClassName) || object.spec.storageClassName == '')"
                ),
                "static volumes are retained and bind into instance namespaces",
            ),
            validation(
                format!(
                    "!(has(object.spec.local) && (object.spec.local.path.contains('/../') || object.spec.local.path.endsWith('/..'))) && \
                     !(has(object.spec.nfs) && (object.spec.nfs.path.contains('/../') || object.spec.nfs.path.endsWith('/..'))) && \
                     ({approved})"
                ),
                "static volumes stay under approved storage roots",
            ),
        ],
        false,
    ));

    objects.extend(policy(
        config,
        "rolebindings",
        json!([rule(&["rbac.authorization.k8s.io"], &["rolebindings"], &["CREATE", "UPDATE"])]),
        json!([]),
        vec![validation(
            format!(
                "object.roleRef.kind == 'ClusterRole' && object.roleRef.name == {role} && size(object.subjects) == 1 && \
                 object.subjects[0].kind == 'ServiceAccount' && object.subjects[0].name == {account} && \
                 object.subjects[0].namespace == {control}",
                role = literal(&config.instance_role),
                account = literal(&config.controller_service_account),
                control = literal(&config.control_namespace),
            ),
            "instance namespaces bind only the instance role to the controller",
        )],
        true,
    ));

    objects.extend(policy(
        config,
        "services",
        json!([rule(&[""], &["services"], &["CREATE", "UPDATE"])]),
        json!([]),
        vec![validation(
            "(!has(object.spec.type) || object.spec.type == 'ClusterIP') && \
             (!has(object.spec.externalIPs) || size(object.spec.externalIPs) == 0)"
                .into(),
            "Services stay inside the cluster",
        )],
        true,
    ));
    Ok(objects)
}
