# Personal Hermes manager development

This workspace implements the shared manager described in the
[specification](../../docs/personal-hermes/specification.md). It is under
development and is not installed by the 0.6.1 chart.

`hermes-domain` contains exact identity, lifecycle, naming and mount invariants.
`hermes-application` authorizes assignment commands and defines transactional
persistence and operation-journal ports. `hermes-adapters` implements those
ports using SQLx/PostgreSQL. Domain/application code imports no HTTP, identity
provider or Kubernetes SDK. HTTP authentication must produce a cryptographically
verified principal; request-supplied roles or decoded, unverified tokens cannot
construct one at a network boundary.

Assignment commands atomically commit desired state, one stable instance ID,
idempotency receipt, actor-attributed audit event and a durable operation.
Changing access or retrying advances the desired generation and fences older
operations. Retry preserves current access, identity, instance and data. Enabling
an assignment enqueues commissioning without waiting for login.

The PostgreSQL adapter uses bounded connection/statement/lock deadlines and
requires durable server settings. Migrations and installation initialization are
explicit operator operations. Existing-mode checks reject missing/mismatched
installation identity, epoch, migration history, unsupported schema, restore
mode and unlogged control tables. Fresh initialization requires evidence from
the trusted external inventory adapter; the real Kubernetes/storage scanner
must still be implemented. Database failures never select a fresh/local store.

Operations use database-time leases, monotonically increasing fencing tokens,
immutable step intents and verified outcomes. An uncertain initializer blocks
for review. A delete result must prove absence of the exact recorded UID.
Journal completion alone does not publish readiness. Kubernetes ownership and
admission enforcement remain separate acceptance gates.

Use the pinned Rust toolchain from `rust-toolchain.toml`:

```bash
cd services/hermes-manager
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --lib
```

The full test suite requires a disposable PostgreSQL 17 database and a test-only
role that can create databases. SQLx creates isolated databases, runs the actual
migrations and cleans up successful tests. Missing `DATABASE_URL` fails rather
than silently skipping the database tests. Never point this at deployment data.

```bash
DATABASE_URL=postgres://hermes_test@127.0.0.1:5432/hermes_test \
  cargo test --workspace --locked
```

The Checks workflow supplies the pinned PostgreSQL fixture and runs formatting,
Clippy and all tests. The [implementation evidence](../../docs/personal-hermes/implementation.md)
tracks the broader service, router, UI, execution and recovery work.

## API process and authentication

The `hermes-manager` executable implements `api`, `migrate`, `check` and
`controller` roles. The controller has a separate configuration described below.
Use the [configuration reference](api.example.json) with deployment-owned Secret
files. Configuration accepts file references, never inline database credentials
or TLS keys. The manager does not print underlying configuration/database/TLS
errors. No Kubernetes client or token is used by the API process.

```bash
cargo build --bin hermes-manager --locked
./target/debug/hermes-manager migrate --config /run/manager/config.json
./target/debug/hermes-manager check --config /run/manager/config.json
./target/debug/hermes-manager api --config /run/manager/config.json
```

These commands do not bootstrap an installation or adopt retained storage.
`migrate` explicitly applies versioned migrations; existing registries require
matching installation/issuer/epoch, and pending upgrades require maintenance.
`check` requires supported migrations and an active installation. `api` validates
the registry before binding its administration and resolver TLS listeners. An
optional `execution` configuration binds a third TLS listener using an independent
certificate/key file pair; omitting it leaves the native execution API unbound. It can remain available for
authentication/lifecycle coordination during maintenance/restore, while all
assignment mutation and routing transactions retain their fail-closed gates.
The trusted fresh inventory and checkpoint/restore adapters are still required
before a deployment can initialize or recover a real installation.

The administration listener implements the
[administration contract](../../contracts/hermes-manager/openapi.yaml), including
paginated identities/assignments/audit, approved templates, instance/operation
metadata and audited create/access/retry commands. `If-Match` uses the quoted
version ETag and every mutation requires an `Idempotency-Key`. An omitted
template version is pinned in the transaction and remains stable on replay.
Unknown command fields, ownership transfers and arbitrary workload specifications
are rejected.

Execution administration also exposes the contract's terminal-runtime catalog,
immutable runtime versions, owner storage bindings, pull-Secret references and
assignment execution policy. Catalog and version pages use exclusive string and
integer cursors respectively; storage pages require an exact `instanceId` and
use UUID cursors. Every page is bounded to 100 entries (50 by default). Storage
registration starts at version 1; PATCH requires its strong `If-Match` ETag and
appends version N+1. Binding ownership cannot transfer, and existing policies
keep their pinned binding versions until a separate policy update.

