//! Worker policy parameters and admission share the resolved worker renderer.
use super::{fence, string};
use crate::execution_workloads::{self, START_GATE, TOLERATION_SECONDS};
use hermes_application::{Error, execution::ResolvedPolicy};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn schema() -> Value {
    let integer = json!({"type":"integer","format":"int64","minimum":1});
    let volume = json!({"type":"object","required":["name","kind"],"properties":{
        "name":string(),"kind":{"type":"string","enum":["empty","pvc","ssh"]},"reference":string(),"sizeLimit":string(),"readOnly":{"type":"boolean"}
    }});
    let mount = json!({"type":"object","required":["name","path","readOnly"],"properties":{
        "name":string(),"path":string(),"readOnly":{"type":"boolean"},"subPath":string()
    }});
    let runtime = json!({"type":"object","required":["version","digest","image","uid","gid","cpu","memory","ephemeralStorage","environment","volumes","mounts","pullSecrets"],"properties":{
        "version":integer,"digest":string(),"image":string(),"uid":integer,"gid":integer,
        "cpu":string(),"memory":string(),"ephemeralStorage":string(),
        "environment":{"type":"object","additionalProperties":string(),"maxProperties":32},
        "volumes":{"type":"array","maxItems":20,"items":volume},
        "mounts":{"type":"array","maxItems":20,"items":mount},
        "pullSecrets":{"type":"array","maxItems":4,"items":string()}
    }});
    json!({"type":"object","required":["generation","digest","runtimes"],"properties":{
        "generation":integer,"digest":string(),"runtimes":{"type":"object","maxProperties":16,"additionalProperties":runtime}
    }})
}
pub(super) fn parameters(policy: &ResolvedPolicy) -> Result<Value, Error> {
    execution_workloads::validate_policy(policy)?;
    let runtimes: BTreeMap<_, _> = policy
        .runtimes
        .iter()
        .map(|r| {
            (
                r.definition.runtime.id.as_str(),
                execution_workloads::template(r),
            )
        })
        .collect();
    let patch = json!({"spec":{"worker":{"generation":policy.generation,"digest":policy.digest()?,"runtimes":runtimes}}});
    if serde_json::to_vec(&patch)
        .map_err(|_| Error::Invalid)?
        .len()
        > 65_536
    {
        return Err(Error::Invalid);
    }
    Ok(patch)
}
pub(super) fn runtime_variable() -> Value {
    json!({"name":"runtime","expression":"has(params.spec.worker) && 'hermes.agent-platform.io/runtime-id' in variables.labels && variables.labels['hermes.agent-platform.io/runtime-id'] in params.spec.worker.runtimes ? params.spec.worker.runtimes[variables.labels['hermes.agent-platform.io/runtime-id']] : null"})
}
pub(super) fn checks(installation: &str, subject: &str) -> Vec<String> {
    let subject = serde_json::to_string(subject).expect("string serialization cannot fail");
    let installation =
        serde_json::to_string(installation).expect("string serialization cannot fail");
    let mut result = vec![
        format!("params != null && namespaceObject != null && has(params.spec.worker) && variables.runtime != null && params.metadata.namespace == request.namespace && params.spec.installationId == {installation} && params.spec.instanceId == namespaceObject.metadata.labels['agent-platform.io/instance'] && variables.labels['agent-platform.io/installation'] == params.spec.installationId && variables.labels['agent-platform.io/instance'] == params.spec.instanceId"),
        fence::namespace_enabled(),
        format!("request.userInfo.username == {subject} && (!has(object.metadata.ownerReferences) || object.metadata.ownerReferences.size() == 0)"),
        "variables.labels['hermes.agent-platform.io/namespace-uid'] == params.spec.namespaceUID && variables.labels['hermes.agent-platform.io/assignment-generation'] == string(params.spec.generation) && variables.labels['hermes.agent-platform.io/execution-policy-generation'] == string(params.spec.worker.generation)".into(),
        "has(object.metadata.annotations) && object.metadata.annotations['hermes.agent-platform.io/policy-digest'] == params.spec.worker.digest && object.metadata.annotations['hermes.agent-platform.io/runtime-digest'] == variables.runtime.digest && variables.labels['hermes.agent-platform.io/runtime-version'] == string(variables.runtime.version)".into(),
        "variables.labels['hermes.agent-platform.io/environment-id'].matches('^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$') && variables.labels['hermes.agent-platform.io/environment-id'] != '00000000-0000-0000-0000-000000000000' && variables.labels['hermes.agent-platform.io/environment-generation'].matches('^[1-9][0-9]{0,18}$') && int(variables.labels['hermes.agent-platform.io/environment-generation']) > 0 && object.metadata.name == 'hw-' + variables.labels['hermes.agent-platform.io/environment-id'].replace('-','') + '-g' + variables.labels['hermes.agent-platform.io/environment-generation']".into(),
        "variables.pod.serviceAccountName == 'worker' && (!has(variables.pod.serviceAccount) || variables.pod.serviceAccount == 'worker') && has(variables.pod.automountServiceAccountToken) && !variables.pod.automountServiceAccountToken && has(variables.pod.enableServiceLinks) && !variables.pod.enableServiceLinks && variables.pod.restartPolicy == 'Never' && variables.pod.terminationGracePeriodSeconds == 30".into(),
        "variables.pod.containers.size() == 1 && variables.pod.containers[0].name == 'execution' && (!has(variables.pod.initContainers) || variables.pod.initContainers.size() == 0) && (!has(variables.pod.ephemeralContainers) || variables.pod.ephemeralContainers.size() == 0)".into(),
        "variables.pod.containers[0].image == variables.runtime.image && variables.pod.containers[0].imagePullPolicy == 'IfNotPresent' && variables.pod.containers[0].command == ['/entrypoint.sh'] && (!has(variables.pod.containers[0].args) || variables.pod.containers[0].args.size() == 0) && variables.pod.containers[0].workingDir == '/workspace'".into(),
        "has(variables.pod.securityContext) && variables.pod.securityContext.runAsNonRoot && variables.pod.securityContext.runAsUser == variables.runtime.uid && variables.pod.securityContext.runAsGroup == variables.runtime.gid && variables.pod.securityContext.fsGroup == variables.runtime.gid && variables.pod.securityContext.seccompProfile.type == 'RuntimeDefault'".into(),
        "(!has(variables.pod.securityContext.supplementalGroups) || variables.pod.securityContext.supplementalGroups.size() == 0) && (!has(variables.pod.securityContext.sysctls) || variables.pod.securityContext.sysctls.size() == 0) && !has(variables.pod.securityContext.seLinuxOptions) && !has(variables.pod.securityContext.windowsOptions) && (!has(variables.pod.securityContext.appArmorProfile) || variables.pod.securityContext.appArmorProfile.type == 'RuntimeDefault')".into(),
        "(!has(variables.pod.hostNetwork) || !variables.pod.hostNetwork) && (!has(variables.pod.hostPID) || !variables.pod.hostPID) && (!has(variables.pod.hostIPC) || !variables.pod.hostIPC) && (!has(variables.pod.shareProcessNamespace) || !variables.pod.shareProcessNamespace) && (!has(variables.pod.hostUsers) || variables.pod.hostUsers) && (!has(variables.pod.os) || variables.pod.os.name == 'linux')".into(),
        "!has(variables.pod.resources) && (!has(variables.pod.resourceClaims) || variables.pod.resourceClaims.size() == 0) && (!has(variables.pod.priorityClassName) || variables.pod.priorityClassName == '') && (!has(variables.pod.priority) || variables.pod.priority == 0) && (!has(variables.pod.preemptionPolicy) || variables.pod.preemptionPolicy == 'PreemptLowerPriority') && !has(variables.pod.runtimeClassName) && !has(variables.pod.schedulingGroup)".into(),
        "variables.pod.dnsPolicy == 'ClusterFirst' && !has(variables.pod.dnsConfig) && variables.pod.schedulerName == 'default-scheduler' && (!has(variables.pod.nodeSelector) || variables.pod.nodeSelector.size() == 0) && !has(variables.pod.affinity) && (!has(variables.pod.hostAliases) || variables.pod.hostAliases.size() == 0) && (!has(variables.pod.topologySpreadConstraints) || variables.pod.topologySpreadConstraints.size() == 0) && (!has(variables.pod.readinessGates) || variables.pod.readinessGates.size() == 0)".into(),
        "(!has(variables.pod.hostname) || variables.pod.hostname == '') && (!has(variables.pod.hostnameOverride) || variables.pod.hostnameOverride == '') && (!has(variables.pod.subdomain) || variables.pod.subdomain == '') && (!has(variables.pod.setHostnameAsFQDN) || !variables.pod.setHostnameAsFQDN)".into(),
        format!("variables.pod.tolerations.size() == 2 && variables.pod.tolerations.all(t, t.key in ['node.kubernetes.io/not-ready','node.kubernetes.io/unreachable'] && variables.pod.tolerations.filter(other,other.key == t.key).size() == 1 && t.operator == 'Exists' && t.effect == 'NoExecute' && t.tolerationSeconds == {TOLERATION_SECONDS} && (!has(t.value) || t.value == ''))"),
        "(!has(variables.pod.imagePullSecrets) ? 0 : variables.pod.imagePullSecrets.size()) == variables.runtime.pullSecrets.size() && (!has(variables.pod.imagePullSecrets) || variables.pod.imagePullSecrets.all(p,p.name in variables.runtime.pullSecrets && variables.pod.imagePullSecrets.filter(other,other.name == p.name).size() == 1))".into(),
        "variables.pod.containers[0].env.size() == variables.runtime.environment.size() && variables.pod.containers[0].env.all(e,e.name in variables.runtime.environment && has(e.value) && e.value == variables.runtime.environment[e.name] && !has(e.valueFrom) && variables.pod.containers[0].env.filter(other,other.name == e.name).size() == 1) && (!has(variables.pod.containers[0].envFrom) || variables.pod.containers[0].envFrom.size() == 0)".into(),
        "has(variables.pod.containers[0].securityContext) && !variables.pod.containers[0].securityContext.allowPrivilegeEscalation && variables.pod.containers[0].securityContext.readOnlyRootFilesystem && variables.pod.containers[0].securityContext.capabilities.drop == ['ALL'] && (!has(variables.pod.containers[0].securityContext.capabilities.add) || variables.pod.containers[0].securityContext.capabilities.add.size() == 0) && (!has(variables.pod.containers[0].securityContext.privileged) || !variables.pod.containers[0].securityContext.privileged)".into(),
        "(!has(variables.pod.containers[0].securityContext.runAsUser) || variables.pod.containers[0].securityContext.runAsUser == variables.runtime.uid) && (!has(variables.pod.containers[0].securityContext.runAsGroup) || variables.pod.containers[0].securityContext.runAsGroup == variables.runtime.gid) && (!has(variables.pod.containers[0].securityContext.runAsNonRoot) || variables.pod.containers[0].securityContext.runAsNonRoot) && (!has(variables.pod.containers[0].securityContext.procMount) || variables.pod.containers[0].securityContext.procMount == 'Default') && (!has(variables.pod.containers[0].securityContext.seccompProfile) || variables.pod.containers[0].securityContext.seccompProfile.type == 'RuntimeDefault') && (!has(variables.pod.containers[0].securityContext.appArmorProfile) || variables.pod.containers[0].securityContext.appArmorProfile.type == 'RuntimeDefault') && !has(variables.pod.containers[0].securityContext.seLinuxOptions) && !has(variables.pod.containers[0].securityContext.windowsOptions)".into(),
        "variables.pod.containers[0].ports.size() == 1 && variables.pod.containers[0].ports[0].name == 'ssh' && variables.pod.containers[0].ports[0].containerPort == 2222 && variables.pod.containers[0].ports[0].protocol == 'TCP' && (!has(variables.pod.containers[0].ports[0].hostPort) || variables.pod.containers[0].ports[0].hostPort == 0) && (!has(variables.pod.containers[0].ports[0].hostIP) || variables.pod.containers[0].ports[0].hostIP == '')".into(),
        "!has(variables.pod.containers[0].lifecycle) && !has(variables.pod.containers[0].livenessProbe) && !has(variables.pod.containers[0].startupProbe) && !has(variables.pod.containers[0].restartPolicy) && (!has(variables.pod.containers[0].volumeDevices) || variables.pod.containers[0].volumeDevices.size() == 0) && (!has(variables.pod.containers[0].resizePolicy) || variables.pod.containers[0].resizePolicy.size() == 0) && (!has(variables.pod.containers[0].stdin) || !variables.pod.containers[0].stdin) && (!has(variables.pod.containers[0].stdinOnce) || !variables.pod.containers[0].stdinOnce) && (!has(variables.pod.containers[0].tty) || !variables.pod.containers[0].tty)".into(),
        "variables.pod.containers[0].terminationMessagePath == '/dev/termination-log' && variables.pod.containers[0].terminationMessagePolicy == 'File'".into(),
        "has(variables.pod.containers[0].readinessProbe) && has(variables.pod.containers[0].readinessProbe.tcpSocket) && variables.pod.containers[0].readinessProbe.tcpSocket.port == 'ssh' && (!has(variables.pod.containers[0].readinessProbe.tcpSocket.host) || variables.pod.containers[0].readinessProbe.tcpSocket.host == '') && variables.pod.containers[0].readinessProbe.periodSeconds == 2 && variables.pod.containers[0].readinessProbe.timeoutSeconds == 1 && variables.pod.containers[0].readinessProbe.successThreshold == 1 && variables.pod.containers[0].readinessProbe.failureThreshold == 3 && (!has(variables.pod.containers[0].readinessProbe.initialDelaySeconds) || variables.pod.containers[0].readinessProbe.initialDelaySeconds == 0) && !has(variables.pod.containers[0].readinessProbe.terminationGracePeriodSeconds)".into(),
        "variables.pod.containers[0].resources.requests.size() == 3 && variables.pod.containers[0].resources.limits.size() == 3 && (!has(variables.pod.containers[0].resources.claims) || variables.pod.containers[0].resources.claims.size() == 0)".into(),
    ];
    for (resource, expected) in [
        ("cpu", "cpu"),
        ("memory", "memory"),
        ("ephemeral-storage", "ephemeralStorage"),
    ] {
        result.push(format!("quantity(variables.pod.containers[0].resources.requests['{resource}']) == quantity(variables.runtime.{expected}) && quantity(variables.pod.containers[0].resources.limits['{resource}']) == quantity(variables.runtime.{expected})"));
    }
    result.extend(volume_checks());
    let singleton = format!(
        "has(variables.pod.schedulingGates) && variables.pod.schedulingGates.size() == 1 && variables.pod.schedulingGates[0].name == '{START_GATE}'"
    );
    let old = format!(
        "has(oldObject.spec.schedulingGates) && oldObject.spec.schedulingGates.size() == 1 && oldObject.spec.schedulingGates[0].name == '{START_GATE}'"
    );
    let none = "!has(variables.pod.schedulingGates) || variables.pod.schedulingGates.size() == 0";
    result.extend([
        format!("request.operation != 'CREATE' || (({singleton}) && ({}) && (!has(variables.pod.nodeName) || variables.pod.nodeName == ''))", fence::namespace_open()),
        format!("request.operation != 'UPDATE' || ((!has(oldObject.spec.schedulingGates) || oldObject.spec.schedulingGates.size() == 0) ? ({none}) : (({old}) && (({singleton}) || (({none}) && ({})))))", fence::namespace_open()),
        format!("request.operation != 'UPDATE' || ({})", fence::unchanged_spec()),
    ]);
    result
}

