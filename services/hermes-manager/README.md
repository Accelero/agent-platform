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

Current API-only authentication uses signed Keycloak user access tokens. Require
the configured issuer/audience, approved authorized client, a user-session `sid`,
`Bearer` token type and the exact administrator role under the management client.
Disable service accounts on management/router OIDC clients. ID tokens,
client-credentials tokens, unverified headers, personal assignments and general
realm roles cannot confer administration. JWT algorithms are pinned (the process
currently selects RS256), and keys come from a configured HTTPS JWKS endpoint
on the issuer origin. Key fetches are bounded, size-limited and do not follow
redirects or environment proxies. The key cache is independent of assignment
authorization. Browser OIDC sessions/CSRF and UI serving remain in development;
cookie-only API requests currently receive 401.

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
