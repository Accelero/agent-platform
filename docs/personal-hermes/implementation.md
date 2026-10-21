# Personal Hermes implementation status

Status as of 2026-10-21 on `feat/personal-hermes-commissioning`. This is the
current source implementation and evidence index. The chart in this checkout
remains version 0.6.1 and does not install the new Rust manager or router-auth
services. No deployment or release qualification is implied by these tests.

The [specification](specification.md), [architecture](architecture.md) and
[delivery plan](plan.md) describe the full target product. Use this page for what
exists now, the [manager README](../../services/hermes-manager/README.md) for
process configuration and authentication, and the
[native commissioning guide](native-commissioning.md) for the infrastructure
input contract and per-user deployment behavior.
The [terminal backend guide](terminal-backends.md) covers profile selection,
scoped worker provisioning, SSH identity and cleanup.
The [current handover](handover.md) records the completed SSH acceptance,
integration fixes and remaining work.

## Current capability boundary

| Area | Implemented and verified | Remaining work or limitation |
| --- | --- | --- |
| Native commissioning | Infrastructure-owned template/user rows create one instance per exact installation issuer and Keycloak subject; image/config changes advance desired generations; removal retires the workload and re-add restores retained state | Infrastructure must deploy the manager and write its input rows; this is not Keycloak user synchronization or self-enrollment |
| Templates and image updates | Typed, digest-pinned deployment definitions, immutable catalog snapshots, per-user template selection and polling of database changes | No arbitrary Pod manifests, registry/tag watcher, automatic upstream release discovery or automatic image rollback |
| Native configuration and state | Separate home/workspace PVCs; missing-only initialization; create-once credentials; Hermes managed config overrides selected leaves while user config remains on the home PVC | No automatic storage-class migration, volume shrinking or permanent data purge |
| Shutdown during updates | Kubernetes Recreate rollout; runtime forwards SIGTERM to gateway/dashboard, waits up to 20 seconds and then kills remaining children; Pod grace period is 45 seconds | No update handshake that waits for agent runs to finish or acknowledges a completed application checkpoint; active work can be interrupted |
| Manager crash recovery | PostgreSQL action journal, leased/fenced claims, frozen renderings, authenticated resource provenance, exact UID/RV checks, uncertain-create recovery and periodic resync | Ownership/admission/manual-recovery faults remain blocked; a deleted or foreign resource is not blindly adopted or replaced; exhaustive crash testing at every external boundary remains open |
| Administration | Versioned API, Keycloak access-token validation, browser OIDC sessions, CSRF, audit, optimistic mutations and execution configuration endpoints | No manager admin frontend, generated React client, owner-status/checkpoint UI or completed directory integration |
| Terminal execution | Database runtime catalog and owner storage/subpaths; profile dashboard `terminal.runtime_id`; instance credential/allowlist enforcement; fenced on-demand v2 worker creation, authenticated pinned SSH readiness, scoped leases and exact-UID retirement/idle cleanup; verified two-owner SSH/file-transfer/denial/retirement integration | Nonempty runtime service-egress resolution, proactive prewarming, combined native/API/routing lifecycle and release acceptance remain open; v1 workers do not get automatic readiness |
| Routing | Rust resolver/auth processes, Go/Yaegi plugin and isolated Traefik/WebSocket compatibility proofs | Native commissioning does not publish routes; full Keycloak/native/browser/proxy integration remains open |
| Backup and recovery | Explicit fresh-install namespace/PV scan, schema/installation/epoch gates and retained ownership history | No scheduled backups, snapshot/copy jobs, backup retention, checkpoint coordinator or full data restore implementation; PVC retention is not a backup |
| Distribution and adoption | Locked source builds and CI checks; reviewed native runtime test image | No released manager/router-auth images or personal-mode chart installation; existing-owner adoption and release acceptance remain open |

The per-user native Hermes dashboard is supplied by Hermes and runs in each
instance. It is distinct from the unimplemented manager administration frontend.
Native `Ready` means the manager observed storage, the exact current workload,
signed initialization and the native HTTP/auth gate. It does not mean a terminal
backend is ready, a route was published or a real Keycloak browser login passed.

## Hermes image, plugin and recorded identity

`runtimes/hermes/Dockerfile` extends the pinned upstream Hermes image with
reviewed platform changes and bundles
`runtimes/hermes/plugins/managed_execution`. The `hermes` release build target
already supplies that image; infrastructure selects its digest in a template.
The plugin reads the profile's native dashboard `terminal.runtime_id`, requests
an authenticated HTTPS lease and uses the manager-supplied SSH endpoint with
strict host-key pinning. The v2 worker controller commits creation/start intents,
verifies real SSH login and records readiness before grants. Session close,
expiry, policy/access/generation changes, lost Pod observation and ordinary idle
cleanup feed fenced retirement with exact UID/RV deletion and verified absence.
Original requests/leases remain tombstones; a new request can create a fresh
environment after retirement. Nonempty runtime service egress still blocks
activation and combined product acceptance remains incomplete.

