//! Namespace-local, typed admission parameters rendered from approved policy.
//! Shared policy/CRD installation is a trusted release action, never a worker API.
mod fence;
mod worker;
pub(crate) use fence::namespace_checks;
use hermes_application::{Error, InstallationScope};
use serde_json::{Value, json};

pub const GROUP: &str = "hermes.agent-platform.io";
pub const VERSION: &str = "hermes.agent-platform.io/v1alpha1";
pub const CRD: &str = "instancepolicies.hermes.agent-platform.io";

fn string() -> Value {
    json!({"type":"string","minLength":1,"maxLength":2048})
}
pub fn definition() -> Value {
    let native = json!({"type":"object","required":["image","environment","cpu","memory"],"properties":{
        "image":string(),"environment":{"type":"object","additionalProperties":string(),"maxProperties":32},"cpu":string(),"memory":string()
    }});
    let spec = json!({"type":"object","required":["installationId","instanceId","namespaceUID","generation","native"],"properties":{
        "installationId":string(),"instanceId":string(),"namespaceUID":string(),"generation":{"type":"integer","format":"int64","minimum":1},"native":native,"worker":worker::schema()
    }});
    let schema = json!({"type":"object","required":["spec"],"properties":{
        "apiVersion":{"type":"string"},"kind":{"type":"string"},"metadata":{"type":"object"},"spec":spec
    }});
    json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition","metadata":{"name":CRD},"spec":{
        "group":GROUP,"scope":"Namespaced","names":{"plural":"instancepolicies","singular":"instancepolicy","kind":"InstancePolicy","listKind":"InstancePolicyList"},
        "conversion":{"strategy":"None"},
        "versions":[{"name":"v1alpha1","served":true,"storage":true,"schema":{"openAPIV3Schema":schema}}]
    }})
}

