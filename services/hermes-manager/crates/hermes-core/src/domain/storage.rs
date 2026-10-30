//! Storage sources, volumes and mounts.
//!
//! Sources are installation configuration: they bound what the controller may
//! ever mount and are also rendered into the admission policies. Volumes are
//! owner-bound storage of one instance, from one source. Mounts place a volume
//! at a path in the Hermes container or a terminal backend.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::names::{paths_overlap, validate_absolute_path, validate_name, validate_relative_path};
use super::{Problems, pointer_segment};

/// The UID/GID every Hermes and terminal container runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataIdentity {
    pub uid: u32,
    pub gid: u32,
}

impl Default for DataIdentity {
    fn default() -> Self {
        Self {
            uid: 1000,
            gid: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    /// Dynamically provisioned claims of a storage class.
    StorageClass { name: String },
    /// Existing directories under `root` on one node (`local` PersistentVolumes).
    LocalPath { node: String, root: String },
    /// Existing directories under `root` of one NFS export.
    Nfs { server: String, root: String },
}

/// `{"id": "nvme", "storageClass": {"name": "local-nvme"}}`: an ID and exactly
/// one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawSource", into = "RawSource")]
pub struct StorageSource {
    pub id: String,
    pub kind: SourceKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawSource {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage_class: Option<ClassSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_path: Option<LocalSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nfs: Option<NfsSpec>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassSpec {
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalSpec {
    node: String,
    root: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NfsSpec {
    server: String,
    root: String,
}

impl TryFrom<RawSource> for StorageSource {
    type Error = &'static str;

    fn try_from(raw: RawSource) -> Result<Self, Self::Error> {
        let kind = match (raw.storage_class, raw.local_path, raw.nfs) {
            (Some(ClassSpec { name }), None, None) => SourceKind::StorageClass { name },
            (None, Some(LocalSpec { node, root }), None) => SourceKind::LocalPath { node, root },
            (None, None, Some(NfsSpec { server, root })) => SourceKind::Nfs { server, root },
            _ => return Err("a storage source has exactly one of storageClass, localPath or nfs"),
        };
        Ok(Self { id: raw.id, kind })
    }
}

impl From<StorageSource> for RawSource {
    fn from(source: StorageSource) -> Self {
        let mut raw = Self {
            id: source.id,
            storage_class: None,
            local_path: None,
            nfs: None,
        };
        match source.kind {
            SourceKind::StorageClass { name } => raw.storage_class = Some(ClassSpec { name }),
            SourceKind::LocalPath { node, root } => raw.local_path = Some(LocalSpec { node, root }),
            SourceKind::Nfs { server, root } => raw.nfs = Some(NfsSpec { server, root }),
        }
        raw
    }
}

impl StorageSource {
    /// Whether volumes of this source are created by the provisioner.
    pub fn dynamic(&self) -> bool {
        matches!(self.kind, SourceKind::StorageClass { .. })
    }
}

/// The installation's approved sources, by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageSources(BTreeMap<String, StorageSource>);

/// A DNS name or IPv4 address, as used for node names and NFS servers.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

impl StorageSources {
    pub fn new(sources: Vec<StorageSource>) -> Result<Self, Vec<super::Problem>> {
        let mut problems = Problems::default();
        let mut map = BTreeMap::new();
        for (index, source) in sources.into_iter().enumerate() {
            let at = format!("/storageSources/{index}");
            problems.check(
                validate_name(&source.id).is_ok(),
                format!("{at}/id"),
                "invalid_name",
            );
            match &source.kind {
                SourceKind::StorageClass { name } => {
                    problems.check(
                        validate_name(name).is_ok(),
                        format!("{at}/storageClass/name"),
                        "invalid_name",
                    );
                }
                SourceKind::LocalPath { node, root } => {
                    problems.check(
                        valid_host(node),
                        format!("{at}/localPath/node"),
                        "invalid_name",
                    );
                    problems.check(
                        validate_absolute_path(root).is_ok(),
                        format!("{at}/localPath/root"),
                        "invalid_path",
                    );
                }
                SourceKind::Nfs { server, root } => {
                    problems.check(
                        valid_host(server),
                        format!("{at}/nfs/server"),
                        "invalid_name",
                    );
                    problems.check(
                        validate_absolute_path(root).is_ok(),
                        format!("{at}/nfs/root"),
                        "invalid_path",
                    );
                }
            }
            if map.insert(source.id.clone(), source).is_some() {
                problems.add(format!("{at}/id"), "duplicate_source");
            }
        }
        problems.into_result().map(|()| Self(map))
    }

    pub fn get(&self, id: &str) -> Option<&StorageSource> {
        self.0.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &StorageSource> {
        self.0.values()
    }
}

/// One volume of an instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeSpec {
    pub source: String,
    /// Static sources: the directory relative to the source root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Dynamic sources: the requested size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gib: Option<u32>,
    /// Mounts this volume read-only everywhere.
    #[serde(default)]
    pub read_only: bool,
}

pub const MAX_VOLUME_GIB: u32 = 1_048_576;

impl VolumeSpec {
    /// Checks the volume against its source's kind.
    pub fn validate(&self, sources: &StorageSources, at: &str, problems: &mut Problems) {
        let Some(source) = sources.get(&self.source) else {
            problems.add(format!("{at}/source"), "unknown_storage_source");
            return;
        };
        if source.dynamic() {
            let size_ok = self
                .size_gib
                .is_some_and(|size| (1..=MAX_VOLUME_GIB).contains(&size));
            problems.check(size_ok, format!("{at}/sizeGib"), "out_of_range");
            problems.check(self.path.is_none(), format!("{at}/path"), "not_allowed");
        } else {
            let path_ok = self
                .path
                .as_deref()
                .is_some_and(|path| validate_relative_path(path).is_ok());
            problems.check(path_ok, format!("{at}/path"), "invalid_path");
            problems.check(
                self.size_gib.is_none(),
                format!("{at}/sizeGib"),
                "not_allowed",
            );
        }
    }
}

/// Paths inside the Hermes container that no volume may cover.
pub const HERMES_RESERVED: &[&str] = &[
    "/opt/data",
    "/opt/hermes",
    "/run",
    "/var/run",
    "/tmp",
    "/proc",
    "/sys",
    "/dev",
    "/etc",
    "/usr",
    "/bin",
    "/lib",
    "/sbin",
];

/// Paths inside terminal backend containers that no volume may cover.
pub const WORKER_RESERVED: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/etc",
    "/run",
    "/var/run",
    "/usr",
    "/bin",
    "/lib",
    "/sbin",
    "/tmp",
    "/home/worker/.ssh",
    "/home/worker/hostkeys",
    "/home/worker/.config",
    "/opt/agent-platform",
];

/// Prefix of the platform's own Pod volume and claim names.
pub const RESERVED_VOLUME_PREFIX: &str = "hermes-";

/// A volume name is a DNS label outside the platform's prefix: it names the
/// PersistentVolumeClaim and the Pod volume.
pub fn volume_name_problem(name: &str) -> Option<&'static str> {
    if validate_name(name).is_err() {
        Some("invalid_name")
    } else if name.starts_with(RESERVED_VOLUME_PREFIX) {
        Some("reserved_name")
    } else {
        None
    }
}

/// Checks mount paths: absolute, clean, outside `reserved`, not overlapping
/// each other. `at` points at the mount map; `paths` maps volume names to paths.
pub fn validate_mount_paths<'a>(
    paths: impl IntoIterator<Item = (&'a String, &'a String)>,
    reserved: &[&str],
    at: &str,
    problems: &mut Problems,
) {
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for (name, path) in paths {
        let pointer = format!("{at}/{}", pointer_segment(name));
        if let Some(code) = volume_name_problem(name) {
            problems.add(&pointer, code);
        }
        let path_pointer = format!("{pointer}/mountPath");
        if validate_absolute_path(path).is_err() {
            problems.add(path_pointer, "invalid_path");
            continue;
        }
        if reserved.iter().any(|r| paths_overlap(path, r)) {
            problems.add(path_pointer, "reserved_path");
        } else if seen.iter().any(|(_, other)| paths_overlap(path, other)) {
            problems.add(path_pointer, "overlapping_path");
        }
        seen.push((name, path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            StorageSource {
                id: "host-data".into(),
                kind: SourceKind::LocalPath {
                    node: "server-1".into(),
                    root: "/srv/hermes-data".into(),
                },
            },
        ])
        .unwrap()
    }

    fn check(volume: &VolumeSpec) -> Vec<&'static str> {
        let mut problems = Problems::default();
        volume.validate(&sources(), "/volumes/x", &mut problems);
        problems.into_vec().into_iter().map(|p| p.code).collect()
    }

    #[test]
    fn volumes_match_their_source_kind() {
        let dynamic = VolumeSpec {
            source: "nvme".into(),
            path: None,
            size_gib: Some(20),
            read_only: false,
        };
        let fixed = VolumeSpec {
            source: "nas".into(),
            path: Some("bob".into()),
            size_gib: None,
            read_only: false,
        };
        assert!(check(&dynamic).is_empty());
        assert!(check(&fixed).is_empty());
        assert_eq!(
            check(&VolumeSpec {
                size_gib: None,
                ..dynamic.clone()
            }),
            ["out_of_range"]
        );
        assert_eq!(
            check(&VolumeSpec {
                path: Some("x".into()),
                ..dynamic.clone()
            }),
            ["not_allowed"]
        );
        assert_eq!(
            check(&VolumeSpec {
                path: Some("../alice".into()),
                ..fixed.clone()
            }),
            ["invalid_path"]
        );
        assert_eq!(
            check(&VolumeSpec {
                path: Some("/bob".into()),
                ..fixed.clone()
            }),
            ["invalid_path"]
        );
        assert_eq!(
            check(&VolumeSpec {
                source: "elsewhere".into(),
                ..fixed
            }),
            ["unknown_storage_source"]
        );
    }

    #[test]
    fn mount_paths_avoid_reserved_and_each_other() {
        let mounts: BTreeMap<String, String> = [
            ("workspace", "/workspace"),
            ("nested", "/workspace/inner"),
            ("home", "/opt/data/home"),
            ("bad", "relative"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let mut problems = Problems::default();
        validate_mount_paths(&mounts, HERMES_RESERVED, "/mounts", &mut problems);
        let found: Vec<_> = problems
            .into_vec()
            .into_iter()
            .map(|p| (p.pointer, p.code))
            .collect();
        assert_eq!(
            found,
            [
                ("/mounts/bad/mountPath".to_owned(), "invalid_path"),
                ("/mounts/home/mountPath".to_owned(), "reserved_path"),
                ("/mounts/workspace/mountPath".to_owned(), "overlapping_path"),
            ]
        );
    }

    #[test]
    fn sources_are_validated_and_unique() {
        let duplicate = StorageSources::new(vec![
            StorageSource {
                id: "nvme".into(),
                kind: SourceKind::StorageClass { name: "a".into() },
            },
            StorageSource {
                id: "nvme".into(),
                kind: SourceKind::LocalPath {
                    node: "Bad_Node".into(),
                    root: "relative".into(),
                },
            },
        ]);
        let codes: Vec<_> = duplicate.unwrap_err().into_iter().map(|p| p.code).collect();
        assert_eq!(codes, ["invalid_name", "invalid_path", "duplicate_source"]);
    }
}