fn volume_checks() -> Vec<String> {
    vec![
        "variables.pod.volumes.size() == variables.runtime.volumes.size() && variables.pod.volumes.all(v,variables.pod.volumes.filter(other,other.name == v.name).size() == 1 && variables.runtime.volumes.exists(p,p.name == v.name && ((p.kind == 'pvc' && has(v.persistentVolumeClaim) && v.persistentVolumeClaim.claimName == p.reference && ((!has(v.persistentVolumeClaim.readOnly) || !v.persistentVolumeClaim.readOnly) == !p.readOnly)) || (p.kind == 'empty' && has(v.emptyDir) && quantity(v.emptyDir.sizeLimit) == quantity(p.sizeLimit) && (!has(v.emptyDir.medium) || v.emptyDir.medium == '')) || (p.kind == 'ssh' && has(v.secret) && v.secret.secretName == p.reference && v.secret.defaultMode == 292 && (!has(v.secret.optional) || !v.secret.optional) && v.secret.items.size() == 1 && v.secret.items[0].key == 'worker' && v.secret.items[0].path == 'worker' && (!has(v.secret.items[0].mode) || v.secret.items[0].mode == 292)))))".into(),
        "variables.pod.containers[0].volumeMounts.size() == variables.runtime.mounts.size() && variables.pod.containers[0].volumeMounts.all(m,variables.pod.containers[0].volumeMounts.filter(other,other.name == m.name).size() == 1 && variables.runtime.mounts.exists(p,p.name == m.name && p.path == m.mountPath && ((!has(m.readOnly) || !m.readOnly) == !p.readOnly) && ((!has(p.subPath) && (!has(m.subPath) || m.subPath == '')) || (has(p.subPath) && has(m.subPath) && p.subPath == m.subPath))) && !has(m.subPathExpr) && (!has(m.mountPropagation) || m.mountPropagation == 'None') && (!has(m.recursiveReadOnly) || m.recursiveReadOnly == 'Disabled'))".into(),
    ]
}