pub fn manifests(scope: &InstallationScope, controller_subject: &str) -> Vec<Value> {
    let mut result = vec![definition()];
    for (resource, group, pod, labels) in [
        ("pods", "", "object.spec", "object.metadata.labels"),
        (
            "deployments",
            "apps",
            "object.spec.template.spec",
            "object.spec.template.metadata.labels",
        ),
    ] {
        let name = format!("hb-{}-{resource}", scope.id.simple());
        let mut checks = checks(&scope.id.to_string());
        checks.push(fence::namespace_enabled());
        if resource == "deployments" {
            checks.push("object.metadata.name == 'hermes' && object.spec.replicas == 1 && object.spec.strategy.type == 'Recreate'".into());
        } else {
            checks.extend(fence::pod_start_checks(controller_subject));
        }
        let mut variables = vec![
            json!({"name":"pod","expression":pod}),
            json!({"name":"labels","expression":labels}),
        ];
        if resource == "pods" {
            checks = checks
                .into_iter()
                .map(|check| {
                    format!(
                        "variables.labels['app.kubernetes.io/component'] != 'hermes' || ({check})"
                    )
                })
                .collect();
            checks.extend(worker::checks(&scope.id.to_string(), controller_subject).into_iter().map(|check| format!("variables.labels['app.kubernetes.io/component'] != 'worker' || ({check})")));
            checks.push(
                "variables.labels['app.kubernetes.io/component'] in ['hermes','worker']".into(),
            );
            checks.push("request.operation != 'UPDATE' || oldObject.metadata.labels['app.kubernetes.io/component'] == variables.labels['app.kubernetes.io/component']".into());
            variables.push(worker::runtime_variable());
        }
        let mut policy = json!({"apiVersion":"admissionregistration.k8s.io/v1","kind":"ValidatingAdmissionPolicy","metadata":{"name":name},"spec":{
            "failurePolicy":"Fail","paramKind":{"apiVersion":VERSION,"kind":"InstancePolicy"},
            "matchConstraints":{"matchPolicy":"Equivalent","namespaceSelector":{"matchLabels":{"agent-platform.io/installation":scope.id.to_string()}},"objectSelector":{},"resourceRules":[
                {"apiGroups":[group],"apiVersions":["v1"],"operations":["CREATE","UPDATE"],"resources":[resource],"scope":"Namespaced"}
            ]},
            "variables":variables,
            "validations":checks.into_iter().map(|expression|json!({"expression":expression,"reason":"Forbidden","message":"Workload differs from approved personal instance policy"})).collect::<Vec<_>>()
        }});
        if resource == "deployments" {
            // Foreground GC removes finalizers after scale-to-zero. That is
            // metadata cleanup, not permission to run an old/disabled spec.
            // Evaluate before parameter lookup so missing policy cannot strand
            // a terminating workload. Every executable field stays identical.
            let mut cleanup = vec!["request.operation == 'UPDATE'".to_string(),
                "has(oldObject.metadata.deletionTimestamp) && has(object.metadata.deletionTimestamp) && object.metadata.deletionTimestamp == oldObject.metadata.deletionTimestamp".into(),
                "object.metadata.uid == oldObject.metadata.uid && object.spec == oldObject.spec".into(),
                "!has(object.metadata.finalizers) || (has(oldObject.metadata.finalizers) && object.metadata.finalizers.all(f, f in oldObject.metadata.finalizers))".into()];
            for field in ["labels", "annotations", "ownerReferences"] {
                cleanup.push(format!("has(object.metadata.{field}) == has(oldObject.metadata.{field}) && (!has(object.metadata.{field}) || object.metadata.{field} == oldObject.metadata.{field})"));
            }
            policy["spec"]["matchConditions"] = json!([{"name":"validate-workload-changes","expression":format!("request.operation != 'UPDATE' ? true : !({})",cleanup.iter().map(|c|format!("({c})")).collect::<Vec<_>>().join(" && "))}]);
        }
        result.extend([
            policy,
            json!({"apiVersion":"admissionregistration.k8s.io/v1","kind":"ValidatingAdmissionPolicyBinding","metadata":{"name":name},"spec":{"policyName":name,"paramRef":{"name":"instance-policy","parameterNotFoundAction":"Deny"},"validationActions":["Deny"]}}),
        ]);
    }
    let name = format!("hb-{}-scale-stop", scope.id.simple());
    let labels = "namespaceObject.metadata.labels";
    let owned = format!(
        "namespaceObject != null && has(namespaceObject.metadata.labels) && {labels}['agent-platform.io/installation'] == {} && request.namespace == {} + {labels}['agent-platform.io/instance'].replace('-','')",
        serde_json::to_string(&scope.id.to_string()).unwrap(),
        serde_json::to_string(&format!("hi-{}-", &scope.id.simple().to_string()[..24])).unwrap()
    );
    let closed = format!(
        "({owned}) && ({}) && {labels}['agent-platform.io/admission'] == 'closed'",
        fence::valid(labels)
    );
    // ScaleSpec.replicas is an optional non-pointer integer with default zero.
    // Kubernetes omits its zero value from CEL's object; an absent field here
    // is the typed Scale API's zero, not the unmerged caller's patch payload.
    let zero = "!has(object.spec) || !has(object.spec.replicas) || object.spec.replicas == 0";
    result.extend([
        json!({"apiVersion":"admissionregistration.k8s.io/v1","kind":"ValidatingAdmissionPolicy","metadata":{"name":name},"spec":{
            "failurePolicy":"Fail","matchConstraints":{"matchPolicy":"Equivalent","namespaceSelector":{"matchLabels":{"agent-platform.io/installation":scope.id.to_string()}},"objectSelector":{},"resourceRules":[{"apiGroups":["apps"],"apiVersions":["v1"],"operations":["UPDATE"],"resources":["deployments/scale"],"scope":"Namespaced"}]},
            "validations":[{"expression":format!("request.userInfo.username == {} && object.metadata.name == 'hermes' && request.name == 'hermes' && ({zero}) && ({closed})",serde_json::to_string(controller_subject).unwrap()),"reason":"Forbidden","message":"Only closed owned Hermes instances may be scaled to zero"}]
        }}),
        json!({"apiVersion":"admissionregistration.k8s.io/v1","kind":"ValidatingAdmissionPolicyBinding","metadata":{"name":name},"spec":{"policyName":name,"validationActions":["Deny"]}}),
    ]);
    result
}

