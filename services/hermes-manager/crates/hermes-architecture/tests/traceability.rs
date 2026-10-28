//! `TESTS.md` must cover every requirement, acceptance scenario and contract
//! operation and operational bound, and every test it names must exist.
//! Execution is checked separately by `tests.support.verification`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const STATUSES: &[&str] = &["pending", "partial", "pass", "manual"];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| panic!("read {}", path.display()))
}

/// Rows of the first Markdown table under `## {heading}`, without the header.
fn table(document: &str, heading: &str) -> Vec<Vec<String>> {
    let marker = format!("## {heading}\n");
    let start = document
        .find(&marker)
        .unwrap_or_else(|| panic!("section `{heading}`"));
    let section = &document[start + marker.len()..];
    let section = &section[..section.find("\n## ").unwrap_or(section.len())];
    section
        .lines()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|line| {
            line.trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

fn first_column(rows: &[Vec<String>]) -> BTreeSet<String> {
    rows.iter().map(|row| row[0].clone()).collect()
}

fn tests_md() -> String {
    read(&repository().join("services/hermes-manager/TESTS.md"))
}

fn specification() -> String {
    read(&repository().join("doc/SPEC.md"))
}

/// Contract, operation ID and `METHOD /path` together identify an operation.
type Operation = (String, String, String);

fn operations_in(contract: &str, document: &serde_yaml_ng::Value) -> BTreeSet<Operation> {
    let mut operations = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for (route, item) in document["paths"].as_mapping().expect("paths") {
        for (method, operation) in item.as_mapping().expect("path item") {
            let method = method.as_str().expect("method");
            if [
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ]
            .contains(&method)
            {
                let id = operation["operationId"].as_str().expect("operationId");
                assert!(!id.is_empty(), "{contract}: empty operationId");
                assert!(ids.insert(id), "{contract}: duplicate operationId `{id}`");
                operations.insert((
                    contract.to_owned(),
                    id.to_owned(),
                    format!(
                        "{} {}",
                        method.to_uppercase(),
                        route.as_str().expect("route")
                    ),
                ));
            }
        }
    }
    operations
}

fn contract_operations() -> BTreeSet<Operation> {
    let mut operations = BTreeSet::new();
    let contracts = repository().join("contracts/hermes-manager");
    for entry in std::fs::read_dir(&contracts).expect("contracts") {
        let path = entry.expect("entry").path();
        if !path.to_string_lossy().ends_with("openapi.yaml") {
            continue;
        }
        let document: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&read(&path)).expect("contract is YAML");
        operations.extend(operations_in(
            path.file_name().unwrap().to_str().unwrap(),
            &document,
        ));
    }
    operations
}

/// Function names defined in the rewrite's Rust sources and the repository's
/// Python tests.
fn defined_tests() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let roots = [
        (
            repository().join("services/hermes-manager/crates"),
            "rs",
            "fn ",
        ),
        (repository().join("tests"), "py", "def "),
    ];
    for (root, extension, keyword) in roots {
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("directory") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    if !path.ends_with("target") && !path.ends_with("__pycache__") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != extension) {
                    continue;
                }
                for line in read(&path).lines() {
                    if let Some(rest) = line.trim_start().split(keyword).nth(1) {
                        let name: String = rest
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        if !name.is_empty() {
                            names.insert(name);
                        }
                    }
                }
            }
        }
    }
    names
}

fn check_rows(heading: &str, rows: &[Vec<String>], tests_column: usize, status_column: usize) {
    let defined = defined_tests();
    for row in rows {
        let status = row
            .get(status_column)
            .unwrap_or_else(|| panic!("{heading}: `{}` has a status", row[0]));
        assert!(
            STATUSES.contains(&status.as_str()),
            "{heading}: `{}` has unknown status `{status}`",
            row[0]
        );
        let tests: Vec<_> = row[tests_column]
            .split(',')
            .map(|name| name.trim().trim_matches('`'))
            .filter(|name| !name.is_empty() && *name != "—")
            .collect();
        if matches!(status.as_str(), "pass" | "partial") {
            assert!(
                !tests.is_empty(),
                "{heading}: `{}` is {status} without tests",
                row[0]
            );
        }
        for name in tests {
            assert!(
                defined.contains(name),
                "{heading}: `{}` names missing test `{name}`",
                row[0]
            );
        }
    }
}

#[test]
fn every_requirement_has_a_traceability_row() {
    let expected = first_column(&table(&specification(), "Requirements"));
    let rows = table(&tests_md(), "Requirements");
    assert_eq!(first_column(&rows), expected);
    check_rows("Requirements", &rows, 2, 4);
}

#[test]
fn every_acceptance_scenario_has_a_traceability_row() {
    let expected = first_column(&table(&specification(), "Acceptance matrix"));
    let rows = table(&tests_md(), "Acceptance");
    assert_eq!(first_column(&rows), expected);
    check_rows("Acceptance", &rows, 1, 3);
}

#[test]
fn every_contract_operation_has_a_traceability_row() {
    let rows = table(&tests_md(), "Interfaces");
    let actual: BTreeSet<_> = rows
        .iter()
        .map(|row| {
            assert_eq!(row.len(), 5, "interface row has five columns");
            (
                row[1].trim_matches('`').to_owned(),
                row[2].trim_matches('`').to_owned(),
                row[0].trim_matches('`').to_owned(),
            )
        })
        .collect();
    assert_eq!(actual.len(), rows.len(), "duplicate interface rows");
    assert_eq!(actual, contract_operations());
    check_rows("Interfaces", &rows, 3, 4);
}

#[test]
fn every_operational_bound_has_a_traceability_row() {
    let expected = first_column(&table(&specification(), "Operational bounds"));
    let rows = table(&tests_md(), "Operational bounds");
    assert_eq!(
        first_column(&rows).len(),
        rows.len(),
        "duplicate bound rows"
    );
    assert_eq!(first_column(&rows), expected);
    check_rows("Operational bounds", &rows, 1, 2);
}

#[test]
fn matching_paths_in_different_contracts_remain_distinct() {
    let document =
        serde_yaml_ng::from_str("paths:\n  /auth/logout:\n    post:\n      operationId: logout\n")
            .unwrap();
    let mut operations = operations_in("openapi.yaml", &document);
    operations.extend(operations_in("gateway.openapi.yaml", &document));
    assert_eq!(operations.len(), 2);
    operations.remove(&(
        "openapi.yaml".into(),
        "logout".into(),
        "POST /auth/logout".into(),
    ));
    assert!(operations.contains(&(
        "gateway.openapi.yaml".into(),
        "logout".into(),
        "POST /auth/logout".into(),
    )));
}

#[test]
#[should_panic(expected = "duplicate operationId")]
fn duplicate_operation_ids_within_a_contract_are_rejected() {
    let document = serde_yaml_ng::from_str(
        "paths:\n  /one:\n    get:\n      operationId: repeated\n  /two:\n    get:\n      operationId: repeated\n",
    )
    .unwrap();
    operations_in("openapi.yaml", &document);
}
