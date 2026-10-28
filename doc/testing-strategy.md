# Testing strategy

This specifies the required test system for personal Hermes: language-native
unit tests, dependency-focused integration tests, and independently collected
Python/pytest end-to-end tests on disposable k3d clusters. Python is the default
for new system-test orchestration and external-interface integration tests.

The decisions here are requirements for the test system, not a claim that its
implementation already exists. [Coverage](testing-coverage.md) records the
current evidence; [gaps and required changes](testing-gaps.md) specify the work.
The existing runnable commands remain in the
[implementation guide](personal-hermes/implementation.md#verification-commands).
Product behaviour remains defined by [SPEC.md](SPEC.md), [ARCH.md](ARCH.md) and
the [OpenAPI contracts](../contracts/hermes-manager/).

## Levels and ownership

Classify a test by the boundary it exercises, not its filename or runner.
A Rust test making real TLS requests or using PostgreSQL is an integration
test, even when Cargo runs it alongside pure tests.

| Level | Runner and language | Dependencies | Responsibility |
| --- | --- | --- | --- |
| Unit | Rust/Cargo for Rust; Python/pytest for Python; TypeScript/Vitest for UI logic | No external service or cluster; explicit fakes and controllable clocks | Validators, state transitions, pure planners, command decisions, retry rules, configuration overlays, serializers and UI state |
| Integration | Native Rust where generic traits, SQLx, adapter internals or process fault hooks are material; pytest/Python otherwise | A real PostgreSQL database, TLS/OIDC/SSH service, runtime image, browser, or Kubernetes API, as the case requires | Persistence and port contracts, actual request/response conformance, filesystem and image behaviour, admission/RBAC, browser/CSP integration and binary startup/crash behaviour |
| E2E | Python/pytest; Playwright's Python pytest integration for browser journeys | Helm-installed system on k3d, real manager/gateway/dashboard/Hermes/workers/PostgreSQL/Keycloak; deterministic fake model and private registry | Every acceptance scenario, public administration workflows, per-owner isolation, security boundaries, failure/recovery and restoration of retained state |

Preserve the shared Rust store contract cases and their in-memory/PostgreSQL
implementations. Do not duplicate domain algorithms in Python to move their
unit tests. Prefer Python for new black-box scenarios; do not rewrite effective
Rust integration tests just to change language. Existing TypeScript browser
tests with mocked API responses remain presentation integration tests. Real
cluster browser journeys belong to pytest and use the served production build.
[Playwright provides Python pytest fixtures](https://playwright.dev/python/docs/test-runners)
for isolated browser contexts.

## Coverage obligations

The unit of coverage is an observable obligation, not a test count or a green
requirement row. Break each R1–R19 requirement and each acceptance row into
behaviours, forbidden effects, boundaries and recovery outcomes. Operational
bounds and the prose sections of the specification are obligations too.

Each obligation records:

- its requirement/acceptance reference and a stable case ID;
- setup, action and the independently stated expected result;
- allowed actors and denied actors, including a second owner and an outsider;
- normal, invalid, boundary, unavailable and recovery variants where relevant;
- the lowest proving level, plus any necessary cluster/browser evidence;
- exact collected test identities, parameter IDs, required CI profile and result;
- current evidence, uncovered variants and a linked gap; a helper's existence
  alone is not execution evidence.

For HTTP, identify operations by **contract file and operation ID**, with method
and path as additional fields. The administration and gateway contracts both
define `POST /auth/logout`; their security cases must remain distinct. For each
operation account for declared success/error responses, authentication and
authorization, request/response schemas, required headers, body limits and
content types. A justified unreachable response is an explicit reviewed
exception, not a silently missing case.

Require full obligation mapping for the specification and all four contracts.
Use risk-based combinations rather than an enormous arbitrary Cartesian
product. Never reduce owner isolation, authentication, durable acknowledgement,
secret delivery, no-fallback behaviour or destructive-action guards to pairwise
sampling. Every relevant actor and boundary needs a direct case.

Examples of independent oracles:

- A rejected write leaves desired state, audit, receipt and Kubernetes resources
  unchanged; do not assert just the HTTP status.
- A replay produces the original result and no second audit/effect; record IDs
  before and after crashes and concurrent requests.
- An unassigned or foreign identity produces no namespace, material or worker.
- A restart preserves selected settings, profiles, sessions, file hashes,
  instance IDs and credential digests; readiness alone does not prove retention.
- A denied network connection is compared with a working allowed connection
  to the same live listener; a dead server is not an isolation oracle.

## Systematic cases by behaviour

| Behaviour | Required dimensions and invariants |
| --- | --- |
| Identity and authorization | Exact issuer/subject and case; same email with another subject; no assignment; every access state; missing/expired/forged/ambiguous credentials; current grants; browser CSRF/origin; credential used on another interface |
| Commands and concurrency | New/replayed/conflicting idempotency key; different actors; stale/missing/malformed precondition; concurrent enrollment, last-admin removal, publication, secret rotation and acquisition; rollback and lost acknowledgement |
| Planning and observation | Desired × observed states, including absent/stale/foreign/terminating objects, lost storage, bad receipts, transient probes and superseded generations; no unobserved or wrong-UID deletion; eventual convergence after recovery |
| Terminal execution | All three lifetime modes; default/allowed/withdrawn/removed/unknown selections; profile/session separation; pending/failed workers; host-key verification; replay/renew/release/expiry/parent leases; policy changes and forced drains; never replay interrupted commands |
| Storage and initialization | All supported sources; overlap and traversal; permissions/read-only; shared path and UID/GID; missing/lost volumes; interrupted/concurrent initialization; no reseeding or private-home mount in workers |
| Settings and secrets | Every precedence layer; removing an override; new profiles; required owner/terminal policies; image-plugin shadowing; env/file/registry delivery; reference changes, rotation, deletion, redaction and wrong sealing keys |
| Proxy and browser | Real login/logout/refresh; history, settings, upload, HTTP mutation and WebSocket; owner-only pending/error pages; forged routing/cookies/tickets; limits, streaming, cancellation and CSP |
| Stability | API/controller/gateway/PostgreSQL/Keycloak/worker failures independently and in selected combinations; leader turnover; lost notifications; bounded retries/concurrency; reconnect storms; cleanup after cancellation; recovery without identity or credential changes |
| Operations and artifacts | Fresh install, operator Jobs, supported upgrade, uninstall retention, same/new-cluster restore, missing/wrong backup inputs, sealed material, exact tested image/chart digests and no fault-injection feature in release images |

Each fixed defect adds the smallest deterministic regression case. Add a higher
level test when the defect depends on that boundary. Generate invalid and
boundary inputs, state sequences and concurrency schedules in native unit or
integration tests; use reproducible seeds and preserve minimized failures.
Mutation testing of selected authorization, owner, lease-finality and deletion
guards checks that the assertions detect a broken guard. It complements
coverage measurement; neither line coverage nor mutation scores prove security.

## Standard k3d environment

Use one shared fixture implementation for Kubernetes integration, E2E and
second-cluster restore. It creates a fresh, uniquely named k3d cluster and a
private kubeconfig. It must neither read a deployment context nor modify the
user's default kubeconfig. k3d exposes configuration for kubeconfig behaviour,
node topology, registry setup and port mappings; specify these explicitly in
the reviewed test configuration rather than relying on defaults.
[k3d configuration reference](https://k3d.io/stable/usage/configfile/).

| Profile | Topology and purpose |
| --- | --- |
| Core | One server; complete functional/security acceptance on every PR and main commit |
| Multi-node | One server and at least two agents; explicit placements for local-path volumes, cross-node NetworkPolicy and worker loss/rescheduling |
| Restore | A separate empty k3d cluster, private database and restored storage inputs; no access to the source cluster's live filesystem as a substitute for a backup |
| Extended storage | Multi-node plus an isolated real NFS service/export; validates mounts, permissions, failure and recovery |
| Soak/failure | Dedicated cluster, explicit resource budgets and longer test deadlines; no sharing with the core profile |

Pin k3d, the k3s node image, PostgreSQL, Keycloak, registry and test-probe images.
Import the exact locally built manager, Hermes and worker artifacts into every
node that may schedule them. Record digests and tool versions in the run
receipt; do not silently reuse whatever a mutable local tag currently names.
Build reviewed release contexts once and install the actual chart using those
digests. Fault-injection builds are separate, explicitly identified test
artifacts and cannot be published as release images.

Preserve admission and network enforcement. K3s includes a network-policy
controller; its presence must still be verified by a functional allow/deny
probe in the test environment.
[K3s network-policy controller](https://docs.k3s.io/networking/networking-services#network-policy-controller).
A profile unable to enforce the policy fails setup. Do not count manifest
comparison or a port-forwarded connection as a live policy test.

Give PostgreSQL and registry fixtures a private run-owned network and explicit
addresses usable by Pods and the driver. Use loopback-only host port mappings
and real test hostnames with an explicit fixture CA. Browser and API drivers
must use the same routing arrangement; a process-global Python DNS patch is
not a browser solution. Specify registry trust/authentication and mirror
configuration for every node, including the restore cluster.

The test administrator provisions infrastructure and only performs observation
or fault injection that an ordinary caller cannot perform. Product workflows
use public APIs, CLI operator commands and real role credentials. Never use
cluster-admin requests as proof that the controller's restricted ServiceAccount
can perform an action. Kubernetes assertions may inspect state but must not
repair a failing workflow behind the product's back.

## Fixture lifecycle and isolation

| Scope | Owns | Isolation rule |
| --- | --- | --- |
| Run/session | Reviewed artifact manifest, k3d cluster/network, fresh PostgreSQL fixture, realm, CA and optional registry | Unique run ID; explicit private kubeconfig and credential directory; no external deployment credentials |
| Scenario/function | Installation/release/database or equivalent isolated assignment namespace set, users, clients, browser contexts and canary secrets | No dependence on another test's mutations or test ordering |
| Destructive scenario | Cluster-wide outages, leader failure, rebind, restore, uninstall, node loss and durability-setting changes | Dedicated cluster/database; cannot run concurrently with tests using that dependency |

Register teardown immediately after each successful allocation, then compose
fixtures. A later setup failure must still clean earlier resources. pytest's
[fixture guidance](https://docs.pytest.org/en/stable/how-to/fixtures.html#safe-fixture-structure)
supports pairing individual state changes with their cleanup. Verify run
ownership and exact recorded resource IDs before cleanup, reuse, reset or
injection. No deletion of all volumes, all similarly named databases or all
namespaces by a broad prefix. No global Docker pruning.

Capture diagnostics before teardown and continue all finalizers if one fails.
Report cleanup failures separately without replacing the original test failure;
leaked resources fail the job. A local keep-on-failure option preserves only
verified run-owned resources and a protected inspection manifest. Successful
runs clean everything. Reused clusters are for local diagnosis and do not count
as a clean CI acceptance run.

Parallelism is allowed only after independence is demonstrated. Shared
read-only artifacts and an immutable control plane may be reused; mutable
realm/database/installation state is isolated. Do not enable parallel execution
of a legacy sequential scenario by adding a marker. Cluster-wide fault tests
use a separate worker/profile. No resource creation during test collection.

## Pytest structure and selection contract

The following is the required organization, **not existing paths or working
commands**:

```text
tests/
  unit/                    Python runtime/plugin/release/privacy units
  integration/             runtime images, HTTP, chart and dependency boundaries
  e2e/                     independently collected system scenarios
    test_enrollment.py
    test_owner_isolation.py
    test_native_workflows.py
    test_execution.py
    test_failures.py
    test_storage.py
    test_restore.py
  support/                 cluster, clients, identity, waits and safe diagnostics
  conftest.py               fixture wiring and profile selection
```

Keep Python assertions in pytest test functions; shared support owns mechanics,
not a second hidden test runner. One assertion helper is fine; one pytest test
that invokes the entire old script does not provide scenario-level collection,
isolation or reporting. Keep intentional multi-step lifecycle journeys together
when those steps are the behaviour being tested.

Every test has exactly one level (`unit`, `integration`, `e2e`), requirement/case
metadata and applicable capabilities (`cluster`, `database`, `native`, `browser`,
`security`, `destructive`, `multi_node`, `slow`, `soak`). Register markers and
reject unknown markers and configuration mistakes.
[pytest marker reference](https://docs.pytest.org/en/stable/example/markers.html).
Parameter IDs describe behaviour, never credentials or personal labels.

Required invocation semantics:

- Collection needs no Docker service and lists individual scenario/parameter IDs.
- Selecting a single scenario provisions its prerequisites without first running
  enrollment or settings tests.
- An explicitly requested cluster/database/browser profile fails setup when its
  dependencies are absent. It never succeeds by skipping the entire profile.
- Lightweight Python checks remain usable without a cluster; CI explicitly
  selects each required profile and validates its expected collected cases.
- Rust SQLx/ignored tests and existing browser integration tests remain explicit
  required jobs until their replacements have equivalent assertions and reports.

Declare direct Python imports as locked test dependencies; do not rely on
`requests`, WebSocket or crypto libraries being transitive dependencies of a
Kubernetes client. Prefer synchronous Python clients and fixtures unless the
case needs concurrency. Use explicit concurrent clients/barriers for races,
not accidental timing from sleeps.

## Timing, reliability and diagnosis

Separate provisioning budgets from product guarantees. A slow image import may
have a long setup deadline; it cannot justify widening the 5 s authorization,
10 s connection-close, 130 s provider-revocation or 5 min drain guarantees.

Use controllable time for native state-machine/cache/lease-boundary tests, and
real unaccelerated defaults for selected k3d timing cases. Start measurement at
the defined event: committed access change, injected dependency failure or
provider acknowledgement. Prime a valid session immediately before that event
and exercise the worst cache phase. Record request start, decision/response and
connection-close timestamps separately. An observation request needs its own
short deadline and cannot extend the product bound indefinitely.

For boundaries test just below, at and above the threshold. For real-time tests,
schedule probes early enough that sampling uncertainty does not hide a breach;
record that uncertainty and fail an inconclusive measurement. A later recovery
poll may have slack; a security deadline may not silently gain it.

Every external operation, polling loop, worker process and entire scenario has
an explicit deadline. Poll observable conditions and show bounded progress
events with scenario IDs and phase names. Retry only specified transient
conditions. Mutation retries retain the same idempotency key and body. Do not
retry assertions or whole scenarios to turn a failed security check green.

Each run records source revision, artifact digests, topology/tool configuration,
selected cases, parameters/seeds, start/end times and pass/fail/skip results.
Produce per-case JUnit and readable progress. Required case omissions,
unexpected skips and expired quarantines fail the gate. Any temporary
quarantine has an owner, reason, issue and expiry; it is not passing coverage.

Keep bounded, allowlisted diagnostics: lifecycle/condition codes, resource
UIDs and generations, selected events and sanitized process output. Do not
upload kubeconfigs, Secrets, raw Pod environments, database dumps, homes,
browser storage, tokens or unreviewed network traces. Screenshots and traces
can contain secret form inputs and cookies; default to disabled until a safe
capture/redaction policy is tested. Scan report files and diagnostics for
generated canary secrets, not only application logs.

## CI and completion criteria

| Lane | Required gate |
| --- | --- |
| Every PR and main commit | Native units; shared store contracts against memory and PostgreSQL; HTTP/TLS/binary integrations; chart/privacy/build invariants; runtime image checks; k3d adapter/admission/network checks; all core acceptance cases including restore and a real browser journey |
| Scheduled extended run | Multi-node/NFS, supported Kubernetes compatibility profiles, key rotations, repeated faults, generated schedules and bounded resource/load/soak cases |
| Release | Successful required results for the exact source revision and exact image/chart digests being promoted; the supported extended profiles for that candidate, plus packaged artifact smoke and privacy checks |

The core E2E suite remains a PR gate; the existing workflow already runs the
native system scenario on PRs. Do not move essential security or acceptance
tests to an optional nightly job during the refactor. A release tag alone is
not evidence that the required source/artifact checks ran. No production model
key, cloud account or deployment repository is required by these lanes.

Measure native branch coverage and uncovered boundary modules before choosing
numeric ratchets. Prioritize untested decisions, not trivial lines or total
test count. Completion means that required obligations have collected tests,
assertions at the proving level, passing evidence for the candidate, safe
fixture lifecycle and no unaccounted high-priority gaps. It does not mean that
every hypothetical interleaving or external deployment configuration is proven.
