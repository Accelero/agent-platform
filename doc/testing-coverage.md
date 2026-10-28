# Test coverage assessment

Review baseline: source commit `5a05a59`. This is an assessment of test code,
fixture mechanics, contracts and CI configuration, not a line/branch coverage
measurement. The preceding local validation passed 106 collected Python cases,
the native owner/runtime checks, release smoke, Rust architecture/traceability,
the Kubernetes adapter/controller-process suite and the full native system run
including restore. That system run used direct k3s, not k3d or pytest E2E.
Those results are useful evidence for that run, not proof of every variant below.

The [strategy](testing-strategy.md) defines the required system. Gap IDs refer to
the actionable [gap register](testing-gaps.md). Existing `pass` labels in
[`TESTS.md`](../services/hermes-manager/TESTS.md) mean that cited tests exist and
exercise examples; they do not establish exhaustive clause/status/boundary
coverage or that every cited function was executed in a particular CI profile.

## Existing suites and what their evidence means

| Source | Current evidence | Limitation |
| --- | --- | --- |
| [Python tests](../tests/) and [pytest configuration](../pyproject.toml) | 106 cases in the preceding run: release/privacy/build invariants, contracts/chart rendering, runtime initialization, terminal policy and plugin selection | Native/system scripts are not pytest test modules and are outside this collection. No layer/capability marker scheme or common cluster fixtures |
| [Core domain](../services/hermes-manager/crates/hermes-core/src/domain/) | Typed state/validation tests and a broad finite desired/observed planner enumeration | No `proptest`, fuzz or mutation setup is present; no measured branch-coverage report. The finite planner test is valuable, but is not a generated event-sequence test |
| [Shared store cases](../services/hermes-manager/crates/hermes-core/src/app/testing/mod.rs) | 44 cases instantiated by the contract macro for memory and [PostgreSQL](../services/hermes-manager/crates/hermes-store/src/testing.rs): replay, races, retention, leases, controller and route rules | This is strong store/application coverage. It is not a reusable conformance suite for every external port, nor proof of process/cluster behaviour for every case |
| [HTTP integrations](../services/hermes-manager/crates/hermes-http/tests/) and [contract checker](../services/hermes-manager/crates/hermes-testkit/src/contract.rs) | Real TLS listeners, PostgreSQL, endpoint success/refusal cases and JSON response-schema checks | No complete operation × status × actor/header/body-case execution ledger. The checker validates status/body, not every header rule or runtime request schema |
| [Identity](../services/hermes-manager/crates/hermes-identity/tests/), [proxy](../services/hermes-manager/crates/hermes-proxy/tests/gateway.rs), [gateway binary](../services/hermes-manager/crates/hermes-gateway/tests/gateway.rs) | Signed JWTs, callback/session/refresh denial, wrong claims and keys, route/cookie/header hygiene, streaming, accelerated revocation and compiled gateway flows | Many use fake OIDC/native/resolver services. They are integrations, not pure units or substitutes for the full real-Keycloak/browser matrix |
| [Kubernetes driver](../tests/personal_hermes/kubernetes.py) and [adapter tests](../services/hermes-manager/crates/hermes-kube/tests/cluster.rs) | Seven real API tests plus controller-process abort/recovery; scoped ServiceAccount, admission, idempotent apply, stale deletion, leader election, actual SSH worker and image-pull failure | Direct single-server k3s is the CI default. An optional k3d path exists, but is not the standard CI path or the restore implementation. No systematic live cross-owner traffic matrix |
| [Native owner](../tests/personal_hermes/native_owner_payload.py), [runtime](../tests/personal_hermes/native_runtime.py), [initializer tests](../tests/test_personal_init.py) | Exact owner login/session/refresh, missing-policy refusal, non-root startup, forced restart, retained settings, concurrent/interrupted initialization and symlink/special-file refusal | Docker/in-image checks are partly standalone scripts with aggregate pass output. Not all malformed-policy/startup combinations are tested in the installed system |
| [System scenario](../tests/personal_hermes/native_e2e.py) | Real chart/operator Jobs, real native chat/cron/uploads, two owners, private registry, suspension, outages and second-cluster restore | One sequential script of more than 3,000 lines. Early failure prevents later scenarios; shared mutations and global DNS patching; no individual pytest node IDs or per-scenario JUnit |
| [UI browser fixtures](../services/hermes-manager/ui/e2e/fixtures.ts) and [production browser case](../services/hermes-manager/ui/e2e/production/assets.spec.ts) | Presentation workflows with mocked APIs; separate real-server CSP, write-only secret and template publication case | The production case injects cookies created by a Rust fixture and ignores browser HTTPS errors. It does not perform a real browser Keycloak login or commission an instance through the cluster dashboard |
| [CI checks](../.github/workflows/checks.yml) and [release](../.github/workflows/release.yml) | PR/main jobs run native language suites, image/runtime checks, direct-k3s integrations and native system script; release runs Python/preflight/build smoke | No scheduled extended topology/soak lane or aggregate execution-coverage gate. Tag publication does not explicitly require the full Rust/cluster result for the same candidate |