`GET/PATCH /api/v1/assignments/{id}/execution` uses the assignment ETag. PATCH
resolves only approved runtime versions and that instance's registered storage
and pull-Secret references; no raw Pod/PV specs, credential values or caller
selected worker image enter the policy request. The response distinguishes
desired from applied generation. Migration 0016 permits trusted controller
admission/storage activation; registering configuration does not make
an instance Ready or publish a route. Execution mutations use the same current
human administrator authorization and browser Origin/CSRF checks as assignments,
with actor/body-scoped idempotency and durable execution configuration audit.
Real PostgreSQL/HTTPS tests cover owner isolation, version/replay conflicts,
bounded pagination, unknown/duplicate fields, rejected opaque native credentials,
CSRF and administrator-role loss on session refresh.

The optional execution TLS listener implements the separate
[native protocol contract](../../contracts/hermes-manager/execution.openapi.yaml).
It accepts only the exact instance's registered opaque execution credential and
profile; JWTs and browser cookies grant no execution access. Native credentials
grant no administration or router access. Acquisitions return pending while
applied policy or trusted worker readiness is missing, and disabling an
assignment invalidates its execution credential immediately. The execution HTTPS
tests use actual encrypted bootstrap/client registration and desired policy;
their separate historical Pending inventory fixture proves redacted owner
listing without setting Ready/applied state or granting a lease.

API-only authentication uses signed Keycloak user access tokens. Require
the configured issuer/audience, approved authorized client, a user-session `sid`,
`Bearer` token type and the exact administrator role under the management client.
Disable service accounts on management/router OIDC clients. ID tokens,
client-credentials tokens, unverified headers, personal assignments and general
realm roles cannot confer administration. JWT algorithms are pinned (the process
currently selects RS256), and keys come from a configured HTTPS JWKS endpoint
on the issuer origin. Key fetches are bounded, size-limited and do not follow
redirects or environment proxies. The key cache is independent of assignment
authorization. Optional browser authentication is described below; static UI
serving remains in development.

The resolver listener requires client mTLS AND fresh user access-token evidence
for its separate route audience/client. Its peer role comes from an exact
verified certificate URI SAN, never a forwarded certificate header. It exposes
no administration/execution endpoints. Every resolution rechecks current
assignment authorization and registered destination ownership/generation, with
an overall deadline below two seconds and no-store responses.

Seven HTTPS integration tests use real certificates, real RSA signatures, a
bounded TLS JWKS fixture and PostgreSQL. They cover role/audience/client/issuer
separation, invalid/expired/stale signatures/evidence, wrong mTLS identities,
missing/forged authentication, API-only mutations, races, unknown/oversized input,
live revocation, maintenance/recovery gates and dependency/lock deadlines.
The process test also runs the actual executable and kills it without shutdown
hooks before reopening the same registry:

```bash
uv run python tests/personal_hermes/api_process.py \
  --postgres-container personal-hermes-manager-tests
```

Run this command from the repository root after building the binary. Its
PostgreSQL container is a disposable fixture using test-only trust authentication.
It creates and removes its own database and all certificate/configuration files.

## Browser sessions and router auth process

The optional `admin.browser` configuration enables `/auth/login`,
`/auth/oidc/callback` and `POST /auth/logout`. Removing it leaves bearer API
authentication available. Browser sessions authenticate the same administration
API; `/api/v1/session` returns the actor and CSRF token, never provider tokens.
Mutations require the exact configured `Origin` and `X-CSRF-Token`. Cookie and
bearer credentials cannot be combined. Hostnames must differ between personal
and administration origins; different ports do not isolate cookies.

`hermes-router-auth --config /run/router-auth/config.json` runs the separate
session-only process. See [its example](router-auth.example.json) and
[contract](../../contracts/hermes-manager/router-auth.openapi.yaml). Public
login/callback/logout and `/_platform/me` are separate from native Hermes auth.
Its internal listener requires mTLS with distinct ForwardAuth and plugin URI
identities. Only ForwardAuth can obtain identity headers, and only the plugin
can re-evaluate the opaque hash handle for an established connection. Native
callbacks with missing sessions receive 401, never a replaying login redirect.
The plugin must still query the manager for assignment authorization.

Use `cargo build --workspace --bins --locked` to build both executables. The
compiled-process test covers their TLS/listener/configuration gates and abrupt
termination. The PostgreSQL/HTTPS tests cover code/PKCE/nonce verification,
single-use flows, browser binding, overlapping login, logout, CSRF, signature,
audience/role checks, refresh rotation and separate mTLS callers. These protocol
fixtures do not replace acceptance with real Keycloak and native Hermes.

