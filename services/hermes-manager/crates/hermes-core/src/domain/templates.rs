//! Hermes template definitions (`contracts/hermes-manager/schemas/template-v2.json`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::names::validate_image;
use super::secrets::{SecretReference, validate_references};
use super::settings::validate_admin_config;
use super::storage::{
    HERMES_RESERVED, MAX_VOLUME_GIB, StorageSources, VolumeSpec, validate_mount_paths,
};
use super::{Problem, Problems, pointer_segment};

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resources {
    pub cpu_millis: u32,
    pub memory_mib: u32,
}

/// The private Hermes home: always a dynamic volume, mounted at `/opt/data`
/// in Hermes only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HomeVolume {
    pub source: String,
    pub size_gib: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Mount {
    pub mount_path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateDefinition {
    pub schema_version: u32,
    pub image: String,
    pub resources: Resources,
    pub home: HomeVolume,
    /// Volumes this template mounts into Hermes, by name.
    #[serde(default)]
    pub mounts: BTreeMap<String, Mount>,
    /// Volumes created for assignments that do not define them.
    #[serde(default)]
    pub default_volumes: BTreeMap<String, VolumeSpec>,
    #[serde(default)]
    pub admin_config: Map<String, Value>,
    #[serde(default)]
    pub secrets: Vec<SecretReference>,
}

impl TemplateDefinition {
    pub fn validate(&self, sources: &StorageSources) -> Result<(), Vec<Problem>> {
        let mut problems = Problems::default();
        problems.check(
            self.schema_version == SCHEMA_VERSION,
            "/schemaVersion",
            "unsupported_schema",
        );
        problems.check(
            validate_image(&self.image).is_ok(),
            "/image",
            "invalid_image",
        );
        problems.check(
            (10..=64_000).contains(&self.resources.cpu_millis),
            "/resources/cpuMillis",
            "out_of_range",
        );
        problems.check(
            (128..=262_144).contains(&self.resources.memory_mib),
            "/resources/memoryMib",
            "out_of_range",
        );
        match sources.get(&self.home.source) {
            Some(source) if source.dynamic() => {}
            Some(_) => problems.add("/home/source", "dynamic_source_required"),
            None => problems.add("/home/source", "unknown_storage_source"),
        }
        problems.check(
            (1..=MAX_VOLUME_GIB).contains(&self.home.size_gib),
            "/home/sizeGib",
            "out_of_range",
        );
        let paths: Vec<_> = self
            .mounts
            .iter()
            .map(|(name, mount)| (name, &mount.mount_path))
            .collect();
        validate_mount_paths(paths, HERMES_RESERVED, "/mounts", &mut problems);
        for (name, volume) in &self.default_volumes {
            let at = format!("/defaultVolumes/{}", pointer_segment(name));
            problems.check(self.mounts.contains_key(name), &at, "volume_not_mounted");
            volume.validate(sources, &at, &mut problems);
        }
        validate_admin_config(&self.admin_config, "/adminConfig", &mut problems);
        validate_references(&self.secrets, "/secrets", &mut problems);
        problems.into_result()
    }

    /// SHA-256 of the canonical serialization, lowercase hex.
    pub fn digest(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// Whether an assignment may move from `current` to `next` without a storage
/// migration: the home stays on its source and never shrinks.
pub fn rollout(
    current: &TemplateDefinition,
    next: &TemplateDefinition,
) -> Result<(), Vec<Problem>> {
    let mut problems = Problems::default();
    problems.check(
        current.home.source == next.home.source,
        "/home/source",
        "storage_migration_required",
    );
    problems.check(
        next.home.size_gib >= current.home.size_gib,
        "/home/sizeGib",
        "storage_migration_required",
    );
    problems.into_result()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::domain::storage::{SourceKind, StorageSource};
    use serde_json::json;

    pub fn sources() -> StorageSources {
        StorageSources::new(vec![
            StorageSource {
                id: "nvme".into(),
                kind: SourceKind::StorageClass {
                    name: "local-nvme".into(),
                },
            },
            StorageSource {
                id: "nas".into(),
                kind: SourceKind::Nfs {
                    server: "nas.internal".into(),
                    root: "/export/homes".into(),
                },
            },
        ])
        .unwrap()
    }

    pub fn definition() -> TemplateDefinition {
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "image": format!("ghcr.io/accelero/agent-platform/hermes@sha256:{}", "a".repeat(64)),
            "resources": {"cpuMillis": 1000, "memoryMib": 2048},
            "home": {"source": "nvme", "sizeGib": 5},
            "mounts": {"workspace": {"mountPath": "/workspace"}},
            "defaultVolumes": {"workspace": {"source": "nvme", "sizeGib": 10}},
            "adminConfig": {"model": {"provider": "custom"}},
            "secrets": [{"name": "llm-key", "env": "OPENAI_API_KEY"}],
        }))
        .unwrap()
    }

    fn pointers(result: Result<(), Vec<Problem>>) -> Vec<(String, &'static str)> {
        result
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.pointer, p.code))
            .collect()
    }

    #[test]
    fn a_complete_definition_is_valid_and_has_a_stable_digest() {
        assert_eq!(definition().validate(&sources()), Ok(()));
        assert_eq!(definition().digest(), definition().digest());
        assert_eq!(definition().digest().len(), 64);
    }

    #[test]
    fn invalid_fields_are_reported_at_their_pointers() {
        let mut bad = definition();
        bad.schema_version = 1;
        bad.image = "ghcr.io/hermes:latest".into();
        bad.resources.cpu_millis = 0;
        bad.home.source = "nas".into();
        bad.mounts.insert(
            "data".into(),
            Mount {
                mount_path: "/opt/data/x".into(),
                read_only: false,
            },
        );
        bad.default_volumes.insert(
            "orphan".into(),
            VolumeSpec {
                source: "nvme".into(),
                path: None,
                size_gib: Some(1),
                read_only: false,
            },
        );
        assert_eq!(
            pointers(bad.validate(&sources())),
            [
                ("/schemaVersion".to_owned(), "unsupported_schema"),
                ("/image".to_owned(), "invalid_image"),
                ("/resources/cpuMillis".to_owned(), "out_of_range"),
                ("/home/source".to_owned(), "dynamic_source_required"),
                ("/mounts/data/mountPath".to_owned(), "reserved_path"),
                ("/defaultVolumes/orphan".to_owned(), "volume_not_mounted"),
            ]
        );
    }

    #[test]
    fn rollout_never_moves_or_shrinks_the_home() {
        let current = definition();
        let mut grown = definition();
        grown.home.size_gib = 10;
        assert_eq!(rollout(&current, &grown), Ok(()));
        assert_eq!(
            pointers(rollout(&grown, &current)),
            [("/home/sizeGib".to_owned(), "storage_migration_required")]
        );
        let mut moved = definition();
        moved.home.source = "other".into();
        assert_eq!(
            pointers(rollout(&current, &moved)),
            [("/home/source".to_owned(), "storage_migration_required")]
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut value = serde_json::to_value(definition()).unwrap();
        value["storageClass"] = json!("local-path");
        assert!(serde_json::from_value::<TemplateDefinition>(value).is_err());
    }
}
