//! Seeded random walks over administration commands, compared after every step
//! with a model of what the specification says each command must do. A failure
//! names its seed and step, so it replays exactly.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use uuid::Uuid;

use super::super::{Inspect, identity};
use super::access::{admin, bootstrap, envelope, key};
use super::assignments::bob_spec;
use super::catalog::published_catalog;
use crate::app::assignments::{
    AssignmentChanges, AssignmentReceipt, CreateAssignment, UpdateAssignment,
};
use crate::app::envelope::Command;
use crate::app::error::AppError;
use crate::app::ports::Store;
use crate::domain::ErrorKind;
use crate::domain::assignments::{Access, AssignmentSpec};

/// splitmix64: a reproducible stream of numbers from a seed.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).expect("bound")).expect("index")
    }
}

/// What the specification says is true of one assignment.
struct Model {
    receipt: AssignmentReceipt,
    access: Access,
    has_default: bool,
    desired: i64,
    execution: i64,
}

/// An accepted command that can be sent again with its key.
enum Sent {
    Create(CreateAssignment),
    Update(UpdateAssignment),
}

fn spec() -> AssignmentSpec {
    let mut spec = bob_spec();
    // Dynamic storage only, so owners never contend for a path.
    spec.volumes.remove("home");
    spec
}

pub async fn generated_command_walks_follow_the_model_and_record_only_accepted_commands<
    S: Store + Inspect + Clone,
