//! Assignments: an administrator's decision for one identity.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::backends::BackendDefinition;
use super::catalog::VersionRef;
use super::names::validate_name;
use super::secrets::{SecretReference, validate_references};
use super::settings::validate_admin_config;
use super::storage::{StorageSources, VolumeSpec, volume_name_problem};
use super::templates::TemplateDefinition;
use super::{ErrorKind, Problem, Problems, Rejection, pointer_segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Enabled,
    Suspended,
    Revoked,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Suspended => "suspended",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enabled" => Some(Self::Enabled),
            "suspended" => Some(Self::Suspended),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// How long terminal environments live. Written into the managed Hermes
/// configuration; the plugin sends it with every acquire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeMode {
    #[default]
    PerSession,
    PerProfileOnDemand,
    PerProfileAlwaysRunning,
}

impl LifetimeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerSession => "per_session",
            Self::PerProfileOnDemand => "per_profile_on_demand",
            Self::PerProfileAlwaysRunning => "per_profile_always_running",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "per_session" => Some(Self::PerSession),
            "per_profile_on_demand" => Some(Self::PerProfileOnDemand),
            "per_profile_always_running" => Some(Self::PerProfileAlwaysRunning),
            _ => None,
        }
    }
}

/// The terminal backends an instance may use, pinned to exact versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalGrant {
    #[serde(default)]
    pub mode: LifetimeMode,
    /// Used when Hermes requests no backend; must be allowed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Backend ID → version.
    #[serde(default)]
    pub allowed: BTreeMap<String, i64>,
}

/// Everything an assignment decides besides identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssignmentSpec {
    pub access: Access,
    pub template: VersionRef,
    #[serde(default)]
    pub volumes: BTreeMap<String, VolumeSpec>,
    #[serde(default)]
    pub terminal: TerminalGrant,
    /// Overrides layered over the template's admin settings.
    #[serde(default)]
    pub admin_config: Map<String, Value>,
    #[serde(default)]
    pub secrets: Vec<SecretReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentRejected {
    Exists,
    NotFound,
    StaleVersion,
}

impl Rejection for AssignmentRejected {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Exists => ErrorKind::Conflict,
            Self::NotFound => ErrorKind::NotFound,
            Self::StaleVersion => ErrorKind::Precondition,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Exists => "assignment_exists",
            Self::NotFound => "not_found",
            Self::StaleVersion => "stale_version",
        }
    }
}

impl AssignmentSpec {
    /// Adds the template's default volumes for mounted names the spec leaves out.
    pub fn with_template_defaults(mut self, template: &TemplateDefinition) -> Self {
        for (name, volume) in &template.default_volumes {
            self.volumes
                .entry(name.clone())
                .or_insert_with(|| volume.clone());
        }
        self
    }

    /// Checks the spec against its pinned definitions and the installation's
    /// storage sources. `backends` holds the definitions of `terminal.allowed`.
    pub fn validate(
        &self,
        template: &TemplateDefinition,
        backends: &BTreeMap<String, BackendDefinition>,
        sources: &StorageSources,
    ) -> Result<(), Vec<Problem>> {
        let mut problems = Problems::default();
        let volume = |name: &str| format!("/volumes/{}", pointer_segment(name));
        for (name, spec) in &self.volumes {
            if let Some(code) = volume_name_problem(name) {
                problems.add(volume(name), code);
            }
            spec.validate(sources, &volume(name), &mut problems);
        }
        let names: Vec<_> = self.volumes.iter().collect();
        for (index, (name, spec)) in names.iter().enumerate() {
            if names[..index].iter().any(|(_, other)| spec.overlaps(other)) {
                problems.add(volume(name), "volume_overlap");
            }
        }
        for name in template.mounts.keys() {
            problems.check(
                self.volumes.contains_key(name),
                volume(name),
                "mount_slot_unbound",
            );
        }
        if let Some(default) = &self.terminal.default {
            problems.check(
                self.terminal.allowed.contains_key(default),
                "/terminal/default",
                "backend_not_allowed",
            );
        }
        for (id, version) in &self.terminal.allowed {
            let at = format!("/terminal/allowed/{}", pointer_segment(id));
            problems.check(
                validate_name(id).is_ok() && *version >= 1,
                &at,
                "invalid_reference",
            );
            let Some(backend) = backends.get(id) else {
                problems.add(at, "backend_unavailable");
                continue;
            };
            for (slot_name, slot) in &backend.mounts {
                if !slot.optional && !self.volumes.contains_key(slot_name) {
                    problems.add(
                        format!("{at}/{}", pointer_segment(slot_name)),
                        "mount_slot_unbound",
                    );
                }
                if let Some(mount) = template.mounts.get(slot_name)
                    && mount.mount_path != slot.mount_path
                {
                    problems.add(
                        format!("{at}/{}", pointer_segment(slot_name)),
                        "mount_path_mismatch",
                    );
                }
            }
        }
        validate_admin_config(&self.admin_config, "/adminConfig", &mut problems);
        validate_references(&self.secrets, "/secrets", &mut problems);
        problems.into_result()
    }

    /// Whether terminal environments must be recreated for this change.
    pub fn execution_changed(&self, next: &Self) -> bool {
        self.volumes != next.volumes || self.terminal != next.terminal
    }
}

/// Volumes of an instance that overlap other instances' volumes.
pub fn overlapping<'a>(
    volumes: &BTreeMap<String, VolumeSpec>,
    others: impl IntoIterator<Item = &'a VolumeSpec> + Clone,
) -> Result<(), Vec<Problem>> {
    let mut problems = Problems::default();
    for (name, volume) in volumes {
        if others
            .clone()
            .into_iter()
            .any(|other| volume.overlaps(other))
        {
            problems.add(
                format!("/volumes/{}", pointer_segment(name)),
                "volume_overlap",
            );
        }
    }
    problems.into_result()
}

