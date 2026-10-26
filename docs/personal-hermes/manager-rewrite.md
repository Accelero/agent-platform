# Manager rewrite guide

Status: active plan, 2026-10-22. This guide covers the move from the current
reference implementation to the target design:

- what is wrong with the current implementation and its design;
- what changes and why;
- what is kept;
- the order of work.

The target itself is described in the [specification](specification.md) and
the [architecture](architecture.md). Delete this guide once the rewrite is
complete.

The project is in early development. Existing state (databases, namespaces,
volumes) is discarded, not migrated. The new schema starts at migration
`0001`.

## Audit of the reference implementation

The audit covers `feat/personal-hermes-commissioning` at `19b8976`. The code
is unchanged at `76c3391`. It follows on from the
[repository audit](../audit-2026-10-22.md), whose concern 2 ("the hexagonal
split is mostly nominal") prompted this rewrite.

### Shape


| Crate | Lines (src) | Intended role | Actual content |
| --- | --- | --- | --- |
| `hermes-domain` | ~440 | Entities, invariants | Identity/name/mount validators, resource-kind enum |
| `hermes-application` | ~3.2k | Use cases, ports | Mostly DTOs and about 20 port traits; two real use cases (`Assignments`, `Routes`) |
| `hermes-adapters` | ~19k | Thin I/O | Nearly all behaviour |
| `hermes-manager` | ~750 | Composition root | Configuration, wiring, controller loop |

`hermes-adapters/src`:

| Area | Lines |
| --- | --- |
| PostgreSQL | 6.5k |
| Kubernetes driver | 3.0k |
| Renderers/admission | 2.5k |
| Identity/sessions/TLS | 2.5k |
| HTTP | 2.4k |
| Controller orchestration | 1.2k |
| Native helpers | 1.0k |

The schema has 56 tables in 24 migrations, with 23 `guard_*` trigger
functions. There are about 20k lines of Rust integration tests in 39 files. 31
of those files drive `PgStore`/`KubernetesDriver` directly, and together they
contain 331 raw SQL statements. There is no consistent test strategy: most
logic can only be tested end-to-end against PostgreSQL.

### Code findings

**F1. Business rules live in SQL.**

- `AssignmentStore::mutate` (`postgres.rs`) does all of the following in one
  SQL script:
  - template approval;
  - default-version resolution;
  - the infrastructure-ownership check;
  - mapping access to operation kind (`"enabled" => "commission"`);
  - the drain start.
- `dashboard_apply_template` decides eligibility from a JSON preview
  (`inspected["eligible"] != true`).
- The lease rules are multi-join `WHERE` clauses in `terminal/leases.rs`:
  - access;
  - policy generation;
  - exclusivity;
  - quotas;
  - parent join.
- The specification's five-minute drain bound is a SQL literal, repeated
  across statements and modules.

None of these can be tested without PostgreSQL, and the same rule exists in
more than one place.

**F2. The mutation boundary is copied by hand.** The specification requires
gate, current grant, idempotency, change, audit and outbox in one transaction.
This sequence is written out separately in five modules: `postgres.rs`,
`execution.rs`, `rollout.rs`, `authoring.rs` and `administrators.rs`.
Operations are inserted from four modules and audit events from four. A new
command can silently miss part of the boundary.

**F3. HTTP bypasses the application layer.** Admin handlers hold a
`PgStore` and call 17 inherent `dashboard_*` methods that return
`serde_json::Value` already shaped as wire JSON. The wire contract is therefore
defined in SQL code and cannot be checked against Rust types.

**F4. Orchestration lives in an adapter.** `hermes-adapters/src/controller*`
is the core workflow: commission, stop, drain, remove, policy activation,
worker provisioning and retirement. It is written against the concrete
`PgStore`/`KubernetesDriver`, calls inherent methods that no port defines, and
shells out to `ssh`. It cannot be tested without a database and a cluster.

**F5. The safety protocol is not an abstraction.** "Renew claim → commit
intent → dispatch → observe → commit result with fence" is unrolled by hand
about 25 times. `renew(&self.scope, claim, 30)` appears 25 times, with error
mapping chosen locally each time.

**F6. Each action kind has its own mechanism.** There are separate claim
queues for operations, policies, environments, retirements, observations and
resync. There are also separate intent, input and receipt tables for each kind
of external action:

- `namespace_fence_inputs`
- `native_child_actions`
- `native_stop_inputs`
- `execution_worker_intents`
- `execution_worker_start_inputs`
- `execution_worker_start_intents`
- `execution_worker_stop_inputs`
- `execution_worker_absence`
- `execution_worker_ready_receipts`
- `execution_policy_admission_inputs`
- `execution_policy_renderings`
- `resource_steps`
- `operation_steps`
- and more.

They all implement the same idea, each in a slightly different way.

**F7. A sequential god loop.** `Controller::step` runs in order:

1. ingestion;
2. resync;
3. policy;
4. lease expiry;
5. retirement;
6. observation;
7. provisioning;
8. one operation.

A slow Kubernetes call stalls everything. Concurrency bounds have no place to
live.

**F8. Lifecycle states are strings.** About 20 state literals (`'Active'`,
`'Draining'`, `'Retiring'`, `'Reserved'`, …) are compared in SQL and Rust. The
state machines exist only in prose and in triggers.

**F9. One flat error enum for every layer.** Domain rejections, protocol
failures, concurrency signals and infrastructure faults share one enum.
`db_error` collapses every SQL failure to `AlreadyExists` or `Unavailable`.
Reconcilers map errors to schedules through wildcards; for example,
`Invalid | Forbidden | OwnershipConflict` all become `AdmissionPending`.

**F10. Build-level coupling.** `hermes-router-auth` links kube-rs, the
renderers and the whole registry store, although it needs only OIDC, sessions
and TLS.

**F11. Two desired-state sources, resolved by branching.**
`infrastructure_managed` is checked in three separate branches of
`Controller::reconcile`, as well as in `mutate` and `rollout`. Infrastructure
ingestion is its own 230-line SQL path, with its own operations and audit.

**F12. Vestigial authority.** `AdminPolicy.management_client`,
`administrator_role` and `VerifiedPrincipal.client_roles` are parsed and
carried but play no part in authorization. PostgreSQL grants are the only
authority.

### Worth keeping

- The safety goals:
  - durable acknowledgements;
  - database time;
  - UID and resourceVersion preconditions;
  - absence before completion;
  - no reseeding.

  Some of the old *mechanisms* behind them are removed (next section).
- Strict input handling: `deny_unknown_fields`, bounded reads, no secrets in
  errors.
- The pure workload and admission renderers.
- The cryptographic, OIDC, JWKS and TLS components.
- The native init receipt (`runtimes/hermes/personal-init.py`).
- The `managed_execution` plugin and its contract.
- The Python acceptance fixtures, as scenarios.

### Design findings

Some of the code findings come from the earlier design itself.

**D1. Step journaling for repeatable actions.** The old specification
required "commit intent, dispatch, observe, commit result" for every
Kubernetes action. Most of those actions are repeatable declarative writes,
which level-triggered reconciliation already makes crash-safe. The journal
roughly doubled the controller code and produced the per-action tables. Only
five effects are genuinely non-repeatable.

**D2. Defences that add nothing over an earlier layer.**

- HMAC creation provenance and per-object reservations: admission already
  makes the controller the only writer in owned namespaces.
- Per-namespace admission parameters written by the controller itself: they
  cannot stop a compromised controller, and static invariants catch a buggy
  one.
- Scheduling gates and recorded Pod UIDs before native start: the instance
  credential identifies the logical instance, not a Pod.
- Relaying the user's access token from the router to the manager on every
  request: a compromised router controls the traffic anyway.

**D3. Two writers of desired state.** The infrastructure stack wrote tables,
while administrators used the API. That produced three things:

- infrastructure-owned branching;
- a second ingestion path;
- three template concepts.

**D4. A four-part router:**

- Traefik multi-layer routing;
- a Rust ForwardAuth service;
- a Go/Yaegi plugin that hijacks WebSockets to re-authorize them;
- a manager resolver that re-verifies relayed tokens.

The strictest revocation bound was enforced in an interpreted plugin.

**D5. Policy activation as a distributed protocol.** Changing a policy
fenced acquisitions, drained environments, wrote admission parameters, read
them back, recorded receipts and only then activated. Stamping new
environments with the new generation achieves the same result.

**D6. Storage that only an operator with cluster access could configure.**
A storage binding had to name an existing PVC inside a manager-created
namespace, by namespace UID and PVC UID. That contradicts controller-owned
namespaces, and it gave the dashboard no way to say "Bob's home folder". The
terminal `/workspace` was also an `emptyDir`, separate from the Hermes
workspace.

**D7. Recovery modes ahead of need.** Restore and maintenance modes, epoch
mirroring and the fresh-install scanner all served a checkpoint flow that does
not exist.

**D8. Per-item leases and fences across controller replicas.** A single
leader-elected controller is the standard pattern, and it removes them.

## Decisions

| # | Decision | Replaces | Reason |
| --- | --- | --- | --- |
| A1 | Level-triggered controller using server-side apply | Step journal, frozen renderings, per-action tables | D1 |
| A2 | Five named non-repeatable effects, one mechanism each | Generic journal | D1 |
| A3 | One leader-elected controller with a queue rebuilt from PostgreSQL | Item leases, fencing tokens, outbox, six claim queues | D8 |
| A4 | Namespace UID binding as the ownership root | Per-object HMAC provenance, owned-resource registry, ownership keyring | D2 |
| A5 | Static admission policies installed by the chart, with parameters from installation values | Per-namespace parameters, `render-bootstrap` step | D2, D5 |
| A6 | `execution_generation` stamped on environments | Policy activation protocol and receipts | D5 |
| A7 | The administration API is the only writer of desired state | `infrastructure_templates`, `desired_users`, ingestion | D3 |
| A8 | One template catalog with Draft, Published and Withdrawn versions; bulk set-template returns per-item results | Infrastructure templates, catalog snapshots, draft and batch tables | D3 |
| A9 | `hermes-gateway`, a Rust reverse proxy | Traefik, ForwardAuth, router-auth, Go plugin, route file publication | D4 |
| A10 | The gateway asserts identity over mTLS | Relayed token evidence | D2 |
| A11 | Native owner pin kept mandatory | — | An independent second check |
| A12 | 5 s authorization freshness for HTTP and WebSocket | Positive caching forbidden for HTTP, separate WebSocket bounds | One bound, one mechanism |
| A13 | Deployment readiness plus receipt plus probe | Scheduling gates, native child tracking, native stop journal | D2 |
| A14 | Installation reduced to identity, issuer, cluster UID and orphan reporting, plus `rebind-cluster` for restores | Restore and maintenance modes, epoch mirroring, inventory scanner | D7 |
| A15 | SSH readiness checked in-process with `russh` | `/usr/bin/ssh` subprocess | No subprocess in the privileged process |
| A16 | Storage sources in installation values; assignment volumes, rendered by the controller as PVCs or as PersistentVolumes plus PVCs | Storage bindings to pre-existing PVCs | D6 |
| A17 | Shared volumes with identical paths and one data identity in Hermes and terminal backends | Worker `emptyDir` workspace | D6; R14 |
| A18 | Write-only admin secrets sealed to the controller's key | No secret mechanism; pull-Secret references | Enforcing an LLM provider usually needs a key |
| A19 | Assignment `adminConfig` overrides on top of template settings | Template-only settings | Per-user enforcement without a template per user |

## Why rebuild

- **No state to keep.** Nothing is released, and existing state may be
  discarded.
- **The defects run through every module.** Rules and wire shapes live in
  SQL, and orchestration lives in adapters. Fixing that means rewriting each
  module.
- **The tests can't check the restructure.** The Rust tests are bound to
  `PgStore` and the current schema.
- **Whole subsystems go.** The decisions above remove them, so there is
  nothing to refactor them into.

### Transplant list

These components move with a line-by-line review, not a rewrite:

| Current | Target | Condition |
| --- | --- | --- |
| `workloads.rs`, `execution_workloads.rs`, `admission*`, `bootstrap.rs`, `kubernetes/quantity.rs` | `hermes-render` | Golden output captured from the current code first. Diffs are reviewed and limited to the decisions above: storage, mounts, secrets, no scheduling gates, static admission |
| `session_crypto.rs`, `materials.rs` | `hermes-native` and `hermes-identity` | Associated-data layout reviewed; admin secret sealing added |
| `auth.rs`, `oidc.rs`, `sessions.rs`, `browser_auth.rs`, `browser_http.rs`, `server.rs`, `directory.rs` | `hermes-identity` | Role and `client_roles` fields removed |
| `router_auth.rs` session logic | `hermes-identity`, `hermes-proxy` | ForwardAuth and plugin endpoints removed |
| `native_probe.rs`, `native_receipt.rs`, `native_bootstrap.rs` | `hermes-native` | — |
| `template_validation.rs`, `hermes-domain` validators | `hermes-core::domain` | — |
| Kubeconfig validation (`hermes-manager/src/controller.rs`) | `hermes-manager` configuration | — |

Everything else is written fresh against the architecture:

- `postgres*`
- `controller*`
- `http*`
- `execution_http.rs`
- `hermes-application`
- `kubernetes.rs` except observation helpers

The same applies to the whole Rust test suite (see the
[verification strategy](architecture.md#verification-strategy)). Before the
old tests are deleted, every scenario they cover is listed in `TESTS.md` and
either mapped to a requirement or dropped with a reason.

### Outside the manager

| Area | Change |
| --- | --- |
| `runtimes/hermes` | Plugin unchanged. The image gains no new behaviour; admin secret files and shared mounts are just paths |
| `runtimes/agent-runtime` | Unchanged; already UID/GID 1000 and `managed-ssh-v2` |
| `plugins/hermes-route-resolver` | Removed at cutover |
| `charts/agent-platform` | Deploys api, controller and gateway. Installs RBAC and static admission policies with storage sources and data identity as values. Drops router-auth and the `render-bootstrap` prerequisite |
| Dashboard UI | Terminal backends naming; template versions; assignment volumes and backends; secrets and storage views; bulk results instead of batch pages; no infrastructure-owned badges |
| Private deployment repository | Stops writing `desired_users` and `infrastructure_templates`; supplies storage sources, keys and Secrets through chart values; backs up PostgreSQL and storage roots |

## Phases

The new workspace was built alongside the previous one and replaced it in
`services/hermes-manager/` at cutover. A phase is done only when its rows in
`TESTS.md` pass.

| Phase | Scope | Done when |
| --- | --- | --- |
| R0 | Docs aligned with the target; workspace skeleton; architecture and traceability tests; `TESTS.md` with every requirement, scenario, contract operation and harvested old test; CI job | CI green; every row listed as pending |
| R1 | `hermes-core` domain and envelope; `hermes-store` schema; in-memory fakes; port contract suites; fault-injection harness | Unit tests green; PostgreSQL integration green for envelope, grants and idempotency |
| R2 | CLI and configuration (`migrate`, `check`, `grant-administrator`, `api`); identity crate (tokens, OIDC, sealed sessions); admin authentication, `/auth/*`, `/api/v1/session` and dashboard serving; test kit (fake realm over TLS) | Unit tests for the authentication contract; integration tests for sessions, the HTTP boundary and the binary's roles |
| R3 | Assignments, templates, terminal backend versions, storage sources and volumes, admin secrets, directory, audit and administrators (administration API complete); dashboard adapted | Unit contract tests for every administration path; integration tests for the store; UI tests |
| R4 | `hermes-kube` (cluster identity, observe, server-side apply, preconditioned delete, leader election); `initialize`, `controller` and `rebind-cluster` roles; chart RBAC and static admission templates; instance planner, controller loop, materials, admin secret delivery, static volume rendering, receipt and probe; golden renderer output captured from the reference implementation first | E2E: two owners with commissioning, update, restart, suspend and re-enable with retained data; shared and static volumes; kill matrix |
| R5 | Execution API, environment planner, SSH readiness, lease expiry | E2E terminal fixture: two owners, two backends, a denial, the shared workspace, a static home, retirement; plugin contract test |
| R6 | `hermes-gateway` | Unit revocation timing; e2e with a Keycloak container and two native instances |
| R7 | Cutover: chart, Dockerfile and `release-policy.json` switched; deployment repository updated; restore drill (`rebind-cluster`); old workspace, plugin and router-auth deleted | Full verification list and privacy preflight pass |

R3 and R4 can proceed in parallel after R2. R6 can start after R2.

### Progress

| Phase | State |
| --- | --- |
| R0 | Done: workspace, architecture and traceability tests, `TESTS.md`, CI job |
| R1 | Done: domain errors and validators, identity and grants, command envelope, kernel and access schema, in-memory store, eleven envelope contract cases passing on both stores, store guards and startup checks |
| R2 | Done: token verification, OIDC, sealed sessions with fenced refresh, admin router with bearer and browser authentication, CSRF, dashboard assets, request limits, binary roles; `initialize`, the controller roles and the chart templates moved to R4 because they need the cluster adapter |
| R3 | Done: catalog, assignment, rollout, secret, grant and query use cases with contract cases on both stores; every administration endpoint with contract-checked integration tests; Keycloak directory; secret sealing to the controller's public key; the dashboard in `services/hermes-manager/ui` rebuilt on the new contract with unit, browser and production-CSP tests. `GET /api/v1/instances/{id}/environments` follows in R5 with environments |
| R4 | Done: instance planner (total over every desired × observed case), controller loop with work queue, epochs and Lease leadership, create-once material sealed to the controller's key, admin secret delivery, `hermes-render` (instance set, managed configuration, RBAC and admission policies, golden files), `hermes-kube` (bounded observation with the owner chain, server-side apply, preconditioned delete), receipt verification against the image's real initializer, the native probe, and the `initialize`, `controller`, `rebind-cluster` and `generate-sealing-key` roles. Verified on k3s as the controller's real ServiceAccount, including aborts at every controller fault point. Moved to R7: chart templates for the rendered RBAC and policies (the chart still deploys the reference implementation) and the e2e runs with real Hermes images. The renderer follows the image contract directly instead of golden output captured from the reference |
| R5 | Done: environment and lease state machines with a total environment planner, execution use cases (acquire with replay, heartbeat, release, close, listing) as contract cases on both stores, the execution listener in the `api` role, worker Pods with keys and registry credentials, SSH readiness with `russh`, controller worker lifecycle and lease expiry, and `GET /api/v1/instances/{id}/environments`. On k3s the repository's `agent-runtime` image passes admission, accepts only the instance's client key, presents the provisioned host key and runs as the data identity. Worker pull, start and contract failures are recorded on the environment and drain it without replacement; the execution contract adds `422 backend_failed` for them |
| R6 | Done: the routing decision as a pure function over the registry row (an enabled assignment and a ready instance at its desired generation, in the installation's own namespace format), the resolver use case and its listener in the `api` role, answering only the configured gateway certificate identity, with a 1.5 s internal deadline. `hermes-proxy` provides header and cookie hygiene, the mTLS resolver client that accepts only consistent replies for the instance's own namespace, 5 s positive caches for sessions and ready decisions, forwarding with a 64 MiB upload bound, status pages, the intercepted native logout and one supervisor that ends WebSockets and streamed responses on revocation. `hermes-gateway` reads its configuration from files and serves over TLS. Tested with fake ports for timing and hygiene, and with the fake realm, PostgreSQL sessions and the real resolver over mTLS for two owners. Moved to R7: the e2e run with a Keycloak container and two native instances in k3s |
| R7 | Done in this repository: the chart deploys `api`, `controller` and `hermes-gateway` with generated configuration, the controller RBAC and static admission policies (equal to the renderer's golden output in the chart tests), NetworkPolicies and one-shot operator Jobs, including `rebind-cluster`; the image builds both binaries; `release-policy.json`, CI and the release checks switched; the previous workspace, its contracts, the Traefik plugin and its fixtures removed. The end-to-end run installs the chart on k3s with an in-cluster Keycloak, a fake model and a private registry, and covers every acceptance row that needs a cluster, real Hermes or real OIDC (see the [implementation status](implementation.md#evidence)). It found and fixed: operator Jobs could not read their Secrets; the admission policies refused the scheduler's claim binding; commands without a declared directory, as in scheduled jobs, ran in root's home, which the worker cannot enter; `rebind-cluster` kept the old cluster's workers and leases; the release build context missed two migrations. Orphaned namespaces and volumes are now reported. Open: the private deployment repository's update, which consumes a released chart |

## Open questions

1. ~~Gateway WebSockets: terminate and re-originate, or splice?~~
   **Decided:** splice after the upgrade. Bytes pass unchanged, so native
   heartbeats, subprotocols and compression need nothing from the gateway;
   revocation closes both connections. Streamed HTTP responses use the same
   supervisor.
2. ~~Administrator session revocation.~~ **Decided:** administrator and
   gateway sessions use the same mechanism, including the fenced provider
   refresh every 60 seconds. One mechanism instead of two, and a disabled
   Keycloak account stops administration within about two minutes.
3. **Dynamic volume restore.** I recommend configuring the local-path
   provisioner with a path pattern derived from namespace and claim names, and
   verifying in the R7 restore drill that it reuses existing directories.
   Otherwise, put restorable data on static sources.
4. **Shared LLM keys.** An admin secret is readable by the owner's own agent.
   For shared provider access, I recommend a model gateway that issues
   per-user keys, so revoking one user doesn't require rotating everyone's
   key.
