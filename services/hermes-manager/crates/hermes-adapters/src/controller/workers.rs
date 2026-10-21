//! Fenced on-demand worker creation and authenticated, pinned SSH readiness.
use super::*;
use hermes_application::terminal::leases::{
    ExecutionEnvironmentRepository, ExecutionLeaseRepository, WorkerReady,
};
use std::{io::Write, process::Stdio, time::Duration};

impl Controller {
    pub(super) async fn worker_step(&self) -> Result<bool, Error> {
        self.store.expire_execution_leases(&self.scope, 64).await?;
        let retired = self.retire_worker_step().await?;
        let observed = if let Some(work) = self
            .store
            .claim_ready_worker_observation(&self.scope, self.worker)
            .await?
        {
            if self.driver.execution_worker_terminated(&work).await? {
                self.store
                    .mark_execution_worker_lost(&self.scope, &work)
                    .await?;
            }
            true
        } else {
            false
        };
        let Some(claim) = self
            .store
            .claim_execution_environment(&self.scope, self.worker, 120)
            .await?
        else {
            return Ok(retired || observed);
        };
        let outcome = self.reconcile_worker(&claim).await;
        match outcome {
            Ok(true) => {}
            Err(Error::StaleFence) => return Ok(true),
            Err(error @ (Error::Maintenance | Error::RecoveryRequired)) => return Err(error),
            _ => {
                self.store
                    .defer_execution_environment(&self.scope, &claim)
                    .await?
            }
        }
        Ok(true)
    }

    async fn retire_worker_step(&self) -> Result<bool, Error> {
        let Some(work) = self
            .store
            .claim_worker_retirement(&self.scope, self.worker)
            .await?
        else {
            return Ok(false);
        };
        if work.intent.is_none() {
            // No committed creation intent means no CREATE was dispatched.
            self.store
                .complete_worker_retirement(&self.scope, &work, None)
                .await?;
            return Ok(true);
        }
        let binding = match self.driver.retiring_execution_worker(&work).await {
            Ok(binding) => binding,
            Err(Error::Maintenance | Error::RecoveryRequired) => {
                return Err(Error::RecoveryRequired);
            }
            Err(_) => {
                self.store
                    .defer_worker_retirement(&self.scope, &work)
                    .await?;
                return Ok(true);
            }
        };
        if let Some(binding) = binding {
            self.store
                .prepare_worker_stop(&self.scope, &work, &binding)
                .await?;
            match self.driver.delete_execution_worker(&work, &binding).await {
                Ok(true) => {
                    self.store
                        .complete_worker_retirement(&self.scope, &work, Some(&binding.pod_uid))
                        .await?;
                    return Ok(true);
                }
                Ok(false) | Err(Error::PreconditionFailed) => {}
                Err(error) => return Err(error),
            }
        } else if work.pod_uid.is_some() {
            self.store
                .complete_worker_retirement(&self.scope, &work, work.pod_uid.as_deref())
                .await?;
            return Ok(true);
        }
        // An uncertain unbound CREATE stays retained while GET sees no object.
        self.store
            .defer_worker_retirement(&self.scope, &work)
            .await?;
        Ok(true)
    }

    async fn reconcile_worker(
        &self,
        claim: &hermes_application::terminal::leases::EnvironmentClaim,
    ) -> Result<bool, Error> {
        self.driver.verify_bootstrap(&self.bootstrap).await?;
        let runtime = claim
            .policy
            .runtimes
            .iter()
            .find(|r| r.definition.runtime.id == claim.runtime_id)
            .ok_or(Error::Forbidden)?;
        if runtime.definition.ssh_contract != "managed-ssh-v2" {
            return Err(Error::TemplateUnavailable);
        }
        let record = self
            .store
            .execution_bootstrap_material(&self.scope, claim)
            .await?;
        let bootstrap =
            crate::native_bootstrap::NativeBootstrap::decode(&self.vault.open(record)?)?;
        let rendered = crate::execution_workloads::render(
            &claim.policy,
            &claim.runtime_id,
            &crate::execution_workloads::WorkerContext {
                assignment_generation: claim.assignment_generation,
                environment_id: claim.environment_id,
                environment_generation: claim.generation,
            },
        )?;
        let intent = self
            .store
            .prepare_execution_worker(&self.scope, claim, &rendered.resource.spec.manifest_digest)
            .await?;
        let binding = self.driver.ensure_execution_worker(&intent).await?;
        let start = self
            .store
            .bind_execution_worker(&self.scope, &intent, &binding)
            .await?;
        self.driver.start_execution_worker(&start).await?;
        let Some(candidate) = self.driver.execution_worker_candidate(&start).await? else {
            return Ok(false);
        };
        let host_key = bootstrap
            .worker_host_key()?
            .public_key()
            .to_openssh()
            .map_err(|_| Error::Invalid)?;
        verify_ssh(
            &candidate.host.to_string(),
            &bootstrap.ssh_private_key,
            &host_key,
            runtime.definition.uid,
        )
        .await?;
        // SSH I/O cannot authorize a stale Pod or assignment. Observe again,
        // then compare the current SQL claim at the final commit boundary.
        let Some(after) = self.driver.execution_worker_candidate(&start).await? else {
            return Ok(false);
        };
        if after.host != candidate.host
            || after.binding.pod_uid != candidate.binding.pod_uid
            || after.binding.observed_spec_digest != candidate.binding.observed_spec_digest
        {
            return Err(Error::OwnershipConflict);
        }
        self.store
            .record_execution_worker_ready(
                &self.scope,
                &intent,
                &WorkerReady {
                    observed_spec_digest: after.binding.observed_spec_digest,
                    pod_uid: after.binding.pod_uid,
                    resource_version: after.binding.resource_version,
                    spec_digest: intent.spec_digest.clone(),
                    host: after.host.to_string(),
                    host_key: Some(host_key),
                },
            )
            .await?;
        Ok(true)
    }
}

async fn verify_ssh(host: &str, private: &str, host_key: &str, uid: u32) -> Result<(), Error> {
    let mut key = tempfile::NamedTempFile::new().map_err(|_| Error::Unavailable)?;
    key.write_all(private.as_bytes())
        .map_err(|_| Error::Unavailable)?;
    key.flush().map_err(|_| Error::Unavailable)?;
    let mut known = tempfile::NamedTempFile::new().map_err(|_| Error::Unavailable)?;
    writeln!(known, "[{host}]:2222 {host_key}").map_err(|_| Error::Unavailable)?;
    known.flush().map_err(|_| Error::Unavailable)?;
    let mut command = tokio::process::Command::new("/usr/bin/ssh");
    command
        .args([
            "-F",
            "/dev/null",
            "-T",
            "-p",
            "2222",
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "GlobalKnownHostsFile=/dev/null",
            "-o",
            "HostKeyAlgorithms=ssh-ed25519",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ProxyCommand=none",
            "-o",
            "ProxyJump=none",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
        ])
        .arg(format!("UserKnownHostsFile={}", known.path().display()))
        .arg("-i")
        .arg(key.path())
        .arg(format!("worker@{host}"))
        .arg(format!("test \"$(id -u)\" = {uid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(8), command.status())
        .await
        .map_err(|_| Error::Unavailable)?
        .map_err(|_| Error::Unavailable)?;
    if !status.success() {
        return Err(Error::PreconditionFailed);
    }
    Ok(())
}
