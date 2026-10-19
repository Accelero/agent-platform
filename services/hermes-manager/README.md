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
