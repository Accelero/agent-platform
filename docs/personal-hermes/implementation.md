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

## Current capability boundary

| Area | Implemented and verified | Remaining work or limitation |
| --- | --- | --- |
| Native commissioning | Infrastructure-owned template/user rows create one instance per exact installation issuer and Keycloak subject; image/config changes advance desired generations; removal retires the workload and re-add restores retained state | Infrastructure must deploy the manager and write its input rows; this is not Keycloak user synchronization or self-enrollment |
| Templates and image updates | Typed, digest-pinned deployment definitions, immutable catalog snapshots, per-user template selection and polling of database changes | No arbitrary Pod manifests, registry/tag watcher, automatic upstream release discovery or automatic image rollback |
| Native configuration and state | Separate home/workspace PVCs; missing-only initialization; create-once credentials; Hermes managed config overrides selected leaves while user config remains on the home PVC | No automatic storage-class migration, volume shrinking or permanent data purge |
| Shutdown during updates | Kubernetes Recreate rollout; runtime forwards SIGTERM to gateway/dashboard, waits up to 20 seconds and then kills remaining children; Pod grace period is 45 seconds | No update handshake that waits for agent runs to finish or acknowledges a completed application checkpoint; active work can be interrupted |
| Manager crash recovery | PostgreSQL action journal, leased/fenced claims, frozen renderings, authenticated resource provenance, exact UID/RV checks, uncertain-create recovery and periodic resync | Ownership/admission/manual-recovery faults remain blocked; a deleted or foreign resource is not blindly adopted or replaced; exhaustive crash testing at every external boundary remains open |
| Administration | Versioned API, Keycloak access-token validation, browser OIDC sessions, CSRF, audit, optimistic mutations and execution configuration endpoints | No manager admin frontend, generated React client, owner-status/checkpoint UI or completed directory integration |
| Terminal execution | Immutable runtime catalog, per-owner storage/pull references, desired/applied policy, scoped execution API and durable leases; worker rendering/admission/UID/start primitives | No complete automatic worker lifecycle: authenticated SSH readiness, networking, creation orchestration, idle/recycle/retirement and terminal drain still need integration |
| Routing | Rust resolver/auth processes, Go/Yaegi plugin and isolated Traefik/WebSocket compatibility proofs | Native commissioning does not publish routes; full Keycloak/native/browser/proxy integration remains open |
| Backup and recovery | Explicit fresh-install namespace/PV scan, schema/installation/epoch gates and retained ownership history | No scheduled backups, snapshot/copy jobs, backup retention, checkpoint coordinator or full data restore implementation; PVC retention is not a backup |
| Distribution and adoption | Locked source builds and CI checks; reviewed native runtime test image | No released manager/router-auth images or personal-mode chart installation; existing-owner adoption and release acceptance remain open |

The per-user native Hermes dashboard is supplied by Hermes and runs in each
instance. It is distinct from the unimplemented manager administration frontend.
Native `Ready` means the manager observed storage, the exact current workload,
signed initialization and the native HTTP/auth gate. It does not mean a terminal
backend is ready, a route was published or a real Keycloak browser login passed.

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
| `tests.personal_hermes.kubernetes --native` | Two real Hermes instances commissioned from SQL input using the constrained controller; image/admin updates, Pod restart, owner separation, mapping removal/re-add, retained PVC/credential identities and user payload/config | Real Keycloak browser login, routed chat, live model calls, cron side effects, terminal SSH or backup/restore |
| `tests.personal_hermes.native_runtime --build` | Reviewed runtime image; non-root gateway/dashboard, private API denial, private/public CA handling, init-only key isolation, managed config precedence and state after forced restart; bind-mounted initialization result handling | Full cluster/provider acceptance |
| `tests.personal_hermes.native_owner` | Pinned real Hermes self-hosted provider with signed token fixtures; exact subject/issuer on login/session/refresh and legacy unpinned compatibility | An actual Keycloak token exchange or full browser session |
| `tests.personal_hermes.api_process` | Compiled API/router-auth listener/configuration gates, abrupt process termination and registry reopening | Full Kubernetes/execution lifecycle |
| `tests.personal_hermes.router_compatibility` | Real pinned Traefik/Yaegi, two fixture backends on the same hostname/path, forged-header rejection and open-WebSocket revocation | Combined Rust/Keycloak/native login, uploads, cookie handling or production route publication |

Latest verification snapshot: **164 standard Rust tests and 114 Python tests
passed**, alongside workspace formatting/Clippy, Helm lint, privacy preflight,
two explicit live Kubernetes tests and the integrated real-native commissioning
fixture. The three live Kubernetes/native Rust tests are ignored in the standard
Cargo invocation and are run by the explicit Python fixture. The native runtime
fixture also passed. These results cover native commissioning; they do not
complete every P01–P18 acceptance requirement in the specification.

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

1. Complete the worker controller and authenticated SSH readiness before granting
   real terminal leases. Align the native egress selector (`hermes-worker`) with
   worker labels (`worker`), install owner-scoped SSH ingress, resolve approved
   Service/Endpoint egress and verify CNI enforcement. Nonempty runtime service
   egress currently prevents policy activation.
2. Finish exact-UID worker retirement, idle/recycle, later generations and the
   combined native/terminal drain barrier. Preserve original deadlines and lease
   tombstones. Finish policy retry/deadline/manual-recovery status.
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