Provider tokens, PKCE verifiers and nonce/CSRF material are encrypted with
XChaCha20-Poly1305 before database writes. Associated data binds the installation,
epoch, purpose, record type, record hash and key ID. The keyring is a bounded
JSON file with only references, for example:

```json
{
  "activeKeyId": "key-2026-10",
  "keyFiles": { "key-2026-10": "/run/secrets/session-keys/key-2026-10" }
}
```

Each referenced key is exactly 32 binary bytes. Provision separate keys for
administrator and router sessions. To rotate, retain previous key references
until their sessions expire, then remove them. Tokens stay server-side; cookies
contain only independent random handles. Browser login increments a durable
generation and invalidates older sessions/callbacks. Absolute sessions expire
after eight hours; provider status/claims are refreshed after 60 seconds.
A refresh claim commits before contacting the provider. An interrupted or
uncertain rotating-token refresh requires a fresh login, rather than replaying
the token. Bounded periodic cleanup removes expired/terminal credentials.

Migrations mirror the installation epoch/mode into each authentication schema.
An old process configuration cannot authenticate sessions after an epoch change;
old records cannot become valid under the new epoch. Router authentication is
fenced during maintenance/restore. Administrator login and logout remain usable
for recovery, while registry mutations retain their own maintenance gates.

Provision separate database roles after migrations. The router session role
needs only `USAGE` on `manager_router_auth`, `SELECT/INSERT/UPDATE/DELETE` on
that schema's `browsers`, `flows` and `sessions`, and `EXECUTE` on
`manager_router_auth.check_scope(uuid,bigint,text)`. The administrator session
role needs equivalent grants in `manager_admin_auth`. Neither needs scope-table
write permission or assignment access. Schema owners/migration roles remain
separate. The narrow definer function validates and locks the epoch without
granting the session role permission to change it.

The router still needs native cookie stripping/filtering, coordinated native
logout/identity switching, owner status and full proxy/provider/native acceptance.
The controller commissions Kubernetes resources in the disposable fixture. These
services do not yet ship in the chart or provide complete personal-instance
lifecycle/routing readiness.

## Owned-resource reconciliation

The application resource reconciler now commits a resource reservation and its
external-action journal entry together before calling the Kubernetes adapter.
Creation provenance includes installation, cluster identity, instance, epoch,
generation, creation operation, namespace UID, purpose, retention and intended
manifest digest. HMAC-SHA256 authenticates that provenance. A same-named object
without the correct proof and intended fields cannot be adopted. After the first
UID binding, a different UID is always a conflict, even with copied annotations.

The Kubernetes adapter uses kube-rs with explicit TLS configuration or in-cluster
configuration. It does not infer a developer's current kubeconfig, run credential
plugins, impersonate callers, accept invalid certificates or retry uncertain
writes in its HTTP transport. Each external call has a bounded deadline. The
registered cluster identity is checked against the live `kube-system` namespace
UID; namespaced resources also require the exact registered parent namespace UID.

Updates observe the existing UID/proof and resource version, commit an immutable
update intent, and use UID/resource-version preconditions. A concurrent change
returns a precondition failure rather than applying a blind write. Resource
re-observation gives a retry a new immutable intent; older uncertain apply steps
link to that intent without asserting that the original write never happened.
Initializers and deletes do not use that replay path. Resource
version observation alone never publishes desired-spec readiness. Namespace and
home/workspace claims are retained; the ordinary retirement path cannot purge
them. Other resources receive an exact-UID deletion intent first, remain
`Retiring` while deletion is pending, and become tombstones only after verified
absence. Deployment deletion requests foreground propagation.

The initial verification key must be deployment-owned, distinct from browser
session encryption keys and included in coordinated recovery. Retain old
verification keys while live resources still use their proofs. Selecting a new
active key alone does not rewrite existing objects.

Run the explicit Kubernetes proof from the repository root:

```bash
uv run python tests/personal_hermes/kubernetes.py \
  --postgres-container personal-hermes-manager-tests
```

The fixture creates its own digest-pinned k3s container and temporary kubeconfig,
uses a loopback API port, runs the Rust integration test, then removes its
container/volumes and credentials. The normal Rust suite deliberately lists this
test as ignored; the explicit runner and CI execute it against a real API server.
It proves creation success before UID persistence, lost worker claim recovery,
UID/proof rejection, safe manifest updates, stale delete preconditions, pending
finalizers and absence-before-completion. A second test uses the actual
constrained controller ServiceAccount, verifies bootstrap/admission, commissions
an assignment from the PostgreSQL outbox and kills/reopens the compiled
controller between Secret creation and UID persistence. The fixture uses an
unavailable application image; creating resources does not prove a running
native instance or publish a route.