## Requirement-by-requirement assessment

“Partial” below identifies unproven variants of an otherwise exercised
requirement; it does not assert that the implementation is defective. Evidence
names are representative proving cases, not a second copy of `TESTS.md`.

| Requirement | Existing proving evidence | Remaining coverage obligation | Gaps |
| --- | --- | --- | --- |
| R1 — identity and PostgreSQL authority | `identity_keys_are_exact_and_email_is_not_a_key`, signed-token tests, `startup_check_accepts_only_the_registered_installation`, outage scenario | Actual database durability-settings refusal, restricted runtime roles and broader dependency recovery; provider/key rotation in the installed system | G10, G15, G18 |
| R2 — explicit enrollment only | `people_without_a_grant_reach_no_administration_endpoint`, `unassigned_and_foreign_identities_are_denied_without_side_effects`, real unassigned login denial | Parameterized actor × mutation matrix with negative side-effect snapshots, including directory/group changes and browser workflows | G03, G13, G21 |
| R3 — exactly one instance | Shared `concurrent_enables_for_one_identity_commission_one_instance` and same-key races; controller abort tests | Real concurrent public API commands and two controller processes during leader turnover, with stable namespace/material/worker identities | G10, G18 |
| R4 — independent owner state/credentials | `material_opens_only_for_its_instance_and_purpose`, real two-owner logins, distinct secret hashes and isolated uploads | Native history/profile/session/model-onboarding separation after crashes/restores; clarify intentionally shared referenced admin-provider keys versus per-owner native credentials | G12–G14, G17 |
| R5 — verified owner routing/no fallback | Store route cases, resolver mTLS negatives, proxy hygiene/cookie cases, foreign native tickets | Live reachability from another owner/unrelated Pod, warm-cache deadline cases, same paths across multiple API/gateway replicas | G06, G07, G15, G18 |
| R6 — native workflows and owner pin | Native login/session/refresh checks; system chat/settings/uploads/WebSocket/logout; production dashboard CSP case | Real browser journeys through Keycloak and native UI; history/session persistence, representative native HTTP mutations, hostile rendered content and cancellation | G12, G13, G21 |
| R7 — dashboard/API administration parity | Broad API workflows, mocked presentation workflows, real-server secret/template browser case | Dashboard-driven enrollment, update/preview/rollout/grant/revoke and error recovery against the installed API; enumerate browser/API parity explicitly | G03, G13, G21 |
| R8 — never overwrite/reseed | Initializer crash/concurrency/symlink cases, retained profile/memory tests, runtime restart and setting preservation | Real mounted home through runtime/template update, override removal, image change, interrupted init Pod and restored private-home contents | G12, G16, G17, G20 |
| R9 — revoke/suspend bounds and retention | Shared retained-state cases, real suspension closes WebSocket and drains pending worker, re-enable checks | Full explicit revoke as well as suspend; exact 5 s/10 s timing; active non-terminating worker forced by 5 min; volume/key preservation after Helm uninstall | G06, G11, G16 |
| R10 — settings precedence | `overrides_merge_leaf_by_leaf`, protected-key rejection, runtime/profile policy tests, system model/admin-secret precedence | Installed template/assignment/user precedence matrix, removing an override exposes retained user value, malformed/absent mounted policy and image-plugin substitution | G09, G12, G14 |
| R11 — selective write-only secrets | Shared secret reference/kind/version/retention cases; real env-model key and registry secret; API/audit/log assertions | Installed file-delivery and selective rotation/removal matrix, wrong/missing sealing keys and redaction during failures, browser/test artifacts included | G14, G15, G20 |
| R12 — allowed execution only | Shared acquisition/replay/expiry/parent/limit cases, SSH readiness, plugin request schemas, real allowed/denied/default/custom/private backends | Real three-mode/profile/session matrix; races at revoke/expiry/release/parent join; active volume change; worker death, command non-replay and forced drain | G09–G11, G18, G21 |
| R13 — approved isolated storage | Validation/overlap races, renderer/mount tests, selected API admission negatives, missing static directory system case | Live NFS, read-only enforcement and unreadable/replaced path cases; full PV/admission mutation matrix; direct cross-owner filesystem/network attempts | G07, G08, G16 |
| R14 — shared paths and UID/GID | `bob_gets_a_shared_workspace_and_his_network_home`, mismatch rejection, real shared `/workspace` files and a backend-only fixture home | “Network home” in the system fixture uses local static storage, not a live NFS export; multi-node mount/scheduling and group/permission variants | G02, G16 |
| R15 — scoped controller authority | Chart token/RBAC comparisons, selected real admission denials, `can-i` checks, kubeconfig exec-plugin refusal | Independently valid/invalid cases per security field, all container kinds and write verbs; spoofed ownership labels, foreign namespaces/PVs; live packet matrix | G07, G08 |
| R16 — one continuous replica per owner and independent workers | Pure rollout/readiness tests, real no-browser terminal/cron, worker idle retirement | Recreate overlap under concurrent changes, node/worker loss while Hermes stays healthy, all lifetime modes and prolonged gateway/cron liveness | G11, G18, G19 |
| R17 — durable crash recovery | 44 shared cases, SQL guard tests, API commit-boundary aborts, five real controller fault locations, planner totality | Worker/lease process failures and competing leaders, database crash/commit ambiguity, dropped notifications, bounded Kubernetes failures and stale observations | G09–G11, G18 |
| R18 — no unintended secret exposure | Privacy allowlists and rejection tests; sealed values, write-only API/audit cases, generated secret scan of app logs | All secret delivery/rotation/error paths; built artifacts and public layer contents; reports/screenshots/traces/driver exceptions; access using restricted DB roles | G10, G14, G20, G21 |
| R19 — restore without adoption | New-cluster rebind restores SQL and a static workspace, reuses the sealing key and permits login; restored older DB reports new namespaces/PVs as orphans without adoption | Restored private-home profiles/memory/sessions/native state and explicit before/after credential digests; successful same-cluster known-instance recovery; missing/wrong keys and partial backups; provisioner-specific dynamic-data evidence | G16, G17 |

