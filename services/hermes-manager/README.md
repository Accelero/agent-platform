# Personal Hermes manager (rewrite)

This workspace implements the target design described in:

- the [specification](../../docs/personal-hermes/specification.md);
- the [architecture](../../docs/personal-hermes/architecture.md);
- the [rewrite guide](../../docs/personal-hermes/manager-rewrite.md), which
  tracks the phases.

It replaces `services/hermes-manager/` at cutover. [`TESTS.md`](TESTS.md) maps
every requirement, acceptance scenario and contract operation to its tests.

| Crate | Role |
| --- | --- |
| `hermes-core` | Domain (pure) and application layer (commands, queries, ports, controller loop) |
| `hermes-wire` | Contract DTOs |
| `hermes-store` | PostgreSQL adapter and migrations |
| `hermes-render` | Pure Kubernetes and configuration renderers |
| `hermes-kube` | Kubernetes adapter |
| `hermes-identity` | Tokens, OIDC, sessions, TLS, directory |
| `hermes-native` | Sealing, init receipts, native probe, SSH readiness |
| `hermes-http` | Administration, resolver and execution routers |
| `hermes-proxy` | Gateway proxy core |
| `hermes-manager`, `hermes-gateway` | Binaries: configuration and wiring only |
| `hermes-architecture` | Dependency-rule and traceability tests |

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
commit. Integration tests that need PostgreSQL read `DATABASE_URL` and fail
when it is missing. Use the disposable fixture from the
[verification commands](../../docs/personal-hermes/implementation.md#verification-commands),
and never point tests at deployment data.