>(
    store: S,
) {
    bootstrap(&store, "alice").await;
    published_catalog(&store).await;
    let base_audit = store.audit_actions().await.len();
    let base_receipts = store.receipt_count().await;
    let (create_action, update_action) = (
        <CreateAssignment as Command>::ACTION,
        <UpdateAssignment as Command>::ACTION,
    );
    let mut expected_actions = Vec::new();
    let mut accepted = 0_usize;
    // Every kind of step must really occur, or the walk proves nothing.
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();

    for seed in [11_u64, 2026, 90_210] {
        let mut generator = Generator(seed);
        let mut people: BTreeMap<String, Model> = BTreeMap::new();
        let mut sent: Vec<(String, Sent, AssignmentReceipt)> = Vec::new();
        for step in 0..50 {
            let at = format!("seed {seed} step {step}");
            let subject = format!("g{seed}-{}", generator.below(4));
            match generator.below(6) {
                // Enabling: new identities succeed, known ones conflict.
                0 => {
                    let command = CreateAssignment {
                        identity: identity(&subject),
                        spec: spec(),
                    };
                    let name = Uuid::new_v4().to_string();
                    let result = envelope(&store)
                        .execute(&admin("alice"), Uuid::new_v4(), &key(&name), &command)
                        .await;
                    match people.entry(subject) {
                        Entry::Occupied(_) => {
                            refused(result, ErrorKind::Conflict, "assignment_exists", &at);
                            *seen.entry("duplicate enable").or_default() += 1;
                        }
                        Entry::Vacant(vacant) => {
                            let receipt = result.unwrap_or_else(|error| panic!("{at}: {error:?}"));
                            assert_eq!(receipt.version, 1, "{at}");
                            vacant.insert(Model {
                                receipt: receipt.clone(),
                                access: Access::Enabled,
                                has_default: true,
                                desired: 1,
                                execution: 1,
                            });
                            sent.push((name, Sent::Create(command), receipt));
                            expected_actions.push(create_action);
                            accepted += 1;
                            *seen.entry("enable").or_default() += 1;
                        }
                    }
                }
                // Changing access with the current version: accepted, one
                // desired generation further, the same instance.
                1 | 2 => {
                    let Some(model) = people.get_mut(&subject) else {
                        continue;
                    };
                    let next =
                        [Access::Enabled, Access::Suspended, Access::Revoked][generator.below(3)];
                    if next == model.access {
                        continue;
                    }
                    let command = UpdateAssignment {
                        assignment_id: model.receipt.assignment_id,
                        expected_version: model.receipt.version,
                        changes: AssignmentChanges {
                            access: Some(next),
                            ..Default::default()
                        },
                    };
                    let name = Uuid::new_v4().to_string();
                    let receipt = envelope(&store)
                        .execute(&admin("alice"), Uuid::new_v4(), &key(&name), &command)
                        .await
                        .unwrap_or_else(|error| panic!("{at}: {error:?}"));
                    assert_eq!(
                        (receipt.version, receipt.instance_id),
                        (model.receipt.version + 1, model.receipt.instance_id),
                        "{at}: one version further, the same instance"
                    );
                    model.receipt = receipt.clone();
                    model.access = next;
                    model.desired += 1;
                    *seen.entry("access change").or_default() += 1;
                    sent.push((name, Sent::Update(command), receipt));
                    expected_actions.push(update_action);
                    accepted += 1;
                }
                // A terminal change moves the execution generation too.
                3 => {
                    let Some(model) = people.get_mut(&subject) else {
                        continue;
                    };
                    let mut terminal = spec().terminal;
                    if model.has_default {
                        terminal.default = None;
                    }
                    let command = UpdateAssignment {
                        assignment_id: model.receipt.assignment_id,
                        expected_version: model.receipt.version,
                        changes: AssignmentChanges {
                            terminal: Some(terminal),
                            ..Default::default()
                        },
                    };
                    let name = Uuid::new_v4().to_string();
                    let receipt = envelope(&store)
                        .execute(&admin("alice"), Uuid::new_v4(), &key(&name), &command)
                        .await
                        .unwrap_or_else(|error| panic!("{at}: {error:?}"));
                    model.receipt = receipt.clone();
                    model.has_default = !model.has_default;
                    model.desired += 1;
                    model.execution += 1;
                    *seen.entry("terminal change").or_default() += 1;
                    sent.push((name, Sent::Update(command), receipt));
                    expected_actions.push(update_action);
                    accepted += 1;
                }
                // A stale version is refused and changes nothing.
                4 => {
                    let Some(model) = people.get(&subject) else {
                        continue;
                    };
                    let wrong = if generator.below(2) == 0 || model.receipt.version == 1 {
                        model.receipt.version + 1
                    } else {
                        model.receipt.version - 1
                    };
                    let result = envelope(&store)
                        .execute(
                            &admin("alice"),
                            Uuid::new_v4(),
                            &key(&Uuid::new_v4().to_string()),
                            &UpdateAssignment {
                                assignment_id: model.receipt.assignment_id,
                                expected_version: wrong,
                                changes: AssignmentChanges::default(),
                            },
                        )
                        .await;
                    refused(result, ErrorKind::Precondition, "stale_version", &at);
                    *seen.entry("stale version").or_default() += 1;
                }
                // Repeating an accepted command with its key returns its
                // recorded result; the same key with another body conflicts.
                _ => {
                    if sent.is_empty() {
                        continue;
                    }
                    let (name, command, receipt) = &sent[generator.below(sent.len())];
                    let replay = match command {
                        Sent::Create(command) => {
                            envelope(&store)
                                .execute(&admin("alice"), Uuid::new_v4(), &key(name), command)
                                .await
                        }
                        Sent::Update(command) => {
                            envelope(&store)
                                .execute(&admin("alice"), Uuid::new_v4(), &key(name), command)
                                .await
                        }
                    };
                    assert_eq!(replay.as_ref(), Ok(receipt), "{at}: replay");
                    // Keys are scoped to the actor and the action (the receipt
                    // key includes it), so the conflicting body is another
                    // body of the same command.
                    let conflicting = match command {
                        Sent::Create(_) => {
                            envelope(&store)
                                .execute(
                                    &admin("alice"),
                                    Uuid::new_v4(),
                                    &key(name),
                                    &CreateAssignment {
                                        identity: identity(&format!("g{seed}-other-{step}")),
                                        spec: spec(),
                                    },
                                )
                                .await
                        }
                        Sent::Update(sent) => {
                            envelope(&store)
                                .execute(
                                    &admin("alice"),
                                    Uuid::new_v4(),
                                    &key(name),
                                    &UpdateAssignment {
                                        expected_version: sent.expected_version + 1000,
                                        ..sent.clone()
                                    },
                                )
                                .await
                        }
                    };
                    assert_eq!(
                        conflicting,
                        Err(AppError::idempotency_conflict()),
                        "{at}: conflict"
                    );
                    *seen.entry("replay and conflict").or_default() += 1;
                }
            }

            // After every step: only accepted commands are on record, and each
            // known instance has exactly the generations the model predicts.
            assert_eq!(
                store.audit_actions().await.len(),
                base_audit + accepted,
                "{at}: audit"
            );
            assert_eq!(
                store.receipt_count().await,
                base_receipts + accepted,
                "{at}: receipts"
            );
            for (who, model) in &people {
                assert_eq!(
                    store.instance_generations(model.receipt.instance_id).await,
                    (model.desired, model.execution),
                    "{at}: generations of {who}"
                );
            }
        }
        let instances: std::collections::BTreeSet<_> =
            people.values().map(|m| m.receipt.instance_id).collect();
        assert_eq!(
            instances.len(),
            people.len(),
            "seed {seed}: one instance each"
        );
    }
    for step in [
        "enable",
        "duplicate enable",
        "access change",
        "terminal change",
        "stale version",
        "replay and conflict",
    ] {
        assert!(seen.contains_key(step), "the walks never produced: {step}");
    }
    let actions = store.audit_actions().await;
    assert_eq!(&actions[base_audit..], expected_actions.as_slice());
}

fn refused<T: std::fmt::Debug>(
    result: Result<T, AppError>,
    kind: ErrorKind,
    code: &'static str,
    at: &str,
) {
    let error = result.expect_err(at);
    assert_eq!((error.kind, error.code), (kind, code), "{at}");
}
