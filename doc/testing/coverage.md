# Test coverage assessment

This assessment compares test code, fixture mechanics, contracts and CI
configuration. It is not a line/branch coverage measurement or a passing receipt
for this checkout. Actual outcomes belong to the verifier's per-run JSON/JUnit
reports; a cited helper or collected case does not establish that it ran.

The [execution ledger](../../tests/verification/README.md) checks exact
Python/Rust collection, proving levels and per-case outcomes in dedicated
profiles. It includes compiled shared-store cases, ignored/feature-gated Rust
cases, and the independent pytest cluster/destructive scenarios. Helpers without
collected wrappers remain explicitly uncollected. Composite operation IDs and
all nine operational bounds are statically checked. Full clause/status/actor
coverage and TypeScript execution inventories remain incomplete.

[TESTING.md](../TESTING.md) describes the implemented runners, fixtures and CI
selection. Gap IDs refer to the actionable [gap register](gaps.md). Existing
`pass` labels in [`TESTS.md`](../../services/hermes-manager/TESTS.md) identify
cited examples; they do not establish exhaustive coverage or execution in a
particular candidate's profile.

## Existing suites and what their evidence means

| Source | Current evidence | Limitation |
| --- | --- | --- |
| [Python tests](../../tests/) and [pytest configuration](../../pyproject.toml) | Release/privacy/build invariants including the tag/version rule, contracts/chart rendering, runtime initialization (with a 68-case malformed-input matrix), terminal policy and plugin selection, direct-dependency and ownership/cleanup checks for the harness, the installed Hermes image as 13 collected `python-native` cases, plus collection/reporting regressions; explicit levels, registered capability markers, a plain run that needs no infrastructure and strict configuration | Independent pytest E2E cases share k3d/platform fixtures alongside the standalone native script; complete requirement/case metadata and exhaustive variant coverage are absent |
| [Core domain](../../services/hermes-manager/crates/hermes-core/src/domain/) | Typed state/validation tests and a broad finite desired/observed planner enumeration | No `proptest`, fuzz or mutation setup is present; no measured branch-coverage report. The finite planner test is valuable, but is not a generated event-sequence test |
| [Shared store cases](../../services/hermes-manager/crates/hermes-core/src/app/testing/mod.rs) | Shared cases instantiated by the contract macro for memory and [PostgreSQL](../../services/hermes-manager/crates/hermes-store/src/testing.rs): replay, races, retention, leases, controller and route rules | This is strong store/application coverage. It is not a reusable conformance suite for every external port, nor proof of process/cluster behaviour for every case |
| [HTTP integrations](../../services/hermes-manager/crates/hermes-http/tests/), [raw wire client](../../services/hermes-manager/crates/hermes-testkit/src/raw.rs) and [contract checker](../../services/hermes-manager/crates/hermes-testkit/src/contract.rs) | Real TLS listeners, PostgreSQL, endpoint success/refusal cases, JSON response-schema checks and byte-level request cases (exact body bounds, chunked and cancelled bodies, repeated and malformed headers, hostile JSON) | No complete operation × status × actor/header/body-case execution ledger. The checker validates status/body, not every header rule or runtime request schema |
| [Identity](../../services/hermes-manager/crates/hermes-identity/tests/), [proxy](../../services/hermes-manager/crates/hermes-proxy/tests/gateway.rs), [gateway binary](../../services/hermes-manager/crates/hermes-gateway/tests/gateway.rs) | Signed JWTs, callback/session/refresh denial, wrong claims and keys, route/cookie/header hygiene, streaming, accelerated revocation and compiled gateway flows | Many use fake OIDC/native/resolver services. They are integrations, not pure units or substitutes for the full real-Keycloak/browser matrix |
| [Kubernetes driver](../../tests/personal_hermes/kubernetes.py) and [adapter tests](../../services/hermes-manager/crates/hermes-kube/tests/cluster.rs) | Real adapter/admission tests plus controller-process abort/recovery; scoped ServiceAccount, idempotent apply, stale deletion, leader election, actual SSH worker and image-pull failure, and the admission/RBAC matrix (positive control and one-field negatives for Pods, namespaces, volumes, Services, RoleBindings and controller rights) | Direct single-server k3s is the CI default. An optional k3d path exists, but is not the standard CI path or the restore implementation. The separate pytest network matrix uses live positive controls across one and three nodes; the installed platform remains single-node |
| [Native owner](../../tests/personal_hermes/native_owner_payload.py), [runtime](../../tests/personal_hermes/native_runtime.py), [initializer tests](../../tests/integration/test_personal_init.py) | Exact owner login/session/refresh, missing-policy refusal, non-root startup, forced restart, retained settings, concurrent/interrupted initialization and symlink/special-file refusal | Docker/in-image checks are partly standalone scripts with aggregate pass output. Not all malformed-policy/startup combinations are tested in the installed system |
| [System scenario](../../tests/personal_hermes/native_e2e.py) | Real chart/operator Jobs, real native chat/cron/uploads, two owners, private registry, suspension, outages and second-cluster restore | One sequential script of more than 3,000 lines. The standalone invocation stops at its first failure and shares mutable state and a process-wide DNS patch. Collected pytest wrappers provide scenario IDs/JUnit with common helpers, but do not give the script itself per-scenario reporting |
| [UI browser fixtures](../../services/hermes-manager/ui/e2e/fixtures.ts) and [production browser case](../../services/hermes-manager/ui/e2e/production/assets.spec.ts) | Presentation workflows with mocked APIs; separate real-server CSP, write-only secret and template publication case | The production case injects cookies created by a Rust fixture and ignores browser HTTPS errors. It does not perform a real browser Keycloak login or commission an instance through the cluster dashboard |
| [CI checks](../../.github/workflows/checks.yml) and [release](../../.github/workflows/release.yml) | PR/main jobs run native language suites, image/runtime checks, direct-k3s integrations and native system script; Python/Rust profiles enforce exact collection and passing outcomes and retain sanitized reports; manual Cluster scenarios runs pytest cluster/destructive profiles; release requires both workflows for the exact commit and runs Python/preflight/build smoke | No scheduled extended topology/soak lane or aggregate obligation-coverage gate. The source-commit gate does not compare tested and published artifact digests |

