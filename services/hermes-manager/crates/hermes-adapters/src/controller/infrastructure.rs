//! Native-only retirement. Persistent owner data is deliberately retained.
use super::*;
use hermes_application::resources::ResourceObservation;

impl Controller {
    pub(super) async fn reconcile_infrastructure_removal(
        &self,
        claim: &OperationClaim,
        plan: LifecyclePlan,
    ) -> Result<ConditionReason, Error> {
        let Some(namespace) = plan.namespace.clone() else {
            return self.finish_infrastructure_removal(claim).await;
        };
        let Some(namespace) = self.retained_resource(claim, namespace).await? else {
            // Keep observing uncertain creations; a point-in-time 404 is not a
            // cancellation receipt for an older in-flight API request.
            return Ok(ConditionReason::Draining);
        };
        self.fence_namespace(claim, &namespace, Admission::Closed)
            .await?;
        let Some(retained) = plan.deployment.clone() else {
            return self.finish_infrastructure_removal(claim).await;
        };
        let Some(mut deployment) = self.retained_resource(claim, retained).await? else {
            return Ok(ConditionReason::Draining);
        };
        if deployment.state == "Bound" {
            Box::pin(self.reconcile_stopping(claim, plan)).await?;
            if !self
                .store
                .native_children(&self.scope, claim)
                .await?
                .is_empty()
                || !self
                    .driver
                    .discover_native_children(&deployment, None, claim)
                    .await?
                    .is_empty()
            {
                return Ok(ConditionReason::Draining);
            }
            let version = self.driver.current_version(&deployment).await?;
            deployment.resource_version = Some(version.resource_version);
        }
        if self
            .retire_infrastructure_workload(claim, deployment)
            .await?
        {
            self.finish_infrastructure_removal(claim).await
        } else {
            Ok(ConditionReason::Draining)
        }
    }

    pub(super) async fn retire_infrastructure_workload(
        &self,
        claim: &OperationClaim,
        deployment: OwnedResource,
    ) -> Result<bool, Error> {
        let resource_version = deployment.resource_version.ok_or(Error::RecoveryRequired)?;
        let suffix: String = Sha256::digest(resource_version.as_bytes())[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let reconciler =
            ResourceReconciler::new(self.store.clone(), self.driver.clone(), self.scope.clone());
        reconciler
            .retire(
                claim,
                &format!("remove-native-{suffix}"),
                deployment.creation.id,
                ResourceObservation {
                    uid: deployment.uid.ok_or(Error::RecoveryRequired)?,
                    resource_version,
                    manifest_digest: deployment.observed_digest.ok_or(Error::RecoveryRequired)?,
                },
            )
            .await
    }

    async fn finish_infrastructure_removal(
        &self,
        claim: &OperationClaim,
    ) -> Result<ConditionReason, Error> {
        if self
            .store
            .complete_native_removal(&self.scope, claim)
            .await?
        {
            Ok(ConditionReason::NativeRemoved)
        } else {
            Ok(ConditionReason::Draining)
        }
    }
}
