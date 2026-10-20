//! Release-owned namespace/RBAC bootstrap policy. The controller cannot install
//! or relax these resources; installation is a separate trusted chart action.
use crate::workloads::WorkloadConfig;
use hermes_application::{Error, InstallationScope};
use kube::{
    Api, Client,
    api::PostParams,
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::timeout;

pub struct BootstrapPolicy {
    manifests: Vec<Value>,
    subject: String,
    control_namespace: String,
    reconciler_role: String,
    installation_id: uuid::Uuid,
}
fn cel(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}
fn object(kind: &str, name: &str, body: Value) -> Value {
    let api = if kind.starts_with("ValidatingAdmission") {
        "admissionregistration.k8s.io/v1"
    } else {
        "rbac.authorization.k8s.io/v1"
    };
    let mut result = body;
    result["apiVersion"] = json!(api);
    result["kind"] = json!(kind);
    result["metadata"] = json!({"name":name});
    result
}
impl BootstrapPolicy {
    pub fn new(scope: &InstallationScope, config: &WorkloadConfig) -> Result<Self, Error> {
        config.validate()?;
        hermes_domain::validate_issuer(&scope.issuer)?;
        if scope.id.is_nil()
            || scope.epoch < 1
            || config.reconciler_role != Self::reconciler_role(scope)
        {
            return Err(Error::Invalid);
        }
        let identifier = format!("hb-{}", scope.id.simple());
        let namespace_policy = format!("{identifier}-namespace");
        let binding_policy = format!("{identifier}-binding");
        let subject = format!(
            "system:serviceaccount:{}:{}",
            config.control_namespace, config.controller_service_account
        );
        let prefix = format!("hi-{}-", &scope.id.simple().to_string()[..24]);
        let labels = "object.metadata.labels";
        let namespace_expression = format!(
            "object.metadata.name.matches({}) && has(object.metadata.labels) && 'agent-platform.io/installation' in {labels} && {labels}['agent-platform.io/installation'] == {} && 'agent-platform.io/instance' in {labels} && {labels}['agent-platform.io/instance'].matches('^[0-9a-f]{{8}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{4}}-[0-9a-f]{{12}}$') && object.metadata.name == {} + {labels}['agent-platform.io/instance'].replace('-', '') && 'pod-security.kubernetes.io/enforce' in {labels} && {labels}['pod-security.kubernetes.io/enforce'] == 'restricted'",
            cel(&format!("^{prefix}[0-9a-f]{{32}}$")),
            cel(&scope.id.to_string()),
            cel(&prefix)
        );
        let immutable_labels = "request.operation == 'CREATE' || (oldObject.metadata.labels['agent-platform.io/installation'] == object.metadata.labels['agent-platform.io/installation'] && oldObject.metadata.labels['agent-platform.io/instance'] == object.metadata.labels['agent-platform.io/instance'])";
        let binding_namespace = format!(
            "namespaceObject != null && request.namespace.matches({}) && has(namespaceObject.metadata.labels) && 'agent-platform.io/installation' in namespaceObject.metadata.labels && namespaceObject.metadata.labels['agent-platform.io/installation'] == {} && 'agent-platform.io/instance' in namespaceObject.metadata.labels && request.namespace == {} + namespaceObject.metadata.labels['agent-platform.io/instance'].replace('-', '')",
            cel(&format!("^{prefix}[0-9a-f]{{32}}$")),
            cel(&scope.id.to_string()),
            cel(&prefix)
        );
        let binding_target = format!(
            "object.metadata.name == 'instance-reconciler' && object.roleRef.apiGroup == 'rbac.authorization.k8s.io' && object.roleRef.kind == 'ClusterRole' && object.roleRef.name == {} && has(object.subjects) && object.subjects.size() == 1 && object.subjects[0].kind == 'ServiceAccount' && object.subjects[0].name == {} && object.subjects[0].namespace == {}",
            cel(&config.reconciler_role),
            cel(&config.controller_service_account),
            cel(&config.control_namespace)
        );
        let mut manifests = vec![];
        for (name, group, resource, validations) in [
            (
                &namespace_policy,
                "",
                "namespaces",
                vec![namespace_expression, immutable_labels.into()],
            ),
            (
                &binding_policy,
                "rbac.authorization.k8s.io",
                "rolebindings",
                vec![binding_namespace, binding_target],
            ),
        ] {
            let validations:Vec<_> = validations.into_iter().map(|expression| json!({
                "expression":expression,
                "reason":"Forbidden",
                "message":"Personal instance bootstrap is outside approved ownership or RBAC scope"
            })).collect();
            manifests.push(object("ValidatingAdmissionPolicy",name,json!({
                "spec":{
                    "failurePolicy":"Fail",
                    "matchConstraints":{
                        "matchPolicy":"Equivalent","namespaceSelector":{},"objectSelector":{},
                        "resourceRules":[{"apiGroups":[group],"apiVersions":["v1"],
                            "operations":["CREATE","UPDATE"],"resources":[resource],"scope":"*"}]
                    },
                    "matchConditions":[{"name":"controller-identity",
                        "expression":format!("request.userInfo.username == {}",cel(&subject))}],
                    "validations":validations
                }
            })));
            manifests.push(object(
                "ValidatingAdmissionPolicyBinding",
                name,
                json!({"spec":{"policyName":name,"validationActions":["Deny"]}}),
            ));
        }
        manifests.push(object("ClusterRole",&config.reconciler_role,json!({"rules":[
            {"apiGroups":[""],"resources":["pods","services","persistentvolumeclaims","secrets","configmaps","serviceaccounts"],"verbs":["get","list","watch","create","patch","update","delete"]},
            {"apiGroups":["apps"],"resources":["deployments"],"verbs":["get","list","watch","create","patch","update","delete"]},
            {"apiGroups":["apps"],"resources":["replicasets"],"verbs":["get","list","watch"]},
            {"apiGroups":["networking.k8s.io"],"resources":["networkpolicies"],"verbs":["get","list","watch","create","patch","update","delete"]}
        ]})));
        manifests.push(object("ClusterRole",&identifier,json!({"rules":[
            {"apiGroups":[""],"resources":["namespaces"],"verbs":["get","list","watch","create","patch","update"]},
            {"apiGroups":["rbac.authorization.k8s.io"],"resources":["rolebindings"],"verbs":["get","list","watch","create","patch","update"]},
            {"apiGroups":["rbac.authorization.k8s.io"],"resources":["clusterroles"],"resourceNames":[config.reconciler_role],"verbs":["get","bind"]},
            {"apiGroups":["rbac.authorization.k8s.io"],"resources":["clusterroles","clusterrolebindings"],"resourceNames":[identifier],"verbs":["get"]},
            {"apiGroups":["admissionregistration.k8s.io"],"resources":["validatingadmissionpolicies","validatingadmissionpolicybindings"],"resourceNames":[namespace_policy,binding_policy],"verbs":["get"]},
            {"apiGroups":["authentication.k8s.io"],"resources":["selfsubjectreviews"],"verbs":["create"]},
            {"apiGroups":["authorization.k8s.io"],"resources":["selfsubjectaccessreviews"],"verbs":["create"]}
        ]})));
        manifests.push(object("ClusterRoleBinding",&identifier,json!({"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":identifier},"subjects":[{"kind":"ServiceAccount","name":config.controller_service_account,"namespace":config.control_namespace}]})));
        Ok(Self {
            manifests,
            subject,
            control_namespace: config.control_namespace.clone(),
            reconciler_role: config.reconciler_role.clone(),
            installation_id: scope.id,
        })
    }

    pub fn reconciler_role(scope: &InstallationScope) -> String {
        format!("hi-{}-reconciler", scope.id.simple())
    }

    pub fn manifests(&self) -> &[Value] {
        &self.manifests
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Read-only verification plus negative dry-run admission probes, using the
    /// actual controller credential. No production resources are written here.
    pub async fn verify(&self, client: &Client) -> Result<(), Error> {
        timeout(Duration::from_secs(15), self.verify_inner(client))
            .await
            .map_err(|_| Error::Unavailable)?
    }

    async fn verify_inner(&self, client: &Client) -> Result<(), Error> {
        let review = json!({"apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview","metadata":{}});
        let response = call_create(client, &review, false).await?;
        if response["status"]["userInfo"]["username"] != self.subject {
            return Err(Error::Forbidden);
        }
        for manifest in &self.manifests {
            let actual = bounded(
                api(client, manifest)?.get(
                    manifest["metadata"]["name"]
                        .as_str()
                        .ok_or(Error::Invalid)?,
                ),
            )
            .await?;
            let mut actual = serde_json::to_value(actual).map_err(|_| Error::Unavailable)?;
            normalize(&mut actual);
            if manifest["kind"] == "ClusterRole" && !actual["aggregationRule"].is_null() {
                return Err(Error::OwnershipConflict);
            }
            let fields = if manifest["kind"]
                .as_str()
                .is_some_and(|k| k.starts_with("ValidatingAdmission"))
            {
                vec!["spec"]
            } else if manifest["kind"] == "ClusterRole" {
                vec!["rules"]
            } else {
                vec!["roleRef", "subjects"]
            };
            for field in fields {
                if actual[field] != manifest[field] {
                    return Err(Error::OwnershipConflict);
                }
            }
            if manifest["kind"] == "ValidatingAdmissionPolicy" {
                if actual["status"]["observedGeneration"] != actual["metadata"]["generation"] {
                    return Err(Error::Unavailable);
                }
                if actual["status"]["typeChecking"]["expressionWarnings"]
                    .as_array()
                    .is_some_and(|warnings| !warnings.is_empty())
                {
                    return Err(Error::OwnershipConflict);
                }
            }
        }
        for attributes in [
            json!({"namespace":self.control_namespace,"verb":"get","resource":"secrets"}),
            json!({"namespace":self.control_namespace,"verb":"get","resource":"pods"}),
            json!({"verb":"list","resource":"secrets"}),
            json!({"verb":"watch","resource":"secrets"}),
            json!({"namespace":self.control_namespace,"verb":"create","resource":"pods"}),
            json!({"namespace":self.control_namespace,"verb":"create","resource":"deployments","group":"apps"}),
            json!({"namespace":self.control_namespace,"verb":"create","resource":"serviceaccounts","subresource":"token"}),
            json!({"namespace":self.control_namespace,"verb":"create","resource":"pods","subresource":"exec"}),
            json!({"verb":"create","resource":"clusterrolebindings","group":"rbac.authorization.k8s.io"}),
            json!({"verb":"create","resource":"clusterroles","group":"rbac.authorization.k8s.io"}),
            json!({"verb":"bind","resource":"clusterroles","group":"rbac.authorization.k8s.io","name":"cluster-admin"}),
            json!({"verb":"delete","resource":"namespaces"}),
        ] {
            let review = json!({"apiVersion":"authorization.k8s.io/v1","kind":"SelfSubjectAccessReview","metadata":{},"spec":{"resourceAttributes":attributes}});
            if call_create(client, &review, false).await?["status"]["allowed"] != false {
                return Err(Error::Forbidden);
            }
        }
        let bad_namespace = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"outside-personal-scope"}});
        expect_denied(client, &bad_namespace).await?;
        let instance = uuid::Uuid::new_v4();
        let valid_namespace = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":hermes_domain::instance_namespace(self.installation_id,instance),"labels":{"agent-platform.io/installation":self.installation_id.to_string(),"agent-platform.io/instance":instance.to_string(),"pod-security.kubernetes.io/enforce":"restricted"}}});
        call_create(client, &valid_namespace, true).await?;
        let subject = self.subject.rsplit_once(':').ok_or(Error::Invalid)?.1;
        let bad_binding = json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":"instance-reconciler","namespace":self.control_namespace},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":self.reconciler_role},"subjects":[{"kind":"ServiceAccount","namespace":self.control_namespace,"name":subject}]});
        expect_denied(client, &bad_binding).await
    }
}
fn normalize(actual: &mut Value) {
    for path in ["/spec/matchConstraints", "/spec/matchResources"] {
        if let Some(map) = actual.pointer_mut(path).and_then(Value::as_object_mut)
            && map.get("excludeResourceRules") == Some(&json!([]))
        {
            map.remove("excludeResourceRules");
        }
    }
    if let Some(map) = actual.get_mut("spec").and_then(Value::as_object_mut) {
        for key in ["variables", "auditAnnotations"] {
            if map.get(key) == Some(&json!([])) {
                map.remove(key);
            }
        }
        if map.get("matchResources")
            == Some(&json!({"matchPolicy":"Equivalent","namespaceSelector":{},"objectSelector":{}}))
        {
            map.remove("matchResources");
        }
    }
    if let Some(subjects) = actual["subjects"].as_array_mut() {
        for subject in subjects {
            if subject["apiGroup"] == "" {
                subject.as_object_mut().unwrap().remove("apiGroup");
            }
        }
    }
}
fn api(client: &Client, object: &Value) -> Result<Api<DynamicObject>, Error> {
    let kind = object["kind"].as_str().ok_or(Error::Invalid)?;
    let version = object["apiVersion"].as_str().ok_or(Error::Invalid)?;
    let (group, version) = version.split_once('/').unwrap_or(("", version));
    let plural = match kind {
        "Namespace" => "namespaces",
        "RoleBinding" => "rolebindings",
        "ClusterRole" => "clusterroles",
        "ClusterRoleBinding" => "clusterrolebindings",
        "ValidatingAdmissionPolicy" => "validatingadmissionpolicies",
        "ValidatingAdmissionPolicyBinding" => "validatingadmissionpolicybindings",
        "SelfSubjectReview" => "selfsubjectreviews",
        "SelfSubjectAccessReview" => "selfsubjectaccessreviews",
        _ => return Err(Error::Invalid),
    };
    let resource =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(group, version, kind), plural);
    Ok(match object["metadata"]["namespace"].as_str() {
        Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
        None => Api::all_with(client.clone(), &resource),
    })
}
async fn bounded<T>(
    request: impl std::future::Future<Output = Result<T, kube::Error>>,
) -> Result<T, Error> {
    timeout(Duration::from_secs(6), request)
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(|_| Error::Unavailable)
}
async fn call_create(client: &Client, object: &Value, dry_run: bool) -> Result<Value, Error> {
    let data: DynamicObject = serde_json::from_value(object.clone()).map_err(|_| Error::Invalid)?;
    let response = bounded(api(client, object)?.create(
        &PostParams {
            dry_run,
            ..PostParams::default()
        },
        &data,
    ))
    .await?;
    serde_json::to_value(response).map_err(|_| Error::Unavailable)
}
async fn expect_denied(client: &Client, object: &Value) -> Result<(), Error> {
    let data: DynamicObject = serde_json::from_value(object.clone()).map_err(|_| Error::Invalid)?;
    match timeout(
        Duration::from_secs(6),
        api(client, object)?.create(
            &PostParams {
                dry_run: true,
                ..PostParams::default()
            },
            &data,
        ),
    )
    .await
    {
        Ok(Err(kube::Error::Api(response))) if response.code == 403 => Ok(()),
        _ => Err(Error::Forbidden),
    }
}