## Requirement-by-requirement assessment

“Partial” below identifies unproven variants of an otherwise exercised
requirement; it does not assert that the implementation is defective. Evidence
names are representative proving cases, not a second copy of `TESTS.md`.

| Requirement | Existing proving evidence | Remaining coverage obligation | Gaps |
| --- | --- | --- | --- |
| R1 — identity and PostgreSQL authority | `identity_keys_are_exact_and_email_is_not_a_key`, signed-token tests, `startup_check_accepts_only_the_registered_installation`, outage scenario; real-Keycloak signing-key rotation followed without a restart (`test_identity.py`) | Actual database durability-settings refusal, restricted runtime roles and broader dependency recovery; refresh-token expiry, keyring rotation across restart, provider partition | G10, G15, G18 |
| R2 — explicit enrollment only | `people_without_a_grant_reach_no_administration_endpoint`, `unassigned_and_foreign_identities_are_denied_without_side_effects`, real unassigned login denial | Parameterized actor × mutation matrix with negative side-effect snapshots, including directory/group changes and browser workflows | G03, G13, G21 |
| R3 — exactly one instance | Shared `concurrent_enables_for_one_identity_commission_one_instance` and same-key races; controller abort tests | Real concurrent public API commands and two controller processes during leader turnover, with stable namespace/material/worker identities | G10, G18 |
| R4 — independent owner state/credentials | `material_opens_only_for_its_instance_and_purpose`, real two-owner logins, distinct secret hashes and isolated uploads | Native history/profile/session/model-onboarding separation after crashes/restores; clarify intentionally shared referenced admin-provider keys versus per-owner native credentials | G12–G14, G17 |
| R5 — verified owner routing/no fallback | Store route cases, resolver mTLS negatives, proxy hygiene/cookie cases, foreign native tickets; live Pod-level allow/deny matrix across roles, owners and lookalikes on one and three nodes, with real-default gateway deadlines | Same paths across multiple API/gateway replicas, warmed-cache restart variants and approved external egress | G06, G07, G15, G18 |
| R6 — native workflows and owner pin | Native login/session/refresh checks; system chat/settings/uploads/WebSocket/logout; production dashboard CSP case; Chromium login through Keycloak, cookie scope, owner-only files, logout and session replay, foreign-origin requests | The native UI as a rendered application: history/session persistence, representative native mutations, hostile rendered content and cancellation | G12, G13, G21 |
| R7 — dashboard/API administration parity | Broad API workflows, mocked presentation workflows, real-server secret/template browser case; Chromium dashboard login, dashboard-driven enrollment of a working instance, stale-revision refusal, administrator revocation; the dashboard's and the gateway's interface ledgers (unauthenticated rows, CSRF/Origin, cookie faults, ambiguous credentials), and contract-generated access, unknown-identifier and stale/missing-precondition matrices, with the named state conflicts and idempotency-key reuse | Template rollout and the remaining error recovery in the browser; enumerate browser/API parity explicitly | G03, G13, G21 |
| R8 — never overwrite/reseed | Initializer crash/concurrency/symlink cases, retained profile/memory tests, runtime restart and setting preservation | Real mounted home through runtime/template update, override removal, image change, interrupted init Pod and restored private-home contents | G12, G16, G17, G20 |
| R9 — revoke/suspend bounds and retention | Shared retained-state cases, real suspension closes WebSocket and drains pending worker, re-enable checks | Full explicit revoke as well as suspend; exact 5 s/10 s timing; active non-terminating worker forced by 5 min; volume/key preservation after Helm uninstall | G06, G11, G16 |
| R10 — settings precedence | `overrides_merge_leaf_by_leaf`, protected-key rejection, runtime/profile policy tests, system model/admin-secret precedence | Installed template/assignment/user precedence matrix, removing an override exposes retained user value, malformed/absent mounted policy and image-plugin substitution | G09, G12, G14 |
| R11 — selective write-only secrets | Shared secret reference/kind/version/retention cases; real env-model key and registry secret; API/audit/log assertions; cluster rotation of the model key: only the referencing instance restarts and receives the current value, and no API or audit output carries it | File and registry delivery, selective removal, wrong/missing sealing keys and redaction during failures, browser/test artifacts included | G14, G15, G20 |
| R12 — allowed execution only | Shared acquisition/replay/expiry/parent/limit cases, SSH readiness, plugin request schemas, real allowed/denied/default/custom/private backends; the three modes, two profiles with a changed default, parent joins and the lease limit on real workers; worker death and command non-replay | Races at revoke/expiry/release/parent join; active volume change; forced drain of a non-cooperating worker | G09–G11, G18, G21 |
| R13 — approved storage and administrator-controlled sharing | Validation/shared static path races, renderer/mount tests, selected API admission negatives, missing static directory system case; read-only volumes refuse writes in the Pod while a writable sibling accepts them (`test_storage.py`) | Live NFS, unreadable/replaced path cases, multi-node placement; PersistentVolume update and remaining admission verbs | G07, G08, G16 |
| R14 — shared paths and backend worker identities | Equal mount-path checks; backend non-root bounds, alternate-identity rendering and declared-UID SSH checks; worker/Hermes admission matrix; `bob_gets_a_shared_workspace_and_his_network_home`, real shared `/workspace` files and a backend-only fixture home | “Network home” in the system fixture uses local static storage, not a live NFS export; multi-node mount/scheduling and group/permission variants; alternate-identity native execution | G02, G16, G23 |
| R15 — scoped controller authority | Chart token/RBAC comparisons, the real admission/RBAC matrix with positive controls and mutation-checked guards, impersonation and escalation refusals, client and kubeconfig refusal rules; the admission/RBAC matrix with positive controls (`admission.rs`) and the live NetworkPolicy matrix | PersistentVolume updates, remaining workload verbs and subresources, each non-controller role's credentials | G07, G08 |
| R16 — one continuous replica per owner and independent workers | Pure rollout/readiness tests, real no-browser terminal/cron, worker idle retirement; a standby takes over a killed leader and Hermes runs untouched without a controller (`test_leadership.py`) | Recreate overlap under concurrent changes, node/worker loss while Hermes stays healthy, all lifetime modes and prolonged gateway/cron liveness | G11, G18, G19 |
| R17 — durable crash recovery | Shared store cases, SQL guard tests, API commit-boundary aborts, five real controller fault locations, planner totality; four lease-race cases on both stores and the cluster leadership cases | Worker/lease process failures, process pause and API partition, database crash/commit ambiguity, dropped notifications, bounded Kubernetes failures and stale observations | G09–G11, G18 |
| R18 — no unintended secret exposure | Privacy allowlists and rejection tests; sealed values, write-only API/audit cases, generated secret scan of app logs | All secret delivery/rotation/error paths; built artifacts and public layer contents; reports/screenshots/traces/driver exceptions; access using restricted DB roles | G10, G14, G20, G21 |
| R19 — restore without adoption | New-cluster rebind restores SQL and a static workspace, reuses the sealing key and permits login; restored older DB reports new namespaces/PVs as orphans without adoption | Restored private-home profiles/memory/sessions/native state and explicit before/after credential digests; successful same-cluster known-instance recovery; missing/wrong keys and partial backups; provisioner-specific dynamic-data evidence | G16, G17 |

