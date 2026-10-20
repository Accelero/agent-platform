//! Namespace lifecycle and native Pod start rules. No parameter lookup fallback.
use hermes_application::{
    namespace_fence::{ACCESS, ADMISSION, EPOCH, GENERATION},
    native_children::START_GATE,
};

pub(super) fn none(labels: &str) -> String {
    [ACCESS, ADMISSION, GENERATION, EPOCH]
        .iter()
        .map(|key| format!("!('{key}' in {labels})"))
        .collect::<Vec<_>>()
        .join(" && ")
}
pub(super) fn valid(labels: &str) -> String {
    let present = [ACCESS, ADMISSION, GENERATION, EPOCH]
        .iter()
        .map(|key| format!("'{key}' in {labels}"))
        .collect::<Vec<_>>()
        .join(" && ");
    format!(
        "({present}) && {labels}['{ACCESS}'] in ['enabled','suspended','revoked'] && {labels}['{ADMISSION}'] in ['closed','open'] && {labels}['{GENERATION}'].matches('^[1-9][0-9]{{0,18}}$') && int({labels}['{GENERATION}']) > 0 && {labels}['{EPOCH}'].matches('^[1-9][0-9]{{0,18}}$') && int({labels}['{EPOCH}']) > 0 && ({labels}['{ACCESS}'] == 'enabled' || {labels}['{ADMISSION}'] == 'closed')"
    )
}
pub(crate) fn namespace_checks() -> Vec<String> {
    let new = "object.metadata.labels";
    let old = "oldObject.metadata.labels";
    let absent = none(new);
    let old_absent = none(old);
    let valid = valid(new);
    let old_valid = self::valid(old);
    vec![
        format!("request.operation != 'CREATE' || ({absent})"),
        format!(
            "request.operation != 'UPDATE' || (({old_absent}) ? (({absent}) || (({valid}) && {new}['{ADMISSION}'] == 'closed')) : (({old_valid}) && ({valid})))"
        ),
        format!(
            "request.operation != 'UPDATE' || ({old_absent}) || (({valid}) && int({new}['{EPOCH}']) >= int({old}['{EPOCH}']) && int({new}['{GENERATION}']) >= int({old}['{GENERATION}']) && ({new}['{GENERATION}'] != {old}['{GENERATION}'] || {new}['{ACCESS}'] == {old}['{ACCESS}']))"
        ),
        format!(
            "({absent}) ? true : ({new}['{ADMISSION}'] != 'open' || (request.operation == 'UPDATE' && ({old_valid}) && {new}['{ACCESS}'] == 'enabled' && {old}['{ACCESS}'] == 'enabled' && {new}['{EPOCH}'] == {old}['{EPOCH}'] && {new}['{GENERATION}'] == {old}['{GENERATION}']))"
        ),
    ]
}

pub(super) fn namespace_enabled() -> String {
    let labels = "namespaceObject.metadata.labels";
    format!(
        "namespaceObject != null && has(namespaceObject.metadata.labels) && ({}) && {labels}['{ACCESS}'] == 'enabled' && {labels}['{GENERATION}'] == string(params.spec.generation)",
        valid(labels)
    )
}
pub(super) fn namespace_open() -> String {
    format!(
        "{} && namespaceObject.metadata.labels['{ADMISSION}'] == 'open'",
        namespace_enabled()
    )
}

pub(super) fn pod_start_checks(subject: &str) -> Vec<String> {
    let caller = serde_json::to_string(subject).expect("string serialization cannot fail");
    let singleton = format!(
        "has(object.spec.schedulingGates) && object.spec.schedulingGates.size() == 1 && object.spec.schedulingGates[0].name == '{START_GATE}'"
    );
    let no_new = "!has(object.spec.schedulingGates) || object.spec.schedulingGates.size() == 0";
    let no_old =
        "!has(oldObject.spec.schedulingGates) || oldObject.spec.schedulingGates.size() == 0";
    let old_singleton = format!(
        "has(oldObject.spec.schedulingGates) && oldObject.spec.schedulingGates.size() == 1 && oldObject.spec.schedulingGates[0].name == '{START_GATE}'"
    );
    vec![
        format!(
            "request.operation == 'CREATE' ? (({singleton}) && ({})) : (({singleton}) || ({no_new}))",
            namespace_open()
        ),
        format!(
            "request.operation != 'UPDATE' || (({no_old}) ? ({no_new}) : (({old_singleton}) && (({singleton}) || (({no_new}) && request.userInfo.username == {caller} && ({})))))",
            namespace_open()
        ),
        format!("request.operation != 'UPDATE' || ({})", unchanged_spec()),
    ]
}

// Explicit pinned PodSpec schema. Never normalize arbitrary object keys or env
// values to create an admission exception. The trusted child driver also checks
// the complete PodSpec digest before/after removing the sole scheduling gate.
// CEL does not expose PodSpec.overhead in the pinned API schema. Kubernetes
// rejects updates to this immutable field; the actual API proof covers it.
// The native policy also forbids RuntimeClass, which supplies Pod overhead.
pub(super) fn unchanged_spec() -> String {
    [
        "activeDeadlineSeconds",
        "affinity",
        "automountServiceAccountToken",
        "containers",
        "dnsConfig",
        "dnsPolicy",
        "enableServiceLinks",
        "ephemeralContainers",
        "hostAliases",
        "hostIPC",
        "hostNetwork",
        "hostPID",
        "hostUsers",
        "hostname",
        "hostnameOverride",
        "imagePullSecrets",
        "initContainers",
        "nodeName",
        "nodeSelector",
        "os",
        "preemptionPolicy",
        "priority",
        "priorityClassName",
        "readinessGates",
        "resourceClaims",
        "resources",
        "restartPolicy",
        "runtimeClassName",
        "schedulerName",
        "schedulingGroup",
        "securityContext",
        "serviceAccount",
        "serviceAccountName",
        "setHostnameAsFQDN",
        "shareProcessNamespace",
        "subdomain",
        "terminationGracePeriodSeconds",
        "tolerations",
        "topologySpreadConstraints",
        "volumes",
    ]
    .iter()
    .map(|field| {
        format!("(has(object.spec.{field}) == has(oldObject.spec.{field}) && (!has(object.spec.{field}) || object.spec.{field} == oldObject.spec.{field}))")
    })
    .collect::<Vec<_>>()
    .join(" && ")
}