## Commissioning controller

```bash
./target/debug/hermes-manager controller --config /run/manager/controller.json
```

Run this role separately from the API. Its JSON configuration contains
`installation` (`id`, `epoch`, `issuer`), `databaseUrlFile`,
`materialEncryptionKeyringFile`, `ownershipKeyringFile`, `clusterIdentity`,
`executionUrl`, `workload` and `kubernetes`. Keys use the reference-only keyring
format above; material encryption and ownership proofs require independent key
bytes. `clusterIdentity` is the registered `kube-system` UID, not a context name.
`executionUrl` must be an HTTPS URL ending in `/execution`.

Choose `kubernetes: {"source":"inCluster"}` or an explicit
`{"source":"kubeconfig","file":"/run/manager/kubeconfig","context":"controller"}`.
The controller never infers a workstation context. An explicit kubeconfig must
use absolute certificate/token file references and cannot use executable
authentication, impersonation, a proxy or insecure certificate verification.
Startup checks the supported database history/installation/epoch, registered
live cluster identity, actual ServiceAccount and release-owned bootstrap policies.
It cannot install or relax those shared policies itself.

`workload` is the typed `WorkloadConfig` in `hermes-adapters/src/workloads.rs`:
control namespace/ServiceAccount/reconciler role, public HTTPS origin, native OIDC
client, execution port/public CA, optional private OIDC CA, exact service-egress
selectors and optional public HTTPS egress. Public certificate material is
validated. Instance templates must already have immutable approved definitions
whose typed digest matches the registered image and schema. Template registration
and chart configuration generation remain separate integration work.

The worker claims committed operations using PostgreSQL leases. It persists
encrypted create-once native credentials, then freezes the complete initial
rendering before dispatching child resource actions. Restarting with changed
public configuration cannot change an outstanding rendering or regenerate its
credentials. Namespace and child UID binding use authenticated creation proofs;
native workload admission derives from the same rendering and rejects foreign
images, credentials, mounts, commands, owner/generation labels and resource limits.

After applying resources it observes storage, Deployment/Pod ownership and the
signed initialization receipt. A bounded HTTP probe verifies the sole native
OIDC provider, login redirect and unauthenticated API denial through the owned
Service, with no redirects, proxy environment or cookies. Probe ingress permits
only controller/router components in the configured control namespace on the
native dashboard port. It persists specific waiting conditions and retains the
commissioning deadline. Passing these checks still waits for effective execution
policy; the combined path also needs full native/cluster acceptance. No route is
published by this implementation. Suspension/re-enable, periodic resync of
completed instances and the terminal lifecycle need combined acceptance; coordinated
recovery remains unfinished;
do not enable this development controller against an existing deployment.

The execution-policy foundation stores immutable runtime versions, owner-local
storage/pull references and resolved desired policy generations with audited,
idempotent optimistic mutations and independent fenced policy work. Applied
generation advances only through the trusted admission/storage activation port,
after prior-generation drain and exact Kubernetes readback. The scoped execution HTTP and durable lease implementation
are described above; production terminal grants still require the unfinished worker/controller and
authenticated SSH readiness integration.

## Retained lifecycle work

Access changes commit the original drain deadline before acknowledgement and
fence older controller claims. The controller closes Namespace admission before
scaling the retained native Deployment to zero. The scale subresource has its own
stop-only policy, so missing native admission parameters cannot prevent stopping.
Uncertain original creations are observed using the encrypted frozen rendering;
GET absence keeps their reservations for later recovery. These paths do not
create replacements or remove retained claims and credentials.

Every native Pod starts behind a scheduling gate. The controller verifies its
ReplicaSet and bound Deployment UID, persists the exact child identity and
external-action input, then removes the gate with UID/RV preconditions. Native
readiness requires that committed start. Stops target registered UIDs, and delete
acceptance does not prove absence. An old name cannot authorize deleting a new
Pod. Completed-instance resync uses a durable database cadence.

Terminal execution draining and verified lifecycle completion are still being
integrated. Native shutdown alone does not mark an assignment Suspended/Retained,
finish its original drain barrier, re-enable native work or publish a route.


Development is paused at the user's request. See the
[paused handoff](../../docs/personal-hermes/implementation.md#paused-handoff--2026-10-20)
for the exact implemented boundary, known gaps, verification commands and resume
sequence. The full specification remains open; this branch is not ready to deploy.