## Acceptance scenario accounting

All 23 acceptance rows have entries in `TESTS.md`. This table identifies their
main present proving boundary and the additional assertion or variant needed.
U = native unit/shared fake; I = real dependency/process integration;
E = installed-system assertions in the native script or collected pytest wrappers.
The cited runner determines whether the fixture is direct k3s or k3d.

| Specification scenario | Present evidence | Additional case or test-system work |
| --- | --- | --- |
| Two assigned identities, one URL | I/E owner login, settings and uploads; E two browser contexts reaching only their own instance | Native history/session/profile isolation (G12, G13) |
| Same path, different verified subjects | U/I/E route and header isolation | Cross-owner live service/Pod access and multi-replica cache isolation (G07, G18) |
| Valid but unassigned identity | U/I/E no enrollment | Independent pytest case with before/after resource inventory (G01, G03) |
| Concurrent enable, retry and controller kill | U/I store races and real aborts | Public API concurrency and competing controller processes together (G10, G18) |
| Email change or new subject with an old email | U/E real directory changes | Independent browser case; exact claim/case variants retained (G01, G13, G15) |
| Forged headers, foreign cookie or ticket, callback race | I/E headers, cookies, tickets; identity callback races; E foreign-origin pages, reflected callback text | Cluster-negative matrix with observable zero backend hits (G07, G15); sibling-host behaviour awaits a decision (G13) |
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
| UI build and dependency boundary | U/I architecture, assets, CSP and privacy; E strict policy and hostile stored text in the deployed dashboard | Artifact-content verification and matching tested/published digests (G20) |

