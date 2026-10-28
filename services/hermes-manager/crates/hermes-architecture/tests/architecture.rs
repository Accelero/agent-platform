//! Crate dependency rules from `doc/ARCH.md`.
//!
//! Dependencies mirror runtime privilege: the core performs no I/O, adapters do
//! not depend on each other, and the gateway cannot reach the registry store or
//! a Kubernetes client even transitively.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Allowed workspace-internal edges. Anything not listed is a violation.
const INTERNAL: &[(&str, &[&str])] = &[
    ("hermes-core", &[]),
    ("hermes-wire", &["hermes-core"]),
    ("hermes-store", &["hermes-core"]),
    ("hermes-render", &["hermes-core"]),
    ("hermes-kube", &["hermes-core", "hermes-render"]),
    ("hermes-identity", &["hermes-core"]),
    ("hermes-native", &["hermes-core"]),
    (
        "hermes-http",
        &["hermes-core", "hermes-wire", "hermes-identity"],
    ),
    (
        "hermes-proxy",
        &["hermes-core", "hermes-wire", "hermes-identity"],
    ),
    (
        "hermes-manager",
        &[
            "hermes-core",
            "hermes-wire",
            "hermes-store",
            "hermes-render",
            "hermes-kube",
            "hermes-identity",
            "hermes-native",
            "hermes-http",
        ],
    ),
    (
        "hermes-gateway",
        &[
            "hermes-core",
            "hermes-wire",
            "hermes-identity",
            "hermes-http",
            "hermes-proxy",
        ],
    ),
    ("hermes-architecture", &[]),
    ("hermes-testkit", &[]),
];

/// The only external crates the core may use directly.
const CORE_EXTERNAL: &[&str] = &[
    "async-trait",
    "serde",
    "serde_json",
    "sha2",
    "thiserror",
    "url",
    "uuid",
    "zeroize",
];

const IO: &[&str] = &[
    "axum",
    "hyper",
    "k8s-openapi",
    "kube",
    "openidconnect",
    "reqwest",
    "rustls",
    "sqlx",
    "tokio",
];
const CLUSTER_AND_REGISTRY: &[&str] = &[
    "hermes-kube",
    "hermes-render",
    "hermes-store",
    "k8s-openapi",
    "kube",
];

/// Packages that must not appear anywhere in a crate's normal dependency closure.
fn forbidden_transitive() -> BTreeMap<&'static str, Vec<&'static str>> {
    let mut rules = BTreeMap::new();
    rules.insert("hermes-core", IO.to_vec());
    rules.insert("hermes-wire", IO.to_vec());
    rules.insert(
        "hermes-render",
        vec!["kube", "reqwest", "sqlx", "tokio", "hermes-store"],
    );
    rules.insert(
        "hermes-store",
        vec!["axum", "k8s-openapi", "kube", "reqwest"],
    );
    rules.insert("hermes-identity", CLUSTER_AND_REGISTRY.to_vec());
    rules.insert(
        "hermes-native",
        vec!["hermes-store", "k8s-openapi", "kube", "sqlx"],
    );
    // Session storage in hermes-identity uses SQLx; the registry store stays out.
    rules.insert("hermes-http", CLUSTER_AND_REGISTRY.to_vec());
    rules.insert("hermes-proxy", CLUSTER_AND_REGISTRY.to_vec());
    rules.insert("hermes-gateway", CLUSTER_AND_REGISTRY.to_vec());
    rules
}

struct Graph {
    names: BTreeMap<String, String>,
    normal: BTreeMap<String, BTreeSet<String>>,
    members: BTreeSet<String>,
}

fn metadata() -> Graph {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--manifest-path",
        ])
        .arg(workspace.join("Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(output.status.success(), "cargo metadata failed");
    let value: Value = serde_json::from_slice(&output.stdout).expect("metadata is JSON");
    let names = value["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let members = value["workspace_members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    let mut normal = BTreeMap::new();
    for node in value["resolve"]["nodes"].as_array().expect("resolve") {
        let deps = node["deps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|dep| {
                dep["dep_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|kind| kind["kind"].is_null())
            })
            .map(|dep| dep["pkg"].as_str().unwrap().to_owned())
            .collect();
        normal.insert(node["id"].as_str().unwrap().to_owned(), deps);
    }
    Graph {
        names,
        normal,
        members,
    }
}

impl Graph {
    fn id(&self, name: &str) -> &str {
        self.members
            .iter()
            .find(|id| self.names[*id] == name)
            .unwrap_or_else(|| panic!("{name} is a workspace member"))
    }

    fn direct(&self, name: &str) -> BTreeSet<&str> {
        self.normal[self.id(name)]
            .iter()
            .map(|id| self.names[id].as_str())
            .collect()
    }

    fn closure(&self, name: &str) -> BTreeSet<&str> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![self.id(name).to_owned()];
        while let Some(id) = stack.pop() {
            for dep in &self.normal[&id] {
                if seen.insert(self.names[dep].as_str()) {
                    stack.push(dep.clone());
                }
            }
        }
        seen
    }
}

#[test]
fn every_workspace_crate_has_an_internal_dependency_rule() {
    let graph = metadata();
    let ruled: BTreeSet<_> = INTERNAL.iter().map(|(name, _)| *name).collect();
    let members: BTreeSet<_> = graph
        .members
        .iter()
        .map(|id| graph.names[id].as_str())
        .collect();
    assert_eq!(ruled, members);
}

#[test]
fn internal_dependencies_follow_the_layering() {
    let graph = metadata();
    for (name, allowed) in INTERNAL {
        let internal: BTreeSet<_> = graph
            .direct(name)
            .into_iter()
            .filter(|dep| dep.starts_with("hermes-"))
            .collect();
        let extra: Vec<_> = internal
            .difference(&allowed.iter().copied().collect())
            .copied()
            .collect();
        assert!(extra.is_empty(), "{name} must not depend on {extra:?}");
    }
}

#[test]
fn core_uses_only_reviewed_external_crates() {
    let graph = metadata();
    let extra: Vec<_> = graph
        .direct("hermes-core")
        .into_iter()
        .filter(|dep| !CORE_EXTERNAL.contains(dep))
        .collect();
    assert!(extra.is_empty(), "hermes-core must not depend on {extra:?}");
}

#[test]
fn privileged_and_io_crates_stay_out_of_restricted_closures() {
    let graph = metadata();
    for (name, forbidden) in forbidden_transitive() {
        let closure = graph.closure(name);
        let found: Vec<_> = forbidden
            .iter()
            .filter(|dep| closure.contains(*dep))
            .collect();
        assert!(found.is_empty(), "{name} reaches {found:?}");
    }
}

#[test]
fn domain_modules_are_synchronous_and_perform_no_io() {
    let domain = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hermes-core/src/domain");
    let mut stack = vec![domain];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("domain directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("source");
            for banned in [
                "async fn",
                ".await",
                "std::fs",
                "std::net",
                "SystemTime::now",
                "Instant::now",
                "std::env",
            ] {
                assert!(
                    !source.contains(banned),
                    "{} contains `{banned}`",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn test_fixtures_are_never_shipped() {
    let graph = metadata();
    for (name, _) in INTERNAL {
        if *name != "hermes-testkit" {
            assert!(
                !graph.closure(name).contains("hermes-testkit"),
                "{name} depends on hermes-testkit outside tests"
            );
        }
    }
}
