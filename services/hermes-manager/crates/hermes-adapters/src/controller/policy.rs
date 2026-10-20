use super::*;
use hermes_application::{
    execution::{ExecutionRepository, PolicyClaim, PolicyCondition},
    terminal::activation::PolicyActivationRepository,
};

impl Controller {
    pub(super) async fn policy_step(&self) -> Result<bool, Error> {
        let Some(claim) = self
            .store
            .claim_policy(&self.scope, self.worker, 30)
            .await?
        else {
            return Ok(false);
        };
        let outcome = self.reconcile_policy(&claim).await;
        match outcome {
            Ok(()) => {}
            Err(error @ (Error::StaleFence | Error::Maintenance | Error::RecoveryRequired)) => {
                return Err(error);
            }
            Err(error) => {
                let condition = match error {
                    Error::PreconditionFailed => PolicyCondition::AdmissionPending,
                    Error::OwnershipConflict | Error::Invalid | Error::Forbidden => {
                        PolicyCondition::AdmissionPending
                    }
                    _ => PolicyCondition::DependencyUnavailable,
                };
                self.store
                    .defer_policy(&self.scope, &claim, condition, 5)
                    .await?;
            }
        }
        Ok(true)
    }
    async fn reconcile_policy(&self, claim: &PolicyClaim) -> Result<(), Error> {
        self.driver.verify_bootstrap(&self.bootstrap).await?;
        self.store
            .renew_policy_activation(&self.scope, claim)
            .await?;
        let pending = self
            .store
            .pending_policy_admissions(&self.scope, claim)
            .await?;
        let mut conflicted = Vec::new();
        for prepared in pending {
            self.store
                .renew_policy_activation(&self.scope, claim)
                .await?;
            match self.driver.apply_policy_admission(&prepared).await {
                Ok(observation) => {
                    self.store
                        .record_policy_admission(&self.scope, claim, &observation)
                        .await?
                }
                Err(Error::PreconditionFailed) => conflicted.push(prepared),
                Err(error) => return Err(error),
            }
        }
        let plan = self.store.activation_plan(&self.scope, claim).await?;
        self.store
            .renew_policy_activation(&self.scope, claim)
            .await?;
        let input = self
            .driver
            .inspect_policy_admission(&plan, self.scope.epoch)
            .await?;
        let prepared = self
            .store
            .prepare_policy_admission(&self.scope, claim, &input)
            .await?;
        self.store
            .renew_policy_activation(&self.scope, claim)
            .await?;
        let observed = self.driver.apply_policy_admission(&prepared).await?;
        self.store
            .record_policy_admission(&self.scope, claim, &observed)
            .await?;
        // A fresh CAS can resolve older unknown inputs with stale RVs. Observe
        // their exact desired subtree and close each original input too.
        for prepared in conflicted {
            self.store
                .renew_policy_activation(&self.scope, claim)
                .await?;
            let observed = self.driver.apply_policy_admission(&prepared).await?;
            self.store
                .record_policy_admission(&self.scope, claim, &observed)
                .await?;
        }
        self.store
            .activate_execution_policy(&self.scope, claim, &input.digest()?)
            .await
    }
}