## Acceptance scenario accounting

All 23 acceptance rows have entries in `TESTS.md`. This table identifies their
main present proving boundary and the additional assertion or variant needed.
U = native unit/shared fake; I = real dependency/process integration;
E = current sequential system script, not pytest E2E or k3d evidence.

| Specification scenario | Present evidence | Additional case or test-system work |
| --- | --- | --- |
| Two assigned identities, one URL | I/E owner login, settings and uploads | Native history/session/profile isolation and real browser contexts (G12, G13) |
| Same path, different verified subjects | U/I/E route and header isolation | Cross-owner live service/Pod access and multi-replica cache isolation (G07, G18) |
| Valid but unassigned identity | U/I/E no enrollment | Independent pytest case with before/after resource inventory (G01, G03) |
| Concurrent enable, retry and controller kill | U/I store races and real aborts | Public API concurrency and competing controller processes together (G10, G18) |
| Email change or new subject with an old email | U/E real directory changes | Independent browser case; exact claim/case variants retained (G01, G13, G15) |
| Forged headers, foreign cookie or ticket, callback race | I/E headers, cookies, tickets; identity callback races | Native/browser and cluster-negative matrix with observable zero backend hits (G07, G13, G15) |
| Native HTTP, upload and WebSocket through the gateway | I/E chat, upload, settings, ticket and logout | History, other representative mutations, exact byte edges, cancellation and long idle (G13, G19, G21) |
| Manager, database or Keycloak outage | I/E real scale/pause/provider disable | Correct timing origins/thresholds, abrupt failures, stalled dependencies and recovery state (G06, G10, G18) |
| Suspend during an open chat and during provisioning | U/I/E real suspend | Exact close/new-request bounds and active worker forced-drain (G06, G11) |
| Re-enable after revoke | U handles revoke; E suspends then re-enables | Explicit real revoke/re-enable case with retained data/key hashes (G11, G16) |
| Admin setting and admin secret | U/I/E precedence, env and registry | Override removal, file delivery and selective rotation/error cases (G12, G14) |
| Bob's example assignment | U/I/E paths, workspace and backend-only local home | Real NFS-backed network home and cross-node placement (G16) |
| Missing static volume directory | U/I/E blocks then recovers | Unreadable/replaced directory, read-only and NFS-unreachable variants (G16) |
| User requests a disallowed backend or edits profile YAML | U/I/E selection and managed policy | New/deleted/renamed profiles, all modes and missing-policy installed Pod (G11, G12) |
| Custom backend image and private registry | I/E pull failure then success; SSH contract probes | Installed incompatible/start-crashing images, rotation while leased, all nodes (G02, G11, G14) |
| Backend or volume change during an active lease | U/E backend version change | Real volume/read-only change while leased; new requests use new binding (G11, G16) |
| Revoke during terminal acquisition | U/I credentials; E suspension | Actual revoke racing acquire/heartbeat, persisted absence of late grants (G10, G11) |
| Hermes with no browser traffic | E native cron and direct plugin acquisition | Longer native gateway/cron run and reconnect after outages (G19) |
| Idle terminal cleanup | U/E idle retirement | All modes, active lease protection and bounded reclamation under load (G11, G19) |
| Kill the manager at every injected fault point | U/I API/controller aborts | Explicit fault-location-to-collected-case inventory; worker/lease boundaries (G03, G10, G18) |
| Execution credential used against another interface or instance | I/E denial and exact-instance authentication | Table-driven wrong-interface/owner/expired/suspended cases and zero effects (G11, G21) |
| Restore PostgreSQL and data on a new cluster | E static workspace, IDs/material and orphans | Private home recovery, negative backups/key inputs and standard k3d restore fixture (G02, G17) |
| UI build and dependency boundary | U/I architecture, assets, CSP and privacy | Real browser enrollment under cluster deployment, artifact-content verification and candidate gating (G13, G20) |

