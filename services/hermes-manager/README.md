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

The `hermes-manager` executable implements `api`, `migrate` and `check` roles.
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
the registry before binding its two TLS listeners. It can remain available for
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
These services do not yet commission Kubernetes resources or ship in the chart.