The manager creates a distinct opaque execution credential for each instance,
registers its hash before dispatching the bootstrap Secret and maps successful
authentication through `manager.execution_clients.instance_id`. This binds the
plugin to its logical instance and assignment without trusting a caller-supplied
user or container ID. Raw bootstrap material is encrypted; selected application
credential files are mounted read-only.

The registry persists stable instance IDs, Namespace/Deployment UIDs in owned
resource records and exact Hermes Pod UIDs in `manager.native_children`, with
verified owner provenance. It does **not** persist CRI/containerd/Docker container
runtime IDs. The execution credential survives ordinary Pod replacement; it
identifies the logical instance rather than attesting a particular Pod or
container process. The full target is specified in the
[image/plugin and identity contract](specification.md#hermes-application-image-execution-plugin-and-caller-identity).

## Manager implementation

The workspace separates domain rules, application ports, PostgreSQL/Kubernetes
adapters and process entry points. Exact issuer/subject identity, immutable
ownership and stable instance IDs are shared by the administration and
infrastructure paths. The infrastructure path owns only assignments it created;
it does not silently take over an existing API-managed assignment.

Migration `0017_infrastructure_desired.sql` adds
`manager.infrastructure_templates` and `manager.desired_users`. The infra stack
writes those tables; the controller polls bounded batches. Template validation,
immutable snapshots, generation fencing, audit and queued operations commit
transactionally. Invalid definitions are recorded on their source template and
cannot dispatch resources. Uncertain original CREATE operations are recovered
before changing their generation or processing removal.

The controller persists encrypted create-once material and frozen resource
renderings before Kubernetes calls. The native renderer creates 14 owned
resources, including a private namespace, home/workspace PVCs, selected Secret
projections, ServiceAccount, Service, policies and a non-root Recreate Deployment.
The constrained controller cannot install or relax its bootstrap RBAC/admission
policies. Native Pods start behind a scheduling gate; exact child ownership and
start intent commit before gate removal. Kubernetes acceptance tests use the
actual constrained ServiceAccount.

Removal closes native admission, stops registered children, foreground-deletes
the exact Deployment UID and waits for observed absence before marking the
instance `Retained`. PVCs, namespace, credential material and ownership history
survive. Re-adding the mapping creates a new Deployment incarnation over the
same retained storage and credentials. This native-only completion path is
separate from the unfinished combined terminal/native drain path.

Transient infrastructure commissioning failures can enqueue delayed resync work
without changing the desired generation. Ownership, admission and explicit
recovery blocks are preserved. Database loss or a connection failure never
selects a fresh/local store. Fresh initialization is an explicit operator
command and rejects evidence of retained installation resources; it does not
restore a lost registry or payload.

## Evidence and practical limits

| Fixture or suite | What it proves | What it does not prove |
| --- | --- | --- |
| Rust workspace with disposable PostgreSQL | Concurrent ingestion, exact owner isolation, immutable snapshots, stale-claim fencing, invalid definitions, pending-create barriers, transient retry, API/session/policy/lease/journal invariants | Complete application functionality through all external services |
| `tests.personal_hermes.kubernetes` | Real Kubernetes create-before-UID-save recovery, copied-proof/foreign UID rejection, bounded preconditioned updates/deletes, finalizer cleanup, actual bootstrap/admission, compiled controller SIGKILL/reopen and policy activation | Running native application by itself; the base fixture uses an unavailable application image |
| `tests.personal_hermes.kubernetes --native` | Two real Hermes instances, image/admin updates, Pod restart, owner separation, removal/re-add and retained state; native profile selection and plugin HTTPS requests; three allowed workers across two backend types/owners, denied backend access, real SSH/file transfer, wrong-host-key rejection, PVC subpath isolation and all three worker retirement/absence receipts | Real Keycloak browser login, routed chat, live model calls, cron side effects or backup/restore |
| `tests.personal_hermes.native_runtime --build` | Reviewed runtime image; non-root gateway/dashboard, private API denial, private/public CA handling, init-only key isolation, managed config precedence and state after forced restart; bind-mounted initialization result handling | Full cluster/provider acceptance |
| `tests.personal_hermes.native_owner` | Pinned real Hermes self-hosted provider with signed token fixtures; exact subject/issuer on login/session/refresh and legacy unpinned compatibility | An actual Keycloak token exchange or full browser session |
| `tests.personal_hermes.api_process` | Compiled API/router-auth listener/configuration gates, abrupt process termination and registry reopening | Full Kubernetes/execution lifecycle |
| `tests.personal_hermes.router_compatibility` | Real pinned Traefik/Yaegi, two fixture backends on the same hostname/path, forged-header rejection and open-WebSocket revocation | Combined Rust/Keycloak/native login, uploads, cookie handling or production route publication |

Latest verification snapshot: **168 standard Rust tests and 116 Python tests
passed**, alongside workspace formatting/Clippy, Helm lint, privacy preflight and
two explicit live Kubernetes tests and the complete extended native/profile/SSH
acceptance fixture. The rebuilt native runtime smoke fixture also passed,
including its non-root SSH client check. Regressions cover native readiness with
pending/active workers and acquisition while a readiness observer holds its
claim, preserving the observer's fence and deadline. The three live
Kubernetes/native Rust tests are ignored in the standard Cargo invocation and
run through the explicit Python fixture. These results do not complete every
P01–P18 acceptance requirement in the specification.

## Verification commands

Use Python 3.14, `uv sync --locked`, Helm 3, Docker and the pinned toolchain in
`services/hermes-manager/rust-toolchain.toml`. Run from the repository root unless
shown otherwise. Create a disposable PostgreSQL fixture before the full Rust or
Kubernetes/process tests; never point these tests at deployment data:

```bash
docker run --detach --name personal-hermes-manager-tests \
  --label agent-platform.io/fixture=personal-hermes-postgres \
  --publish 127.0.0.1:32814:5432 \
  --env POSTGRES_USER=hermes_test --env POSTGRES_DB=hermes_test \
  --env POSTGRES_HOST_AUTH_METHOD=trust \
  postgres@sha256:2a0d0fe14825b0939f78a8cad5cd4e6aa68bf94d0e5dd96e24b6d23af4315545
export DATABASE_URL=postgres://hermes_test@127.0.0.1:32814/hermes_test
```

Wait for that fixture's `pg_isready` before testing. SQLx uses a test-only role
with CREATE DATABASE to create/migrate isolated databases. The process and
Kubernetes runners default to this named Docker container and use temporary
credentials; they do not read an existing kubecontext.

```bash
uv sync --locked
uv run ruff check agent_platform tests services/tools/platform_bootstrap
uv run ruff format --check agent_platform tests services/tools/platform_bootstrap
uv run pytest -q
helm lint charts/agent-platform
uv run python -m agent_platform.privacy

cd services/hermes-manager
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --bins --locked
cd ../..
uv run python -m tests.personal_hermes.api_process
uv run python -m tests.personal_hermes.native_runtime --build
uv run python -m tests.personal_hermes.kubernetes --native
```

Run Cargo builds/tests sequentially against a shared target directory. The native
Kubernetes fixture uses the image built by `native_runtime --build`. Native-owner
and router-compatibility fixtures are additional targeted checks for changes to
those components. Clean up only the exact disposable PostgreSQL fixture and its
test volume after verification; the other runners remove their own temporary
containers. Keep fixture credentials out of logs and release inputs.

## Remaining work

1. Resolve approved runtime Service/Endpoint egress and verify wider CNI
   isolation; nonempty runtime service egress still prevents activation. The
   native-to-worker selector, owner-scoped SSH ingress and controller SSH probe
   path are implemented.
2. Finish full API-managed native/terminal lifecycle completion and broader
   failure/recovery acceptance. Preserve original drain deadlines and request/
   lease tombstones. Finish policy retry/deadline/manual-recovery status.
3. Complete route publication and real Keycloak/native/proxy browser acceptance,
   including HTTP mutations, uploads, cookies, identity changes and WebSockets.
4. Implement the manager frontend and remaining owner/checkpoint contracts using
   the existing administration authorization and CSRF boundary.
5. Implement checkpoint/inventory/readback receipts, full data restore and
   explicit adoption; supply backup transport and scheduling from infrastructure.
6. Package the new services and personal-mode chart configuration, then complete
   release acceptance. Updating source is separate from deployment/publication.

## Paused handoff — 2026-10-20

The earlier pause ended when native commissioning work resumed. That checkpoint
ended at migration 0016 and is preserved in Git history; it is not the current
status or resume instruction. Migration 0017, explicit fresh initialization and
integrated native commissioning described above supersede its native-instance
and fresh-inventory gaps. Terminal, routing, UI and coordinated recovery work
remain incomplete as listed here.
