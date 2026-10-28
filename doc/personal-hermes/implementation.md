# Personal Hermes implementation status

What is built and what proves it. The source implements every in-scope
requirement of the [specification](../SPEC.md), as described in the
[architecture](../ARCH.md) and the guides for [instances](instances.md),
[terminal backends](terminal-backends.md) and the [dashboard](dashboard.md).
[`TESTS.md`](../../services/hermes-manager/TESTS.md) maps every requirement,
acceptance scenario and contract operation to the tests that prove it and the
level they run at; every row is `pass`.

## What exists

| Area | Source |
| --- | --- |
| Hermes image and plugin | Pinned upstream Hermes with reviewed patches and the bundled `managed_execution` plugin; owner-pinned native OIDC; signed home-initialization receipt; protected terminal policy |
| Terminal worker image | `agent-runtime`, `managed-ssh-v2`, UID/GID 1000 |
| Manager | `services/hermes-manager`: the `api` role (administration API and dashboard, execution API, route resolver), the leader-elected `controller` role and the operator commands (`migrate`, `check`, `initialize`, `grant-administrator`, `rebind-cluster`, `generate-sealing-key`), all with state only in PostgreSQL |
| Dashboard | React and TypeScript, served by the `api` role under a strict CSP: users and the Keycloak directory, assignments with instance status and terminal environments, the template and backend catalogs with form, JSON and YAML editing, storage with orphans, secrets, administrators and the audit log |
| Desired state | Administrator commands through the command envelope: assignments, template and terminal backend versions (Draft, Published, Withdrawn), volumes from installation storage sources, admin settings and write-only admin secrets sealed to the controller |
| Reconciliation | Pure planners and server-side apply; create-once material; epoch-guarded status; preconditioned deletes; static admission policies and RBAC from the chart; orphaned namespaces and volumes reported, never adopted |
| Terminal backends | Environments and leases with database-time expiry; worker Pods with the instance's keys, the assignment's mounts and private registry credentials; in-process SSH readiness; pull, start and contract failures reported without replacement |
| Routing | `hermes-gateway`: OIDC platform sessions, route resolution over mTLS, forwarding to the owner's instance only, header and cookie hygiene, revocation of open WebSockets and streams |
| Chart | `charts/agent-platform` deploys `api`, `controller` and `hermes-gateway` with generated configuration, RBAC, admission policies, NetworkPolicies and one-shot operator Jobs |

Not part of the platform, by the specification's scope: coordinated
checkpoints, automatic backups (they are deployment-owned), SMB storage,
terminal service egress, worker prewarming, board integration, team instances
and a command that permanently deletes an instance's data.

An update or stop sends SIGTERM: the runtime gives its gateway and dashboard
20 s, the Pod grace period is 45 s, active agent runs can be interrupted, and
nothing rolls back automatically.

## Evidence

