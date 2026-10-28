# Personal Hermes manager and gateway

This Rust workspace builds two binaries into one image:

- `hermes-manager`, whose `api` role serves the administration API, the
  dashboard, the execution API for the `managed_execution` plugin and the
  gateway's route resolver, and whose `controller` role reconciles instances
  and terminal environments as the single leader-elected writer to Kubernetes;
- `hermes-gateway`, the reverse proxy on the public Hermes hostname: platform
  sign-in through Keycloak, route resolution over mTLS, forwarding to the
  owner's instance only, and revocation of open connections.

What they must do is in the [specification](../../doc/SPEC.md), how they do it in
the [architecture](../../doc/ARCH.md). [`TESTS.md`](TESTS.md) maps every
requirement, acceptance scenario and contract operation to its tests. The
contracts are under [`contracts/hermes-manager/`](../../contracts/hermes-manager/).

## Crates

| Crate | Role |
| --- | --- |
| `hermes-core` | Domain (pure planners, validators, state machines, orphan detection) and application layer (commands and the envelope, queries, ports, controller loop, execution use cases); with the `testing` feature also the in-memory store, fakes and the port contract suites |
| `hermes-wire` | Contract DTOs, checked against the OpenAPI schemas |
| `hermes-store` | PostgreSQL adapter: repositories, read models, migrations with guard triggers, `LISTEN`/`NOTIFY`; runs the contract suites against PostgreSQL |
| `hermes-render` | Pure renderers: instance object set, worker Pod, managed configuration, admission policies and RBAC (golden files) |
| `hermes-kube` | Kubernetes adapter: bounded observation, server-side apply, preconditioned deletes, worker failures, owned-object listing, leader election |
| `hermes-identity` | Access tokens, OIDC flows, sealed sessions in PostgreSQL, Keycloak directory |
| `hermes-native` | Material and admin-secret sealing, init receipts, native probe, in-process SSH readiness |
| `hermes-http` | Administration, resolver and execution routers, TLS listeners, dashboard assets |
| `hermes-proxy` | Gateway core: hygiene, resolver client, caches, forwarding, WebSocket and stream supervision |
| `hermes-manager`, `hermes-gateway` | Binaries: configuration, roles and operator commands |
| `hermes-testkit` | Test-only fake OIDC realm, certificates and contract-checking client |
| `hermes-architecture` | Dependency-rule and traceability tests |

`crates/hermes-store/migrations/` holds the schema; `ui/` holds the dashboard
(see the [dashboard guide](../../doc/personal-hermes/dashboard.md)).

## Commands

`hermes-manager` reads one JSON configuration file, which the chart generates:

| Command | Purpose |
| --- | --- |
| `api` | Administration, execution and resolver listeners |
| `controller` | Leader election, reconciliation, lease expiry and the orphan scan |
| `migrate` | Apply the schema with the schema owner's database URL |
| `check` | Verify the schema and the installation without changing anything |
| `initialize` | Record the installation and the cluster identity in an empty database |
| `grant-administrator --identity-file <file> [--recover]` | Grant the first administrator, or restore a grant |
| `rebind-cluster` | After a restore onto a new cluster: record its identity and retire the old cluster's state |
| `generate-sealing-key --private-key-file <file> --public-key-file <file>` | Write the controller's X25519 key pair; never overwrites |

`hermes-gateway` takes its own configuration file. The
[chart guide](../../charts/agent-platform/README.md) lists every role's
configuration and Secret keys.

## Checks

```bash
cd services/hermes-manager
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p hermes-manager --features fault-injection --test process --locked
```

The `fault-injection` build aborts the process at the fault point named by
`HERMES_FAULT_POINT`; the process tests use it to kill the API around a
commit, and the k3s suite uses it for the controller. It is never part of a
release image. Integration tests that need PostgreSQL read `DATABASE_URL` and
fail when it is missing. Use the disposable fixture from the
[verification commands](../../doc/personal-hermes/implementation.md#verification-commands),
and never point tests at deployment data. The cluster and end-to-end suites are
described there too.
