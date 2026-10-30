# Testing

The repository runs Rust, Python and TypeScript tests, plus disposable Kubernetes
and browser fixtures. [SPEC.md](SPEC.md), [ARCH.md](ARCH.md) and the
[OpenAPI contracts](../contracts/hermes-manager/) define product behaviour.
The [coverage assessment](testing/coverage.md) records the evidence and its
limits; the [gap register](testing/gaps.md) holds test work still to do.

The runnable commands are in the
[implementation guide](personal-hermes/implementation.md#verification-commands),
the [dashboard guide](personal-hermes/dashboard.md#frontend-development-and-checks) and the
[case verifier guide](../tests/verification/README.md). These commands use
fixtures, never deployment data or a deployment kubecontext.

## Evidence records

| Record | Contents | Enforcement |
| --- | --- | --- |
| [`TESTS.md`](../services/hermes-manager/TESTS.md) | Requirement, acceptance, bound and composite contract-operation rows; named tests, levels and status | Architecture tests compare row sets with the specification and contracts and check that named functions exist |
| [`cases.json`](../tests/verification/cases.json) | Exact Python and compiled Rust test identities, proving levels and execution profiles; explicitly uncollected helpers | `tests.support.verification` compares actual collection and execution against the reviewed inventory |

A `pass` traceability row identifies cited examples. It is neither exhaustive
clause coverage nor an execution receipt for a candidate. JSON and JUnit reports
from the verifier record the outcomes of a particular profile run. The coverage
assessment and gap register review these records rather than duplicating the
case inventory.

## Levels and ownership

Tests are classified by the boundary they exercise. Real PostgreSQL, TLS, SSH,
process, browser and Kubernetes tests are integrations even when they live in a
Rust library test target.

| Level | Implementation | Boundary |
| --- | --- | --- |
| Unit | Cargo, pytest and Vitest | Pure validators, planners, state transitions, command decisions with fakes, configuration and UI logic |
| Integration | Cargo and pytest; TypeScript Playwright for presentation workflows | Store contracts, real protocol listeners, process failures, runtime images, chart/render comparisons, admission/RBAC and production dashboard/CSP |
| E2E | Independently collected pytest cases and the standalone native script | Helm-installed manager, gateway, Hermes, workers, PostgreSQL and Keycloak, with a fake model and fixture registry |

One shared Rust contract suite runs against both the in-memory and PostgreSQL
stores. Adapter-specific suites exercise other ports. There is no generic
conformance suite for every port. Finite planner state-grid and deterministic
command-sequence tests exist; no `proptest`, fuzzing or measured branch-coverage
pipeline is configured.

## Execution profiles

The verifier supports these profiles from the repository root:

| Profile | Selection and prerequisites |
| --- | --- |
| `python` | Tests without infrastructure capability markers; Helm is needed by chart tests |
| `python-native` | `native` cases against `HERMES_TEST_NATIVE_IMAGE` |
| `python-cluster` | `cluster and not destructive`; `HERMES_TEST_K3D` names the pinned k3d binary; browser cases also need Chromium and NSS `certutil` |
| `python-destructive` | `destructive` cases on a separate platform pool; requires `HERMES_TEST_K3D` |
| `rust` | Non-ignored workspace tests, including PostgreSQL integrations; requires disposable `DATABASE_URL` |
| `rust-faults` | Feature-enabled API process abort cases; requires disposable PostgreSQL |
| `rust-browser` | Ignored production dashboard/CSP case; requires PostgreSQL, `MANAGER_UI_DIST`, UI dependencies and Chromium |
| `rust-cluster` | Ignored Kubernetes adapter/admission cases; the cluster driver supplies `HERMES_TEST_KUBECONFIG` and worker-image inputs |
| `rust-controller` | Ignored feature-enabled controller process cases; the driver supplies PostgreSQL and Kubernetes fixture inputs |

`inventory` checks collection only. `run` also requires every reviewed case to
pass and the underlying runner to exit successfully. Empty collection,
missing/unexpected cases, wrong levels, skips, xfail/xpass and fixture failures
fail the profile. Rust inventory compiles and lists test binaries without
starting their external fixtures. Required execution inputs are checked before
running a profile.

A plain `uv run pytest -q` uses the same infrastructure-free selection as
`python`. Every Python test has exactly one level marker; collection rejects
missing or ambiguous levels. `pyproject.toml` registers capability markers and
enables strict marker/configuration checks. Requirement/case markers are not
comprehensively mapped. Browser, database and multi-node capabilities have no
separate verifier profiles.

## Cluster fixtures

There are two cluster execution paths:

- `tests.personal_hermes.kubernetes` starts direct single-server k3s by default,
  with a private temporary kubeconfig. It runs the Rust adapter/admission and
  controller process profiles. Its optional `--k3d` path creates a k3d cluster.
  `--native` then installs the chart and runs the sequential native scenario.
- `tests/support/cluster.py` creates run-owned k3d clusters for pytest. The
  session `cluster` fixture has one server and two agents and proves
  NetworkPolicy enforcement with a working allowed connection and a denied
  connection before handing the cluster to a case. The installed `platform`
  fixture has a separate single-node cluster because its local static volumes
  and dynamic storage must be mountable together on one node.

Both paths pin the k3s image and keep kubeconfigs private. The pytest path
requires k3d v5.9.0 explicitly through `HERMES_TEST_K3D` and disables changes to
the default kubeconfig. Fixture images are built/imported locally or pulled
from pinned references. Fixture hostnames use a generated CA. The real-browser
helper trusts that CA in a private NSS database and uses explicit resolver
rules; it disables Chromium's Local Network Access checks for the private
fixture address.

The restore helper creates a second direct-k3s cluster, including when called
from a pytest case whose source platform uses k3d. It restores a PostgreSQL dump
and an archive of static fixture storage, runs `rebind-cluster`, and checks
identity/material retention and orphan reporting. It does not establish
recovery of dynamic private-home data across provisioners. No live NFS fixture
or multi-node acceptance run of the installed platform exists.

## Fixture lifecycle and isolation

`tests/support/ownership.py` checks resource kind and run labels before reset,
injection or cleanup. Database cleanup checks run ownership comments. The
`Cleanup` helper registers finalizers as resources are allocated and attempts
all registered cleanup actions even after a failure. Docker inspection failures
are errors, never evidence that a resource is safe to remove.

The non-destructive pytest cases share a commissioned platform and create
additional assignments as needed. Teardown suspends extra assignments to free
Pods while retaining data. They do not have separate installation/database state
per case. `HERMES_TEST_ORDER=reverse` reverses collection order for diagnosis.

Destructive cases take turns on their own session platform pool. After each
case the pool checks health and replaces a failed or unusable platform before
another case takes it. This pool is separate from the non-destructive platform.
The standalone native script has a sequential scenario order and shares its
run's mutable state. It pauses its PostgreSQL fixture for an outage case, so
only one such run may use that fixture at a time. `--keep` and `--reuse` support
local diagnosis with verified fixture resources.

## Tests and proving boundaries

| Area | Existing cases and limits |
| --- | --- |
| Commands and stores | Replay/conflict, stale revisions, concurrency, rollback, access changes, secret references, leases and controller state; memory and PostgreSQL share the contract cases |
| Protocols | Administration, execution, resolver and gateway fixtures check declared responses and selected malformed headers/bodies, body bounds, authentication, CSRF and wrong-owner/wrong-interface refusals; complete operation/status/header coverage remains in G21 |
| Storage and initialization | Approved roots, traversal, static sharing, read-only mounts, mount-path compatibility, dynamic claims, private-home isolation, retained state and interrupted initialization; live NFS, more permission variants and private-home restore remain gaps |
| Worker identity | Domain range validation, backend UID/GID/fsGroup rendering, real SSH against the declared UID, and worker/Hermes admission cases; native alternate-identity file ownership and image mismatch execution remain in G23 |
| Browser | TypeScript workflows use mocked APIs; the Rust production browser case uses real assets/CSP with fixture sessions; pytest browser cases use the installed chart and Keycloak |
| Failures and bounds | API/controller fault injection, leader turnover, worker loss, provider/database outages and selected timing boundaries; no full interleaving, capacity or sustained soak claim |

The standalone native script and pytest acceptance wrappers share scenario
helpers. Pytest provides individual node IDs and reports for those wrappers;
the standalone script reports aggregate progress and stops at its first failure.
The [coverage assessment](testing/coverage.md) identifies the uncovered
variants without treating helper existence as a passing result.

## Reports and diagnostics

Verifier reports contain source revision, dirty-tree status, catalogue digest,
profile, start/end times and per-case outcomes. Python records each case's aggregate setup/call/teardown duration; Rust per-case
durations are not measured. A sibling JUnit XML report
is written for execution. Captured output, environments and failure bodies are
omitted; unreviewed identities are hashed before reporting. CI uploads these
reports with seven-day retention.

These reports do not contain a complete tested artifact-digest manifest or
exhaustive clause/actor/header/timing coverage. The standalone native run and
TypeScript suites retain their own output; TypeScript identities are outside
the reviewed case ledger. Timing cases have explicit waits and observation
budgets, with incomplete production-boundary coverage recorded in G06.

## CI and releases

| Workflow | Trigger and execution |
| --- | --- |
| `Checks` (`checks.yml`) | Pull requests and pushes to main: Python/chart/privacy, Rust and shared-store profiles, dashboard unit/browser/build checks, manager image checks, runtime checks, direct-k3s adapter/controller profiles and the full standalone native scenario |
| `Cluster scenarios` (`cluster.yml`) | Manual dispatch: builds fixture images, then runs `python-cluster` and `python-destructive`, including the network matrix and installed browser journeys |
| `Release agent platform` (`release.yml`) | Explicit version tag: first requires successful latest `Checks` and `Cluster scenarios` runs for that exact commit, then validates, builds and publishes images and their matching chart |

The manually dispatched cluster profiles are not automatic PR gates. There is
no scheduled extended/NFS/soak workflow. The release gate compares workflow runs
by source commit; it does not compare the digests tested in those runs with the
digests built for publication. The release builder performs its own image and
packaged-chart validation. G20 records the remaining artifact evidence; G22
records the remaining profile and reporting work.