## Operational bounds

These are the nine rows in the specification, not new performance promises.
Some existing checks are accelerated or prove an example rather than the exact
production boundary. G06 owns their measurement contract.

| Bound | Present evidence | Missing precise evidence |
| --- | --- | --- |
| Route resolution: 2 s | `a_stalled_registry_is_unavailable_within_the_deadline` and configured deadlines | End-to-end hung resolver/TLS/DNS variants with timeout cleanup and no upstream fallback |
| Authorization: 5 s decisions; 10 s open connections | Accelerated proxy checks; system suspension and outage checks | Suspension waits 6 s before an HTTP denial assertion and accepts WebSocket close under 12 s. Outage helper uses 8 s/12 s after some shutdown setup. These do not establish the specified 5 s/10 s boundaries |
| Identity evidence: refresh at most 60 s; reject older than 120 s | Token maximum-age checks and real provider outage/revocation | Exact refresh cadence and just-before/at/after freshness under a warmed cache, restart and multiple gateways |
| Provider revocation: 130 s | Real provider disable/outage checks | Start at provider acknowledgement, preserve event timestamps, prove no fresh access/reconnect after expiry |
| Uploads: 64 MiB or lower | `uploads_are_bounded_and_bodies_reach_the_instance` tests a reduced fixture limit; real small upload | Limit − 1, exact limit, limit + 1 at configured and default bounds; streamed/chunked upload without length; cancellation without cross-owner partial effects |
| WebSocket idle: up to 1 h with heartbeats | Handshake/byte forwarding and short chat connections | Long-idle heartbeat soak, proxy reconnect/resource cleanup and concurrent revocation |
| Terminal drain: at most 5 min | Pure planner deadlines, shared lease cases, real pending-worker drain | Non-cooperating active command/lease at forced cutoff, all lifetime modes and preserved data |
| Calls: bounded deadlines/concurrency/backoff | Queue unit tests, selected HTTP/probe timeouts, explicit command timeouts | Systematic 429/5xx/stall/cancellation, lost notifications and bounded in-flight work during prolonged failure |
| Bodies: 16 KiB; admin config: 8 KiB | Strict JSON/schema and request-size rejection examples | Independent exact byte boundaries, Unicode/duplicate-key/deep JSON variants and required response/header semantics per interface |

## What cannot be concluded from the current green results

- No measured line/branch percentage or comprehensive mutation/fuzz result exists.
- No standard k3d run, multi-node acceptance run or live NFS test is established
  by the current CI commands.
- Generated manifests matching each other do not demonstrate live isolation.
- A traced function name does not prove collection, feature selection, execution,
  its asserted level, or complete coverage of a requirement's clauses.
- The static restore scenario does not establish recovery of every private-home
  storage backend. The specification already limits its dynamic-provisioner claim;
  keep that deployment-specific limit explicit rather than claiming universal recovery.
- Mocked UI workflows and an authenticated real-server browser case do not prove
  a complete real browser login-to-commissioning journey on the installed chart.

These are coverage and evidence limitations. The gap register identifies the
tests and refactors needed to resolve them without discarding the existing
working unit, shared-contract and integration suites.
