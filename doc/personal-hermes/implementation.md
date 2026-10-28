# Personal Hermes implementation status

What is built and currently tested. The source implements the personal Hermes
manager, gateway and runtimes described in the [specification](../SPEC.md), the
[architecture](../ARCH.md) and the guides for [instances](instances.md),
[terminal backends](terminal-backends.md) and the [dashboard](dashboard.md).
[`TESTS.md`](../../services/hermes-manager/TESTS.md) maps every requirement,
acceptance scenario and contract operation to named tests and their levels;
every row currently records `pass` for its cited examples. The
[coverage assessment](../testing/coverage.md) identifies the additional
clause, boundary and security evidence needed beyond those examples.

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
| Rust workspace (`cargo test --workspace`, with disposable PostgreSQL) | Domain planners and validators; port contract cases on the in-memory and PostgreSQL stores; contract-checked administration, execution, resolver and gateway responses over real TLS; OIDC sessions against a fake realm; gateway hygiene and revocation timing; binary roles, configuration gates and abrupt kills; with `fault-injection`, the API aborted right before and right after a commit | Behaviour on a real cluster or with real Hermes |
| `tests.personal_hermes.kubernetes` | Server-side apply, admission and RBAC as the controller's real ServiceAccount on k3s; preconditioned deletes; leader election; the controller process aborted at every controller fault point, including right after a delete; a real `agent-runtime` worker with the instance's keys; an unpullable worker image reported as `image_unavailable`; the owned objects the orphan scan lists | Real Hermes instances, Keycloak, the gateway on a cluster |
| `tests.personal_hermes.kubernetes --native` (`native_e2e`) | The chart installed with its operator Jobs; real Hermes instances commissioned from published versions through the administration API; two owners on one hostname through the gateway with Keycloak and the native owner pin; denial and no enrollment without an assignment; distinct credentials; overlapping paths refused; a missing static directory blocking the instance until it exists; chat over the native WebSocket against an in-cluster fake model, with the template's and assignment's settings enforced over the owner's and the owner's own settings kept; the model key delivered only to the referencing instance and never returned; uploads in the owner's private home only, bounded by the gateway; a cron job firing without browser traffic and running its command in a terminal backend; two backends sharing the workspace, a backend-only static fixture home mounted only in one and the private home in neither; idle workers retired; a backend change during an active lease; a private registry backend with wrong credentials reported and right ones pulling; foreign cookies, tickets and forged headers refused; an email change at the provider; the manager, the database and the provider unavailable, and a person disabled at the provider, denied under the current test thresholds; suspension closing an open WebSocket and draining a pending worker; data and claims kept on re-enable; only the controller holding cluster rights, an execution credential refused by the administration API and the gateway, and no secret in any log; a restore onto a new cluster with `rebind-cluster`, static volume data and the original material; an instance missing from an older backup reported as orphans and left alone | A real model provider, NFS sources and multi-node scheduling |
| `tests/test_manager_chart.py` | The chart's admission policies and RBAC equal the renderer's; generated role configurations are the ones the Rust parsers load; pod security, NetworkPolicy peers, strict values and operator Jobs | A live installation |
| `tests.personal_hermes.native_runtime --build` | Reviewed runtime image; non-root gateway and dashboard, private API denial, CA handling, init-only key isolation, managed configuration precedence and state after a forced restart | Cluster or provider acceptance |
| `tests.personal_hermes.native_owner` | The pinned Hermes provider requires the mounted owner policy and accepts only the exact issuer and subject on login, session and refresh; missing or malformed policy is refused, and environment pins cannot replace it | A real Keycloak exchange |
| Dashboard (`ui`) | Unit tests, Chromium presentation workflows with mocked APIs, and a separate real-server production-build/CSP browser case | A real browser Keycloak login and dashboard-driven commissioning on the installed chart |

## Testing strategy and coverage limits

The [testing strategy](../TESTING.md) specifies language-native units,
integration tests at their proving boundary, and independently collected
Python/pytest E2E on disposable k3d clusters. The
[coverage assessment](../testing/coverage.md) inventories current evidence
against every requirement, acceptance row and operational bound. The
[gap register](../testing/gaps.md) specifies the missing tests and fixture/runner
changes. These documents distinguish required test-system behaviour from
features that are already implemented.

Today, `uv run pytest -q` collects the Python test modules, not the standalone
native/system scripts. CI's cluster driver defaults to direct k3s;
`kubernetes.py --k3d` is optional and the restore code still creates direct k3s.
The existing system script is one sequential run. It proves selected examples,
not complete per-clause/security-bound coverage. In particular, live NFS,
multi-node policy enforcement, a real cluster browser commissioning journey
and full private-home restore are not established by the evidence table.

The commands below describe the current implementation. They are not the
proposed pytest/k3d invocation surface, which does not exist yet.

The [collected-case verifier](../../tests/verification/README.md) provides
working Python/Rust inventory and execution commands with exact profile gates
and sanitized JSON/JUnit reports. It accounts for compiled shared-store cases,
fault-enabled process cases and explicitly ignored browser/cluster cases.
Standalone system helpers remain marked uncollected; TypeScript retains its
separate gates. This is baseline execution evidence, with clause and scenario
coverage limitations still recorded in G01/G03.

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
part. It keeps the cluster after failure and disables test-database cleanup;
a successful run still removes its cluster. `--reuse <container>` resets and
reruns on it. Use reuse only with disposable run-owned fixtures; its broad
reset/cleanup scope is recorded in G04 of the [gap register](../testing/gaps.md).
Clean up only the exact disposable PostgreSQL fixture after verification. Keep
fixture credentials out of logs and release inputs.
