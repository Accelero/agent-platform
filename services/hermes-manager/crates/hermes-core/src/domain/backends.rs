//! Terminal backend definitions (`schemas/terminal-backend-v1.json`): an SSH
//! worker image with its resources, limits and mount slots.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::names::{validate_image, validate_name};
use super::storage::{DataIdentity, WORKER_RESERVED, validate_mount_paths};
use super::{Problem, Problems};

pub const SSH_CONTRACT: &str = "managed-ssh-v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerResources {
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub ephemeral_gib: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    /// Idle time before an environment without leases is retired.
    pub idle_seconds: u32,
    /// Longest single lease.
    pub max_lease_seconds: u32,
    /// Live environments of this backend per instance.
    pub max_environments: u32,
}

/// Where a volume appears in the worker, if the assignment binds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Slot {
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
    /// An unbound optional slot is skipped; an unbound required slot makes the
    /// backend unusable for that instance.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackendDefinition {
    pub image: String,
    pub ssh_contract: String,
    pub uid: u32,
    pub gid: u32,
    pub resources: WorkerResources,
    pub limits: Limits,
    #[serde(default)]
    pub mounts: BTreeMap<String, Slot>,
    /// A `registry` admin secret for private images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_secret: Option<String>,
}

impl BackendDefinition {
    pub fn validate(&self, identity: DataIdentity) -> Result<(), Vec<Problem>> {
        let mut problems = Problems::default();
        problems.check(
            validate_image(&self.image).is_ok(),
            "/image",
            "invalid_image",
        );
        problems.check(
            self.ssh_contract == SSH_CONTRACT,
            "/sshContract",
            "unsupported_contract",
        );
        problems.check(self.uid == identity.uid, "/uid", "data_identity_mismatch");
        problems.check(self.gid == identity.gid, "/gid", "data_identity_mismatch");
        let resources = &self.resources;
        problems.check(
            (10..=64_000).contains(&resources.cpu_millis),
            "/resources/cpuMillis",
            "out_of_range",
        );
        problems.check(
            (64..=262_144).contains(&resources.memory_mib),
            "/resources/memoryMib",
            "out_of_range",
        );
        problems.check(
            (1..=1024).contains(&resources.ephemeral_gib),
            "/resources/ephemeralGib",
            "out_of_range",
        );
        let limits = &self.limits;
        problems.check(
            (60..=86_400).contains(&limits.idle_seconds),
            "/limits/idleSeconds",
            "out_of_range",
        );
        problems.check(
            (60..=86_400).contains(&limits.max_lease_seconds),
            "/limits/maxLeaseSeconds",
            "out_of_range",
        );
        problems.check(
            (1..=64).contains(&limits.max_environments),
            "/limits/maxEnvironments",
            "out_of_range",
        );
        let paths: Vec<_> = self
            .mounts
            .iter()
            .map(|(name, slot)| (name, &slot.mount_path))
            .collect();
        validate_mount_paths(paths, WORKER_RESERVED, "/mounts", &mut problems);
        if let Some(secret) = &self.registry_secret {
            problems.check(
                validate_name(secret).is_ok(),
                "/registrySecret",
                "invalid_name",
            );
        }
        problems.into_result()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub fn definition() -> BackendDefinition {
        serde_json::from_value(json!({
            "image": format!("ghcr.io/accelero/agent-platform/agent-runtime@sha256:{}", "b".repeat(64)),
            "sshContract": "managed-ssh-v2",
            "uid": 1000,
            "gid": 1000,
            "resources": {"cpuMillis": 2000, "memoryMib": 4096, "ephemeralGib": 10},
            "limits": {"idleSeconds": 900, "maxLeaseSeconds": 3600, "maxEnvironments": 4},
            "mounts": {
                "workspace": {"mountPath": "/workspace"},
                "home": {"mountPath": "/home/worker/home", "optional": true},
            },
        }))
        .unwrap()
    }

    #[test]
    fn backends_must_match_the_data_identity_and_contract() {
        assert_eq!(definition().validate(DataIdentity::default()), Ok(()));
        let mut bad = definition();
        bad.uid = 0;
        bad.ssh_contract = "managed-ssh-v1".into();
        bad.mounts.insert(
            "keys".into(),
            Slot {
                mount_path: "/home/worker/.ssh".into(),
                read_only: true,
                optional: false,
            },
        );
        let codes: Vec<_> = bad
            .validate(DataIdentity::default())
            .unwrap_err()
            .into_iter()
            .map(|p| (p.pointer, p.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("/sshContract".to_owned(), "unsupported_contract"),
                ("/uid".to_owned(), "data_identity_mismatch"),
                ("/mounts/keys/mountPath".to_owned(), "reserved_path"),
            ]
        );
    }
}