## Operational bounds

These are the nine rows in the specification, not new performance promises.
Some existing checks are accelerated or prove an example rather than the exact
production boundary. G06 owns their measurement contract.

| Bound | Present evidence | Missing precise evidence |
| --- | --- | --- |
| Route resolution: 2 s | `a_stalled_registry_is_unavailable_within_the_deadline` (manager side) and the gateway resolver client's three deadline cases (silent resolver, stall in the body, oversized reply) | Hung TLS/DNS variants and no upstream fallback end to end |
| Authorization: 5 s decisions; 10 s open connections | Gateway cases at the real default timings: WebSocket closed within 10 s of a denial and of a dependency failure, denial effective for new requests within 5 s of a warmed cache; accelerated mechanism checks; system suspension and outage checks | The native system checks still wait 6 s and accept 12 s; multiple gateway replicas, restart and warmed-cache variants on a cluster |
| Identity evidence: refresh at most 60 s; reject older than 120 s | Token maximum-age checks and real provider outage/revocation | Exact refresh cadence and just-before/at/after freshness under a warmed cache, restart and multiple gateways |
| Provider revocation: 130 s | Real provider disable/outage checks | Start at provider acknowledgement, preserve event timestamps, prove no fresh access/reconnect after expiry |
| Uploads: 64 MiB or lower | Limit − 1, the limit and limit + 1 with a declared length at a lowered limit and at the default 64 MiB; a streamed upload without a length at the limit and one byte over (`413`, never a success or an outage); an over-limit upload never reaches the instance | Cancellation without cross-owner partial effects; long slow uploads |
| WebSocket idle: up to 1 h with heartbeats | Handshake/byte forwarding and short chat connections | Long-idle heartbeat soak, proxy reconnect/resource cleanup and concurrent revocation |
| Terminal drain: at most 5 min | Pure planner deadlines, shared lease cases, real pending-worker drain | Non-cooperating active command/lease at forced cutoff, all lifetime modes and preserved data |
| Calls: bounded deadlines/concurrency/backoff | Queue unit tests, selected HTTP/probe timeouts, explicit command timeouts | Systematic 429/5xx/stall/cancellation, lost notifications and bounded in-flight work during prolonged failure |
| Bodies: 16 KiB; admin config: 8 KiB | Exact bounds (declared: limit − 1, limit, limit + 1; chunked: limit, limit + 1) on the administration, execution and resolver listeners; the serialized `adminConfig` bound at template and assignment layers; cancelled bodies; Unicode, duplicate-key, unsafe-number and deep JSON refusals with no effect (`wire_boundaries.rs`) | Gateway-side request bodies and the full response/header semantics per operation (G21) |

## What cannot be concluded from the current green results

- No measured line/branch percentage or comprehensive mutation/fuzz result exists.
- The manually dispatched `Cluster scenarios` workflow runs the k3d network,
  acceptance, browser and destructive profiles. They are not automatic PR gates;
  a workflow definition or a source-level assertion is not a candidate result. No live NFS test and no multi-node acceptance run of the platform
  exist; the platform runs on a single node.
- Generated manifests matching each other do not demonstrate live isolation.
- A traced function name does not prove collection, feature selection, execution,
  its asserted level, or complete coverage of a requirement's clauses.
- The static restore scenario does not establish recovery of every private-home
  storage backend. The specification already limits its dynamic-provisioner claim;
  keep that deployment-specific limit explicit rather than claiming universal recovery.
- The browser journeys drive the dashboard and the gateway's own pages, not the
  native UI as a rendered application, and they run with Local Network Access
  checks off (the fixture's node address is private).

These are coverage and evidence limitations. The gap register identifies the
tests and refactors needed to resolve them without discarding the existing
working unit, shared-contract and integration suites.