| Suite | What it proves | What it does not prove |
| --- | --- | --- |
| Rust workspace (`cargo test --workspace`, with disposable PostgreSQL) | Domain planners and validators; port contract cases on the in-memory and PostgreSQL stores; every administration, execution, resolver and gateway endpoint against its contract over real TLS; OIDC sessions against a fake realm; gateway hygiene and revocation timing; binary roles, configuration gates and abrupt kills; with `fault-injection`, the API aborted right before and right after a commit | Behaviour on a real cluster or with real Hermes |
| `tests.personal_hermes.kubernetes` | Server-side apply, admission and RBAC as the controller's real ServiceAccount on k3s; preconditioned deletes; leader election; the controller process aborted at every controller fault point, including right after a delete; a real `agent-runtime` worker with the instance's keys; an unpullable worker image reported as `image_unavailable`; the owned objects the orphan scan lists | Real Hermes instances, Keycloak, the gateway on a cluster |
| `tests.personal_hermes.kubernetes --native` (`native_e2e`) | The chart installed with its operator Jobs; real Hermes instances commissioned from published versions through the administration API; two owners on one hostname through the gateway with Keycloak and the native owner pin; denial and no enrollment without an assignment; distinct credentials; overlapping paths refused; a missing static directory blocking the instance until it exists; chat over the native WebSocket against an in-cluster fake model, with the template's and assignment's settings enforced over the owner's and the owner's own settings kept; the model key delivered only to the referencing instance and never returned; uploads in the owner's private home only, bounded by the gateway; a cron job firing without browser traffic and running its command in a terminal backend; two backends sharing the workspace, a network home mounted only in one and the private home in neither; idle workers retired; a backend change during an active lease; a private registry backend with wrong credentials reported and right ones pulling; foreign cookies, tickets and forged headers refused; an email change at the provider; the manager, the database and the provider unavailable, and a person disabled at the provider, each denied within the bounds; suspension closing an open WebSocket and draining a pending worker; data and claims kept on re-enable; only the controller holding cluster rights, an execution credential refused by the administration API and the gateway, and no secret in any log; a restore onto a new cluster with `rebind-cluster`, static volume data and the original material; an instance missing from an older backup reported as orphans and left alone | A real model provider, NFS sources and multi-node scheduling |
| `tests/test_manager_chart.py` | The chart's admission policies and RBAC equal the renderer's; generated role configurations are the ones the Rust parsers load; pod security, NetworkPolicy peers, strict values and operator Jobs | A live installation |
| `tests.personal_hermes.native_runtime --build` | Reviewed runtime image; non-root gateway and dashboard, private API denial, CA handling, init-only key isolation, managed configuration precedence and state after a forced restart | Cluster or provider acceptance |
| `tests.personal_hermes.native_owner` | The pinned Hermes provider requires the mounted owner policy and accepts only the exact issuer and subject on login, session and refresh; missing or malformed policy is refused regardless of environment markers | A real Keycloak exchange |
| Dashboard (`ui`) | Unit tests, Chromium tests, the production build under the real CSP | — |

## Verification commands

Use Python 3.14, `uv sync --locked`, Helm 3, Docker, Node from
`services/hermes-manager/ui/.node-version` and the toolchain in
`services/hermes-manager/rust-toolchain.toml`. Run from the repository root
unless shown otherwise. Create a disposable PostgreSQL fixture first; never
point these tests at deployment data:

```bash
docker run --detach --name personal-hermes-manager-tests \
  --label agent-platform.io/fixture=personal-hermes-postgres \
  --publish 127.0.0.1:32814:5432 \
  --env POSTGRES_USER=hermes_test --env POSTGRES_DB=hermes_test \
  --env POSTGRES_HOST_AUTH_METHOD=trust \
  postgres@sha256:2a0d0fe14825b0939f78a8cad5cd4e6aa68bf94d0e5dd96e24b6d23af4315545
export DATABASE_URL=postgres://hermes_test@127.0.0.1:32814/hermes_test
```

Wait for `pg_isready`. SQLx creates and migrates an isolated database per test.

```bash
uv sync --locked
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
uv run python -m agent_platform.privacy

cd services/hermes-manager
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p hermes-manager --features fault-injection --test process --locked
cd ../..
uv run python -m tests.personal_hermes.native_runtime --build
uv run python -m agent_platform.manager_build --tag hermes-manager:e2e
uv run python -m tests.personal_hermes.kubernetes --native --postgres-container personal-hermes-manager-tests
```

The dashboard checks are in the [dashboard guide](dashboard.md). The Kubernetes
runner starts its own digest-pinned k3s container with a temporary kubeconfig,
never reads an existing kubecontext, and removes the container afterwards.
`--native` adds the end-to-end run, which needs the two images built above and
Helm. It installs the chart with an in-cluster Keycloak and a fake
OpenAI-compatible model (`fake_model.py`), runs a digest-pinned registry
behind basic authentication (`registry_proxy.py`) next to the cluster, pauses
the PostgreSQL fixture once to prove the database outage bound, and restores
onto a second disposable cluster. For development,
`python -m tests.personal_hermes.native_e2e --keep` runs only the end-to-end
part and keeps a failed cluster, and `--reuse <container>` resets and reruns on
it.
Clean up only the exact disposable PostgreSQL fixture after verification. Keep
fixture credentials out of logs and release inputs.