/// Existing dynamic volumes keep their source and never shrink; a volume
/// cannot switch between dynamic and static storage.
pub fn storage_change(
    current: &BTreeMap<String, VolumeSpec>,
    next: &BTreeMap<String, VolumeSpec>,
    sources: &StorageSources,
) -> Result<(), Vec<Problem>> {
    let mut problems = Problems::default();
    for (name, before) in current {
        let Some(after) = next.get(name) else {
            continue;
        };
        let dynamic = |spec: &VolumeSpec| {
            sources
                .get(&spec.source)
                .is_some_and(|source| source.dynamic())
        };
        let at = format!("/volumes/{}", pointer_segment(name));
        if dynamic(before) || dynamic(after) {
            let same_place = before.source == after.source;
            let not_smaller = after.size_gib.unwrap_or(0) >= before.size_gib.unwrap_or(0);
            problems.check(same_place && not_smaller, at, "storage_migration_required");
        }
    }
    problems.into_result()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backends::tests::definition as backend;
    use crate::domain::templates::tests::{definition as template, sources};
    use serde_json::json;

    fn spec() -> AssignmentSpec {
        serde_json::from_value(json!({
            "access": "enabled",
            "template": {"id": "personal-default", "version": 3},
            "volumes": {
                "workspace": {"source": "nvme", "sizeGib": 20},
                "home": {"source": "nas", "path": "bob"},
            },
            "terminal": {"default": "python-tools", "allowed": {"python-tools": 2}},
        }))
        .unwrap()
    }

    fn backends() -> BTreeMap<String, BackendDefinition> {
        BTreeMap::from([("python-tools".to_owned(), backend())])
    }

    fn codes(result: Result<(), Vec<Problem>>) -> Vec<(String, &'static str)> {
        result
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.pointer, p.code))
            .collect()
    }

    #[test]
    fn bobs_assignment_binds_every_mount_with_matching_paths() {
        assert_eq!(
            spec().validate(&template(), &backends(), &sources()),
            Ok(())
        );
    }

    #[test]
    fn mounts_must_be_bound_and_shared_paths_must_agree() {
        let mut unbound = spec();
        unbound.volumes.remove("workspace");
        assert_eq!(
            codes(unbound.validate(&template(), &backends(), &sources())),
            [
                ("/volumes/workspace".to_owned(), "mount_slot_unbound"),
                (
                    "/terminal/allowed/python-tools/workspace".to_owned(),
                    "mount_slot_unbound"
                ),
            ]
        );
        let mut moved = backend();
        moved.mounts.get_mut("workspace").unwrap().mount_path = "/home/worker/workspace".into();
        let backends = BTreeMap::from([("python-tools".to_owned(), moved)]);
        assert_eq!(
            codes(spec().validate(&template(), &backends, &sources())),
            [(
                "/terminal/allowed/python-tools/workspace".to_owned(),
                "mount_path_mismatch"
            )]
        );
        let mut optional_missing = spec();
        optional_missing.volumes.remove("home");
        assert_eq!(
            optional_missing.validate(&template(), &self::backends(), &sources()),
            Ok(())
        );
    }

    #[test]
    fn defaults_and_backends_must_be_allowed_and_available() {
        let mut stray = spec();
        stray.terminal.default = Some("gpu-tools".into());
        stray.terminal.allowed.insert("gpu-tools".into(), 1);
        assert_eq!(
            codes(stray.validate(&template(), &backends(), &sources())),
            [(
                "/terminal/allowed/gpu-tools".to_owned(),
                "backend_unavailable"
            )]
        );
        let mut not_allowed = spec();
        not_allowed.terminal.default = Some("other".into());
        assert_eq!(
            codes(not_allowed.validate(&template(), &backends(), &sources())),
            [("/terminal/default".to_owned(), "backend_not_allowed")]
        );
    }

    #[test]
    fn template_defaults_fill_only_missing_volumes() {
        let mut empty = spec();
        empty.volumes.clear();
        let filled = empty.with_template_defaults(&template());
        assert_eq!(filled.volumes["workspace"].size_gib, Some(10));
        let kept = spec().with_template_defaults(&template());
        assert_eq!(kept.volumes["workspace"].size_gib, Some(20));
    }

    #[test]
    fn owners_cannot_overlap_and_dynamic_volumes_never_move_or_shrink() {
        let alice = VolumeSpec {
            source: "nas".into(),
            path: Some("bob/shared".into()),
            size_gib: None,
            read_only: false,
        };
        assert_eq!(
            codes(overlapping(&spec().volumes, [&alice])),
            [("/volumes/home".to_owned(), "volume_overlap")]
        );
        let mut smaller = spec();
        smaller.volumes.get_mut("workspace").unwrap().size_gib = Some(5);
        assert_eq!(
            codes(storage_change(
                &spec().volumes,
                &smaller.volumes,
                &sources()
            )),
            [(
                "/volumes/workspace".to_owned(),
                "storage_migration_required"
            )]
        );
        let mut rehomed = spec();
        rehomed.volumes.get_mut("home").unwrap().path = Some("bob-new".into());
        assert_eq!(
            storage_change(&spec().volumes, &rehomed.volumes, &sources()),
            Ok(())
        );
        assert!(spec().execution_changed(&rehomed));
        let mut renamed = spec();
        renamed.access = Access::Suspended;
        assert!(!spec().execution_changed(&renamed));
    }
}