/// Merge-patch ONLY the worker policy subtree of the owner-bound InstancePolicy.
/// The trusted driver must preserve native parameters and perform UID/RV CAS.
pub fn worker_parameters(
    policy: &hermes_application::execution::ResolvedPolicy,
) -> Result<Value, Error> {
    worker::parameters(policy)
}

fn checks(installation: &str) -> Vec<String> {
    // CEL's namespaceObject does not expose metadata.uid. The authenticated
    // Kubernetes driver verifies the live namespace UID before every child
    // action; admission preserves that binding between policy and Pod labels.
    let mut checks=vec![
        format!("params != null && namespaceObject != null && params.metadata.namespace == request.namespace && params.spec.installationId == {} && params.spec.namespaceUID == variables.labels['agent-platform.io/namespace-uid'] && params.spec.instanceId == namespaceObject.metadata.labels['agent-platform.io/instance']",serde_json::to_string(installation).unwrap()),
        "variables.labels['agent-platform.io/installation'] == params.spec.installationId && variables.labels['agent-platform.io/instance'] == params.spec.instanceId && variables.labels['app.kubernetes.io/component'] == 'hermes'".into(),
        "variables.labels['agent-platform.io/policy-generation'] == string(params.spec.generation)".into(),
        "variables.pod.serviceAccountName == 'hermes' && has(variables.pod.automountServiceAccountToken) && variables.pod.automountServiceAccountToken == false && variables.pod.enableServiceLinks == false".into(),
        "!has(variables.pod.imagePullSecrets) || variables.pod.imagePullSecrets.size() == 0".into(),
        "(!has(variables.pod.hostNetwork) || !variables.pod.hostNetwork) && (!has(variables.pod.hostPID) || !variables.pod.hostPID) && (!has(variables.pod.hostIPC) || !variables.pod.hostIPC) && (!has(variables.pod.shareProcessNamespace) || !variables.pod.shareProcessNamespace) && (!has(variables.pod.ephemeralContainers) || variables.pod.ephemeralContainers.size() == 0)".into(),
        "has(variables.pod.securityContext) && variables.pod.securityContext.runAsNonRoot && variables.pod.securityContext.runAsUser == 1000 && variables.pod.securityContext.runAsGroup == 1000 && variables.pod.securityContext.fsGroup == 1000 && variables.pod.securityContext.seccompProfile.type == 'RuntimeDefault'".into(),
        "variables.pod.containers.size() == 1 && variables.pod.initContainers.size() == 1".into(),
        "variables.pod.containers[0].name == 'hermes' && variables.pod.initContainers[0].name == 'native-home'".into(),
        "variables.pod.containers[0].command == ['/opt/hermes/.venv/bin/python','/usr/local/lib/personal-run.py'] && (!has(variables.pod.containers[0].args) || variables.pod.containers[0].args.size() == 0)".into(),
        "variables.pod.initContainers[0].command == ['/opt/hermes/.venv/bin/python','/usr/local/lib/personal-init.py'] && variables.pod.initContainers[0].args == ['--context','/run/hermes/owner/context.json']".into(),
        "has(variables.pod.containers[0].env) && variables.pod.containers[0].env.size() == params.spec.native.environment.size() && variables.pod.containers[0].env.all(e, e.name in params.spec.native.environment && has(e.value) && e.value == params.spec.native.environment[e.name] && !has(e.valueFrom))".into(),
        "variables.pod.containers[0].env.all(e, variables.pod.containers[0].env.filter(other, other.name == e.name).size() == 1)".into(),
        "!has(variables.pod.initContainers[0].env) || variables.pod.initContainers[0].env.size() == 0".into(),
        "(!has(variables.pod.resourceClaims) || variables.pod.resourceClaims.size() == 0) && (!has(variables.pod.securityContext.supplementalGroups) || variables.pod.securityContext.supplementalGroups.size() == 0) && (!has(variables.pod.securityContext.sysctls) || variables.pod.securityContext.sysctls.size() == 0)".into(),
        "!has(variables.pod.resources)".into(),
        "(!has(variables.pod.priorityClassName) || variables.pod.priorityClassName == '') && (!has(variables.pod.priority) || variables.pod.priority == 0) && (!has(variables.pod.preemptionPolicy) || variables.pod.preemptionPolicy == 'PreemptLowerPriority') && !has(variables.pod.runtimeClassName)".into(),
        "!has(variables.pod.securityContext.seLinuxOptions) && !has(variables.pod.securityContext.windowsOptions)".into(),
        "variables.pod.containers[0].ports.size() == 1 && variables.pod.containers[0].ports[0].name == 'dashboard' && variables.pod.containers[0].ports[0].containerPort == 9119 && variables.pod.containers[0].ports[0].protocol == 'TCP'".into(),
        "!has(variables.pod.containers[0].livenessProbe) && !has(variables.pod.containers[0].restartPolicy) && !has(variables.pod.initContainers[0].restartPolicy) && !has(variables.pod.initContainers[0].startupProbe) && !has(variables.pod.initContainers[0].readinessProbe) && !has(variables.pod.initContainers[0].livenessProbe)".into(),
        "variables.pod.initContainers[0].terminationMessagePath == '/tmp/native-init-result.json' && variables.pod.initContainers[0].terminationMessagePolicy == 'File'".into(),
        "variables.pod.containers[0].terminationMessagePath == '/dev/termination-log' && variables.pod.containers[0].terminationMessagePolicy == 'File'".into(),
        "variables.pod.volumes.size() == 7".into(),
    ];
    for (probe, failures) in [("startupProbe", 60), ("readinessProbe", 3)] {
        checks.push(format!("has(variables.pod.containers[0].{probe}) && has(variables.pod.containers[0].{probe}.tcpSocket) && variables.pod.containers[0].{probe}.tcpSocket.port == 'dashboard' && (!has(variables.pod.containers[0].{probe}.tcpSocket.host) || variables.pod.containers[0].{probe}.tcpSocket.host == '') && variables.pod.containers[0].{probe}.periodSeconds == 5 && variables.pod.containers[0].{probe}.failureThreshold == {failures}"));
    }
    for container in [
        "variables.pod.containers[0]",
        "variables.pod.initContainers[0]",
    ] {
        checks.push(format!("{container}.image == params.spec.native.image && has({container}.securityContext) && {container}.securityContext.allowPrivilegeEscalation == false && {container}.securityContext.readOnlyRootFilesystem && (!has({container}.securityContext.privileged) || !{container}.securityContext.privileged) && {container}.securityContext.capabilities.drop == ['ALL'] && (!has({container}.securityContext.capabilities.add) || {container}.securityContext.capabilities.add.size() == 0)"));
        checks.push(format!("(!has({container}.securityContext.runAsUser) || {container}.securityContext.runAsUser == 1000) && (!has({container}.securityContext.runAsGroup) || {container}.securityContext.runAsGroup == 1000) && (!has({container}.securityContext.procMount) || {container}.securityContext.procMount == 'Default') && (!has({container}.securityContext.seccompProfile) || {container}.securityContext.seccompProfile.type == 'RuntimeDefault')"));
        checks.push(format!("(!has({container}.envFrom) || {container}.envFrom.size() == 0) && !has({container}.lifecycle) && (!has({container}.volumeDevices) || {container}.volumeDevices.size() == 0) && (!has({container}.ports) || {container}.ports.all(p, !has(p.hostPort) || p.hostPort == 0))"));
        checks.push(format!("!has({container}.securityContext.seLinuxOptions) && !has({container}.securityContext.windowsOptions) && (!has({container}.resources.claims) || {container}.resources.claims.size() == 0) && {container}.resources.requests.size() == 2 && {container}.resources.limits.size() == 2"));
        // Quantity comparison accepts Kubernetes' canonicalization (1000m -> 1,
        // 1024Mi -> 1Gi) while keeping the approved resource ceiling exact.
        checks.push(format!("quantity({container}.resources.requests['cpu']) == quantity(params.spec.native.cpu) && quantity({container}.resources.limits['cpu']) == quantity(params.spec.native.cpu) && quantity({container}.resources.requests['memory']) == quantity(params.spec.native.memory) && quantity({container}.resources.limits['memory']) == quantity(params.spec.native.memory)"));
    }
    for (name, field, reference) in [
        ("home", "persistentVolumeClaim", "home"),
        ("workspace", "persistentVolumeClaim", "workspace"),
        ("owner", "configMap", "native-owner"),
        ("trust", "configMap", "native-trust"),
    ] {
        let target = if field == "persistentVolumeClaim" {
            "claimName"
        } else {
            "name"
        };
        checks.push(format!("variables.pod.volumes.filter(v, v.name == '{name}').size() == 1 && variables.pod.volumes.exists(v, v.name == '{name}' && has(v.{field}) && v.{field}.{target} == '{reference}')"));
    }
    checks.push("variables.pod.volumes.exists(v,v.name == 'tmp' && has(v.emptyDir) && v.emptyDir.sizeLimit == '256Mi')".into());
    for (name, files) in [
        ("init", vec!["init-key", "native-config.json"]),
        (
            "credentials",
            vec![
                "dashboard-session-token",
                "execution-client-token",
                "id_ed25519",
            ],
        ),
    ] {
        let file_tests = files
            .iter()
            .map(|key| format!("v.secret.items.exists(i, i.key == '{key}' && i.path == '{key}')"))
            .collect::<Vec<_>>()
            .join(" && ");
        checks.push(format!("variables.pod.volumes.exists(v,v.name == '{name}' && has(v.secret) && v.secret.secretName == 'native-bootstrap' && v.secret.defaultMode == 288 && v.secret.items.size() == {} && {file_tests})",files.len()));
    }
    for (container, mounts) in [
        (
            "variables.pod.containers[0]",
            vec![
                ("home", "/opt/data", false),
                ("workspace", "/workspace", false),
                ("tmp", "/tmp", false),
                ("owner", "/run/hermes/owner", true),
                ("trust", "/run/hermes/execution-ca", true),
                ("credentials", "/run/hermes/credentials", true),
            ],
        ),
        (
            "variables.pod.initContainers[0]",
            vec![
                ("home", "/opt/data", false),
                ("tmp", "/tmp", false),
                ("owner", "/run/hermes/owner", true),
                ("init", "/run/hermes/init", true),
            ],
        ),
    ] {
        checks.push(format!("{container}.volumeMounts.size() == {} && {container}.volumeMounts.all(m, !has(m.subPath) && !has(m.subPathExpr) && (!has(m.mountPropagation) || m.mountPropagation == 'None'))",mounts.len()));
        for (name, path, readonly) in mounts {
            let mode = if readonly {
                "has(m.readOnly) && m.readOnly"
            } else {
                "(!has(m.readOnly) || !m.readOnly)"
            };
            checks.push(format!("{container}.volumeMounts.exists(m,m.name == '{name}' && m.mountPath == '{path}' && {mode})"));
        }
    }
    checks
}

pub fn parameters(
    scope: &InstallationScope,
    instance: uuid::Uuid,
    namespace_uid: &str,
    generation: i64,
    pod: &Value,
) -> Result<Value, Error> {
    if scope.id.is_nil()
        || instance.is_nil()
        || generation < 1
        || !hermes_domain::resources::valid_uid(namespace_uid)
    {
        return Err(Error::Invalid);
    }
    let environment: std::collections::BTreeMap<_, _> = pod["containers"][0]["env"]
        .as_array()
        .ok_or(Error::Invalid)?
        .iter()
        .map(|item| {
            Ok((
                item["name"].as_str().ok_or(Error::Invalid)?.to_string(),
                item["value"].as_str().ok_or(Error::Invalid)?.to_string(),
            ))
        })
        .collect::<Result<_, Error>>()?;
    Ok(
        json!({"spec":{"installationId":scope.id,"instanceId":instance,"namespaceUID":namespace_uid,"generation":generation,"native":{"image":pod["containers"][0]["image"],"environment":environment,"cpu":pod["containers"][0]["resources"]["requests"]["cpu"],"memory":pod["containers"][0]["resources"]["requests"]["memory"]}}}),
    )
}
