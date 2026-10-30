# Test gaps and required changes

This is the implementation specification for adapting the tests to the
[testing strategy](../TESTING.md). The evidence and requirement/acceptance
matrices are in the [coverage assessment](coverage.md). The initial
findings refer to source commit `5a05a59`; current implementation notes identify
the added evidence. An uncovered case is not, by itself, a confirmed runtime
vulnerability. An item remains open until its full completion criteria hold.

Priorities describe test work: **P0** closes a critical proof or harness-safety
gap; **P1** extends edge/failure coverage; **P2** establishes longer-running
capacity/stability evidence. Track each item through assertions implemented,
collected in its required profile, executed, and reported for a specific
candidate. File/function existence is not completion.

## Working a gap

1. Read the gap, the [strategy](../TESTING.md) rules it touches and the cited
   SPEC.md requirements. Write the obligations it covers (behaviour, actors,
   variants, expected effects) before the tests.
2. Test against the specification. When a test would pin behaviour that
   SPEC.md, ARCH.md, a contract or a guide does not state, decide first whether
   the document is ambiguous (several readings; say which is meant) or
   incomplete (silent; state the behaviour or the rule), and fix the document in
   the same change. Pin an implementation detail that no document should
   promise (an object's name, say) only by what a document does state. A
   behaviour that looks wrong goes to the maintainer, not into a test.
   Add or change tests at the lowest proving level. Every Python test carries
   exactly one level marker plus its capability markers; parameter IDs describe
   behaviour.
3. In the same change, update [`cases.json`](../../tests/verification/README.md)
   for every added, renamed or removed identity, and name obligation-proving
   tests in the matching [`TESTS.md`](../../services/hermes-manager/TESTS.md) row.
4. Run the affected verifier profiles and the repository checks in `AGENTS.md`.
   A product defect found by a new test is fixed in the product with its
   regression test; never weaken a SPEC.md bound or skip a case to pass.
5. Rewrite the gap's "Current" paragraph to what now exists. When its "Done"
   criteria hold, delete the entry and its references, and correct the
   [coverage assessment](coverage.md).

Product code changes are allowed only to make a behaviour testable (a fault
point, a clock seam, a probe) or to fix a defect a test exposed. They follow the
ARCH.md rules like any other change.

## G01 — Independently collected pytest scenarios

**P0 · Refactor system tests · Prerequisite for reliable per-case reporting.**

Current: `tests/personal_hermes/native_e2e.py` is still the sequential program, and
CI still runs it. Its bring-up and its scenarios are now also reachable as
independent pytest cases (`tests/e2e/`): `tests/support/platform.py` installs
Keycloak, the fake model, PostgreSQL and the chart on the run-owned k3d cluster
and commissions Bob and Carol through the factory `commission()` extracted from
the program, and each scenario helper is its own collected case that keeps the
helper's assertions. Twelve run against one shared platform and can run alone or
back to front (`HERMES_TEST_ORDER=reverse`). The `destructive` marker names the
twelve cases that break or take over their platform: outages, controller absence and
failover, a wrong sealing key, key rotations, suspension, a backend change, a
read-only volume and the restore onto a second cluster. They share one platform of their
own in turn (`tests/support/pool.py`): after each case the pool checks that the
database, the provider, the workloads and the commissioned owners are back, and it
replaces the platform when they are not or when the case failed. Run that way the
twelve take about 25 minutes, against 57 with a platform each. A reverse-order run
found a real order dependency (a gateway login within the 5 s key-refetch cooldown of
the gateway's last fetch is refused once; the case now retries, and the specification
states the bound); the standby and wrong-key cases were also made to leave the platform
as they found it. The shared platform runs on a single-node cluster of its own: with
three nodes, node-local dynamic storage and a static local-path source pinned
to the server leave a worker that mounts both unschedulable, which the first
full run showed as a persistent `409` (the specified single-server setup;
multi-node storage placement belongs to G16). The private-registry scenario is not collected: it edits
one node's hosts file, so it needs per-node registry configuration first.

The installed-image checks of `native_runtime.py` are collected as 13
`python-native` cases (`tests/integration/test_native_runtime.py`, mechanics in
`tests/support/native.py`); the script stays until the scenarios below have
collected equivalents; one end-to-end helper remains uncollected.

Extract clients and mechanics into shared Python support; express each of the
23 acceptance rows as one or more independent collected cases. Factories create
the required published versions, identities and assignments through public
commands. Give every parameter a stable behavioural ID. Keep deliberate
multi-step journeys such as suspend/re-enable inside one case. Convert native
Docker checks and release runtime smoke assertions into reusable pytest
integration cases without removing the existing entry points before parity.

**Done:** any case can run alone and in a different order; a failed case does
not prevent unrelated cases; JUnit and progress name each acceptance variant;
all existing assertions have an explicit retained/replaced disposition.

## G02 — One k3d fixture for every cluster

**P0 · Rewrite cluster lifecycle support · R13–R19.**

Current: `tests/support/cluster.py` creates one run-owned k3d cluster (one server
and two agents) from an explicit configuration: the pinned k3s node image, a
loopback-only API port, k3d's helper image pinned by digest, run labels on every
node, a private kubeconfig and no change to the default context. Its teardown is
registered the moment the cluster exists and removes exactly the labelled nodes.
It imports the pinned probe and platform images into every node by digest, waits
for CoreDNS, and proves NetworkPolicy enforcement with a functional allow/deny
probe before a case runs. The `python-cluster` and `python-destructive` profiles
require `HERMES_TEST_K3D` (k3d v5.9.0). The Rust adapter fixtures, the standalone
runner and the restore's second cluster still use direct k3s containers, per-node
registry mirror and authentication are not configured, and the Python DNS
override for the host name routing is still process-global.

Specify and implement one run-owned k3d fixture with explicit node image/tool
pins, topology, CA/host routing, registry mirror/auth configuration, all-node
image import, private kubeconfig, readiness probes and cleanup. Use it for Rust
adapter fixtures, pytest system cases and both sides of a restore. Preserve
restricted-role tests when changing the infrastructure driver. Add one-server
and multi-node profiles; test the setup's admission and NetworkPolicy enforcement
before counting security results. Do not simply append `--k3d` to the old CI
command and assume fixture parity.

**Done:** clean core and restore profiles run through the same fixture without
editing a default kubecontext or relying on Docker node IPs being directly
reachable from the driver/browser. Multi-node is a topology parameter of the
same fixture; its cases belong to G07 and G16.

## G03 — Trace executed obligations, not function names

**P0 · Extend traceability and reporting · All requirements/contracts.**

Current: the [execution ledger](../../tests/verification/README.md) records exact
collected Python and compiled Rust IDs and reviewed levels. CI gates collection
and passing execution separately for the workspace, fault-enabled process,
production browser, Kubernetes adapter and controller-process profiles. Both
53-case store instantiations are included. Skips, missing cases, additions,
level changes and absent execution results fail. Reports contain sanitized
JSON/JUnit outcomes, source/profile/catalogue metadata and unresolved helper
names; standalone native helpers explicitly remain uncollected G01 gaps.

Architecture checks still scan source names for static consistency. Interface
rows now use contract filename, operation ID and method/path, so all 58
operations and both `POST /auth/logout` rows remain distinct. All nine
operational bounds have required `partial` traceability rows. Full clause,
actor/status/header/side-effect/timing mapping and TypeScript collection/result
inventories are not implemented; passing existing examples does not close them.

Keep the useful static consistency checks and add a case ledger linking
individual specification clauses, all 23 acceptance rows, nine operational
bounds and composite contract operation identities to exact Cargo/pytest/UI
test identities. Include parameter and CI profile requirements. Compare it to
collection and execution reports, including Rust tests generated by the shared
53-case macro and ignored/feature-gated cases. Record missing variants separately
from existing passing examples. Mark body/schema evidence separately from
header, authorization, side-effect and timing evidence.

**Done:** deleting a case from collection, removing it from the macro, omitting
the fault feature or skipping a required profile makes the coverage gate fail;
both logout contracts remain independently accounted for.

## G04 — Safe isolation, teardown and useful failures

**P0 · Refactor harness and test its lifecycle · Infrastructure safety.**

Current: `tests/support/ownership.py` proves a container is a fixture by its
`agent-platform.io/fixture` label (and, for run-scoped steps, its
`agent-platform.io/run` label) before the standalone driver resets it, injects
into it or removes it; a failed inspection is an error and never permission.
A database belongs to a run by the comment the run stored, so `reset()` drops
only those and never a name pattern. Finalizers are registered immediately
after each allocation (k3s containers, the registry pair, the restore cluster
and its database), all of them run, and cleanup failures are attached to the
original error instead of replacing it. `--keep` retains the cluster and the
database after a failure only. Unit tests with a fake daemon and integration
tests with real, never-started containers cover foreign, similar-named,
other-run and other-kind resources and the finalizer stack. The shared
PostgreSQL container is still used by concurrent runs (a pause can affect
another run), the k3d path's node containers carry no run label, the driver as
a whole has no failure-injection tests for each allocation point, timeout or
interrupt, and per-case sanitized phase output waits for G01.

Allocate exact run-owned resources and register finalizers immediately.
Implement ownership checks before reset/injection/removal; cleanup only
recorded IDs. Test failure after each allocation, timeout, interrupt, teardown
failure, partial creation and a foreign resource with a similar name. Capture
allowlisted diagnostics before deletion; preserve the original error and
report cleanup errors too. Restore temporary DNS/environment changes and reap
child processes. Local retention must have consistent success/failure semantics
and protected inspection inputs. Serialize shared outages or use separate clusters.

**Done:** harness failure tests leave no unexpected resource, never touch a
foreign fixture, and produce per-case sanitized phase/result output. Parallel
workers cannot reset or pause each other's database/cluster.

## G05 — Reproducible test inputs and direct dependencies

**P1 · Test setup and artifact receipts · All cluster/native cases.**

Current: `pyproject.toml` declares every direct host-side import (`requests`,
`urllib3`, `websocket-client`, `cryptography`, `jsonschema`) at its locked
version, and `tests/unit/test_dependencies.py` fails when a test or tooling
module imports an undeclared third-party package. Local mutable image tags are
imported into fixtures; there is no shared candidate/test configuration receipt,
and browser tooling is not yet declared.

Declare browser tooling in the locked test environment; pin cluster and
dependency tools/images in reviewed configuration.
Build once from finite contexts, record source plus all resulting digests, and
import those exact artifacts on every node. Distinguish source-unit tests,
fault-enabled integration builds and release-candidate acceptance. Test wrong,
missing and stale artifacts and an environment without accidental globally
installed packages. Do not download/install dependencies during a scenario.

**Done:** a clean machine with documented prerequisites can collect and run the
same selected cases; the result identifies exactly what was tested.

## G06 — Prove the specified deadlines

**P0 · Replace permissive timing oracles · R5, R9, R12, R17.**

Current: the proxy tests use shortened timings, which is useful for mechanism
tests, and `default_timing_meets_the_specified_bounds` only asserts constants.
Real-default behaviour tests now exist in the gateway: an open WebSocket closes
within 10 s of a denial and of a dependency failure (the injection lands just
after a passing check, the worst phase), and a denial reaches new requests within
5 s of a warmed cache (sampled every 100 ms, so the allowed overrun is two
samples). The resolver client gives up at its 2 s deadline for a resolver that
never answers or stalls in the body, and refuses an oversized reply at once.
Slowing the recheck interval or the cache fails these tests. In native E2E,
suspension still sleeps 6 s before checking HTTP denial and accepts a WebSocket
close below 12 s, and its outage helper uses 8 s/12 s; the 60/120 s identity
evidence ages, the 130 s provider revocation, the 1 h WebSocket idle and the
5 min forced drain have no real-default measurement.

Retain fast mechanism tests. Add real-default cases with timestamped committed
access change or acknowledged injection, warmed caches and concurrent HTTP,
stream and WebSocket probes. Account separately for request initiation,
authorization evidence age, response duration and connection closure. Cover
resolver 2 s, evidence 60/120 s, provider revocation 130 s, forced drain 5 min,
body/upload byte boundaries and native shutdown budgets. Exercise exact
boundaries with controllable clocks at lower levels. Do not loosen product
bounds to accommodate test scheduling; record measurement uncertainty and make
inconclusive security measurements fail.

**Done:** an implementation exceeding the required bound would fail its case;
configuration assertions and late post-event polls cannot stand in for it.

## G07 — Live network isolation matrix

**P0 · Add pytest/k3d security integration and E2E · R4, R5, R13, R15, R18.**

Current: `tests/e2e/test_network_isolation.py` applies the platform's own
policies on the live cluster: the chart's control-plane NetworkPolicies, and the
renderer's instance policies configured as the chart configures the controller
(peer selectors, execution port and control namespace taken from the chart's
generated configuration, pod labels from its rendered Deployments). It probes
real Pod addresses over TCP from the gateway, controller, manager API, ingress,
two owners' Hermes and worker Pods, an unrelated namespace and lookalikes that
carry a role's labels from another namespace: 11 documented paths and 28 denied
paths, plus DNS, each with all roles on one node and spread over three. Every
denied path is compared with the same path before any policy existed and with a
live listener on the destination, and a separate case shows that widening a
selector or removing a policy changes the outcome. Not yet covered: the approved
external 443 egress, token absence per role at packet level, Service (not Pod)
endpoints, and enforcement under node loss.

Probe real Pod and Service endpoints from gateway, controller, API, owner A's
Hermes/worker, owner B's Hermes/worker and an unrelated namespace. Require the
documented gateway/native, controller/probe and own-Hermes/SSH/execution paths;
deny cross-owner native/SSH, worker-to-manager, unapproved egress and unrelated
namespace access. Cover DNS/approved egress exceptions and namespace-plus-Pod
selector combinations. Prove token absence for every non-controller role.
Run critical cases across nodes too, with a working positive listener control
and a post-policy readiness barrier. Scope test expectations to ARCH.md's
trusted controller/gateway model; do not invent isolation from an all-powerful
cluster administrator.

**Done:** removing a required deny policy or widening an owner selector makes
a packet-level test fail; endpoint unavailability cannot produce a false pass.

## G08 — Complete admission and restricted-role cases

**P0 · Expand integration matrices · R13, R15, R18.**

Current: `hermes-kube/tests/admission.rs` runs on the disposable API server as
the controller's real ServiceAccount. Each rule has an admitted positive control
(a compliant Pod, a namespace with the installation's name and labels, a Service,
a RoleBinding, a local-path and an NFS volume) and negative cases that differ
from it by one field. Each refusal must carry the intended policy's message, or
Pod Security Admission's where the policy leaves a rule to that layer; both
layers answer some cases, API validation or the RBAC escalation check others.
The cases are 36 Pod rows (34 refused, two admitted by design), 13 namespace
refusals, 21 volume refusals, 4 Service and 7 RoleBinding refusals and 12 RBAC
and impersonation refusals, plus Pod updates and the ephemeral-container
subresource. Mutation checks (an unpinned image, a volume that is not retained,
a controller right to delete volumes) each fail exactly one test. The cases found
that a container could override the Pod's UID/GID with another non-root identity;
the Pod policy now has a rule for it (renderer, chart and golden file). Not yet
covered: PersistentVolume updates, the remaining Deployment-level and
subresource verbs, initContainer and ephemeral-container variants beyond those
above, and a negative case for each non-controller role's own credentials.

Start from a successfully admitted valid fixture and mutate one policy-relevant
field. Cover create/update/patch and relevant subresources, init/regular/
ephemeral containers, privilege/escalation/capabilities/seccomp, host namespaces,
token projection, image pins, namespace/ownership-label forgery, approved PV
root/node/server boundaries, claim references and reclaim policy. Test actual
controller credentials and denied roles, including attempts to edit admission,
RBAC or foreign Secrets/resources. Assert the relevant denial/reason and absence
of mutation. Keep positive valid operations so a policy denying everything fails.

**Done:** each security restriction has an independently valid positive control
and targeted negative case; chart-versus-renderer consistency remains additional
evidence, not the sole security oracle.

## G09 — Generated state, parser and mutation coverage

**P1 · Extend native units and dependency integrations · R3, R8–R18.**

Current: `the_planner_is_total_and_never_contradicts_itself` enumerates a broad
finite grid. The shared contract suite has one seeded generated case,
`generated_command_walks_follow_the_model_and_record_only_accepted_commands`,
which runs three seeds of 50 random administration commands (enable, access and
terminal changes, stale versions, replays and key conflicts) against both stores.
A model of what the specification requires is compared after every step: only
accepted commands appear in the audit log and receipts, each instance has
exactly the predicted generations, instances stay unique, replays return the
recorded receipt, and a failure names its seed and step. Every kind of step
must occur or the case fails. There is no property-testing, fuzz or mutation
infrastructure otherwise, and no generated observation or lease sequences.

Keep that grid. Add reproducible generated sequences for access/generation/
observation changes, lease lifecycle and stale/foreign object observations.
Check monotonic final states, no wrong-UID effects, no ready state without the
required evidence and convergence once dependencies stabilize. Generate path,
identity, nested settings and strict-JSON boundaries. Fuzz exposed parsers and
malformed JWT/header/cookie inputs with bounded memory/time. Mutation-test
selected authorization, owner, lease-finality, secret-delivery and deletion
guards using independent expected outcomes, not a copy of the implementation.

**Done:** minimized failing inputs/seeds become deterministic regressions;
coverage reports identify uncovered branches rather than claiming an invented
overall percentage. Rust domain properties remain Rust tests.

## G10 — Durable writes and transaction/lease races

**P1 · Extend existing Rust shared cases/process tests; Python for black-box races · R1–R4, R9, R17, R18.**

Current: shared same-key/enrollment/last-admin/overlap races, SQL guards and API
before/after-commit aborts already exist. Do not replace them with weaker HTTP
smokes. Database pause is not a database crash-and-durable-restart test. Four
shared lease-race cases now run on both stores: revocation racing concurrent
acquisitions (each acquisition succeeds or is refused, and after the revocation
returns nothing is granted or renewed), heartbeat racing release and racing the
expiry sweep (the lease is finally dead whichever wins), and concurrent
acquisitions with one request ID (one lease). Disabling the PostgreSQL access
check fails the revocation case. A fifth case races deleting
a secret against publishing a template version that names it, 150 rounds on both
stores: exactly one wins whole (publish and a refused delete, or delete and a
refused publish with `unknown_secret`), never both; disabling the delete's
reference check fails it. The harness polls both commands on one task, so it does
not force the lock-level interleaving: removing the `FOR UPDATE` on the secret
row still passes, and a deterministic version needs a pausing fault point
inside a transaction.

Add scheduled races for stale `If-Match`, different actor/key/body combinations,
secret reference/rotation/delete, revoke-versus-acquire/heartbeat/parent join
and concurrent lease expiry/release. Use barriers and database observations
to assert exactly which effects commit. Test lost acknowledgements, transaction
timeouts/deadlocks and fresh process retry with no duplicate audit/material/
generation/grant. Crash/restart disposable PostgreSQL with its retained data.
Exercise unsafe durability settings independently and prove startup refuses
them or safely enforces the required session setting. Run gateway sessions
with the documented restricted database role, separately from the schema owner.

**Done:** both stores pass applicable common cases; real transactions/processes
prove database-specific effects. An acknowledged change survives the modeled
database/process failure; do not label SIGKILL as proof against all storage loss.

## G11 — Complete execution and drain lifecycle

**P0 · Extend integration and independent E2E · R9, R12, R14, R16, R17.**

Current: lower-level lease/mode/key cases are strong; native E2E principally
uses `per_session`, changes a backend under an active lease and suspends during
pending acquisition. `tests/e2e/test_access_states.py` walks all six ordered
transitions between `enabled`, `suspended` and `revoked` on one instance: the
owner is denied within the cache bound and never let through while not enabled,
an open WebSocket closes within 12 s, nothing runs, and enabling again restarts
the same instance on the same volumes, marker data and credential digests. This
is not the entire revoke matrix. `tests/e2e/test_terminal_modes.py` runs the
three lifetime modes on real workers (a 60 s idle backend): `per_session` keys an
environment by session and retires it when the session ends; the two profile
modes share one environment across sessions; on demand retires it after the
idle timeout and always running keeps it and still serves; an acquire naming
another mode is rejected and creates nothing. `tests/e2e/test_worker_loss.py` stops a worker's
container mid-command: the call fails, the command is never replayed or
finished, no other worker replaces the lost one, its environment retires and its
Pod goes, the shared volume keeps its data, Hermes is not restarted, and the next
session gets a fresh worker of the same backend on the same data.
`tests/e2e/test_environment_limits.py` limits a per-session backend to two
environments: a third session is refused `503` at once with no record and no
worker Pod, the two live sessions are untouched, and the room returns when one
retires. `tests/e2e/test_lease_lifecycle.py` drives the plugin's own client inside a
running Hermes: a delegate joins its parent's environment with a second lease while a
lease of another session, a stranger's and a released parent's are refused `403` and
create nothing, and releasing the parent leaves the child live; one instance serves two
backends (the default, an explicit pick, a shared profile environment per backend, a
runtime outside the policy refused), and changing the default sends new requests to an
environment of the new generation while live leases keep theirs; and a lease is granted
for and renewed up to `maxLeaseSeconds` and no longer. The last case found that a lease
was granted for the standard 120 s whatever the backend's limit, so a backend limited to
60..119 s was exceeded; the grant is now capped (shared store case
`a_lease_never_outlives_its_backends_lease_limit`, which fails without the cap). The
specification says that a backend change reaches a running session when its lease ends,
at most `maxLeaseSeconds` (pinned by the existing backend-change scenario and this cap),
and that an access change drains at once. Still open: fairness among instances, a
forced drain of a non-cooperating active worker at the five-minute cutoff, and the
revoke matrix beyond suspension.

Run real `per_session`, `per_profile_on_demand` and
`per_profile_always_running` cases with multiple profiles/sessions and both
default/explicit selections. Add actual revoke/re-enable separately from
suspend, acquisition/heartbeat races at revocation and expiry, parent joins,
limits/fairness, active backend removal and volume/read-only binding changes.
Kill a worker mid-command and verify no command replay, no unauthorized
replacement and retained data. Force a non-cooperating active worker to the
drain cutoff. Verify pinned SSH identity, retired-lease finality, default choice
on new requests and independent Hermes liveness throughout.

**Done:** every mode and transition has native/shared evidence plus selected
real workers; no late grants, revived leases, local fallback or private-home
mount is possible without a failing assertion.

## G12 — Installed owner policy and private-home lifecycle

**P0 · Extend image integration and k3d cases · R4, R6, R8, R10, R18.**

Current: every instance requires the mounted owner and terminal policies; there
is no non-personal mode and no environment marker that selects one. Unit and
in-image checks refuse missing policies, environment pins and shadow plugins;
`runtime-init.py` refuses to start without the managed policy. Initializer
tests cover interrupted/concurrent writes, symlinks and tampered receipts, and
`tests/integration/test_personal_init_inputs.py` drives the initializer entry
point with 68 malformed mounted inputs (context, configuration, key, input
file kind and home root): each is refused with one fixed message that echoes
nothing and leaves the home untouched, and the same inputs with one field
corrected initialize. `tests/integration/test_native_runtime.py` (profile
`python-native`) starts the built image as a Pod does and collects: refusal of
unowned authentication and execution without the mounted policies, managed
precedence and the unredirectable policy directory, the terminal lock for every
scope and backend, shadow-plugin refusal, profile overrides, native dashboard
authentication, the receipt through the kubelet result file, and retention of
owner state across an update and an abrupt kill. Init-Pod death at each receipt
boundary in the installed image, malformed owner/instance policy inputs to the
running image and layer-precedence for removed overrides remain.

Exercise actual installed init/run entry points with missing, malformed,
oversized, foreign-owner and wrong-instance/material policy/receipt inputs,
with markers absent or forged. Assert blocked/denied behaviour, no secret in
errors and no overwrite of the existing home. Include actual init-Pod death
at supported receipt boundaries and runtime image/profile changes. Validate
layer precedence and override removal while preserving user settings, multiple
profiles, memory, sessions and file hashes. Assert mounted policy immutability
and no terminal plugin/local-backend substitution. Do not infer this from helper
exceptions alone.

**Done:** the Helm-commissioned image fails closed for bad inputs and preserves
its exact retained owner state through the supported lifecycle.

## G13 — Real browser and native workflow coverage

**P1 · Extend the pytest/Playwright journeys · R4, R6, R7.**

Current: `tests/e2e/test_browser.py` drives Chromium (Python Playwright,
`tests/support/browser.py`) on the installed chart through Keycloak's real login
form. The browser trusts the fixture CA through its own certificate database
(hostname and chain validation stay on), resolves only the three fixture
hostnames and runs from a private home; every case takes a context of its own.
The 15 collected cases cover: session cookies (Secure, HttpOnly, `SameSite=Lax`,
host-only, unreadable by page script, no code or state left in the URL);
an unassigned account's inert denial page; two owners reaching only their own
instance and files; foreign pages on another site and on a sibling host
(`attacker.example`, `attacker.e2e.test`) unable to use the owner's or the
administrator's cookies for what the specification protects; logout and the
replay of the ended session's whole cookie jar (a control proves the jar works
first); hostile text in a callback and in stored catalog content shown as text,
with no dialog, script error or CSP violation; the dashboard's strict policy and
host-only cookies; a person without a grant refused; stale-revision refusal in
two administrators' dialogs; an administrator revoked while a page is open; and
a dashboard action that commissions an instance whose new owner then uses it in
a browser while another owner cannot see its files. The mocked presentation
cases remain integration tests with their limited claim. The browser starts with
Local Network Access checks off: the fixture's node address is private, and the
browser would otherwise refuse every request a foreign page makes to it before
the platform saw it.

Not covered: the native UI as a rendered application (chat, history, session and
settings pages, a WebSocket from a page, hostile file names and model text in
its views), dependency failures and the owner-only pending and error pages in a
browser, an owner's revocation during navigation, template rollout in the
dashboard and its remaining validation errors, the dashboard's 60 s permission
re-check, and an email change in a browser.

Decided and implemented (see the specification): a logout refused for its
Origin or CSRF token expires no cookie, on the gateway and the dashboard; the
gateway refuses requests that can change state, and WebSocket upgrades, whose
Origin is not the public origin, so a page of a sibling host cannot act with the
owner's cookies; logout stays platform-only and does not end the identity
provider's session, which the specification now says. The browser cases pin all
three, including a forged upload, logout and form post from a sibling host and
from another site.

**Done:** the not-covered journeys above are collected in the cluster profile and
passing, each with a control that shows its oracle can fail, and nothing else
is pending.

## G14 — Secret delivery, rotation and safe diagnostics

**P0 · Add integration/E2E matrix and harness redaction cases · R10, R11, R18.**

Current: write-only API/audit, sealed store versions, env-model delivery,
registry authentication and application-log canary checks exist.
`tests/e2e/test_secrets.py` rotates the model key on a platform of its own, twice:
only the referencing instance restarts, it receives the current value and no
other instance does, a referenced secret cannot be deleted, and neither value
appears in the secret list or the audit log. It also starts the controller with
a different valid sealing key: it refuses to start (`sealing_key_mismatch`), people
added and secrets rotated meanwhile are handled once the right key returns,
credentials are unchanged, and no instance ends up with material the right key
cannot open. The startup check is also a shared controller case on both stores and a process
case (the compiled controller exits with `sealing_key_mismatch` before it reads
or writes anything; removing the startup call fails it).
`tests/e2e/test_secret_delivery.py` runs three cases on the shared platform with
canary values compared only by hash: a file secret is a read-only `0440` file in
the referencing instance alone (a scan of every Kubernetes Secret finds it
nowhere else), follows rotation (the instance restarts, the old value is
gone from the cluster) and leaves with its reference; a registry secret becomes
a Docker-config pull Secret in the namespace of the instance whose backend names
it and nowhere else, and follows rotation; and references of the wrong kind or to
nothing, bad registry values, a wrong-kind rewrite, a dangling draft, and
deletion without or with a stale version, or while referenced, are refused with
their codes and store nothing. Each case scans the API bodies, the audit log and
the logs of the control plane and the instances for its canaries, after a
control that shows the scan reads real output. A secret an owner's agent can
read inside its own instance is delivered on purpose and is not reported.

Not covered: assignment-level secret references, a Pod-level check of workers for
a file secret, a missing sealing key, canaries for JUnit, driver exceptions,
screenshots and traces, and a review of candidate images for payloads (the
privacy preflight covers the reviewed inputs).

Test env/file/registry secrets of each permitted reference scope, wrong kinds,
missing references, selective rotation/restart, removal and deletion refusal
while referenced. Assert non-referencing instances/workers do not receive the
value; compare allowed recipients' content without exposing it in reports.
Missing/wrong sealing keys must refuse operation rather than regenerate material.
Add canaries for errors, malformed inputs, startup failures, HTTP/audit responses,
Pod logs, JUnit, driver exceptions, screenshots and any enabled traces/artifacts.
Review the candidate images/chart/context/receipt for forbidden payloads.

**Done:** correct recipients receive the current value; unrelated instances
remain unchanged; no unintended output/artifact contains a canary. Reading an
intentionally delivered secret inside its owner's agent is allowed by SPEC.md
and must not be mislabeled as a leakage failure.

## G15 — Real identity and trust rotation failures

**P1 · Extend existing identity integrations and selected real-Keycloak E2E · R1, R5, R6, R9, R18.**

Current: signed tokens, bad claims and keys, callbacks and resolver mTLS are tested
against fake providers, and the system run covers a provider outage and a
disabled person. `tests/e2e/test_identity.py` rotates the realm's signing key at
the real Keycloak on a platform of its own (with the access-token lifespan
raised so key and expiry are not confused): a token of the new key is accepted
by the manager API without a restart after the verifier refetches on the
unknown key ID, a gateway login still works, the old token keeps working while
the old key is published, and after the provider stops publishing it the old
token is rejected once the five-minute cache window has passed while the new
one is unaffected. The same file rotates the gateway's session key across restarts (platform
of its own): a restart with the same ring keeps a signed-in session; a ring with
a new active key beside the old keeps old sessions readable and seals new ones
under the new key; dropping the old key ends exactly the sessions sealed under it,
and people sign in again. A further case on a platform of its own shortens the realm's SSO idle limit to
90 s (with the access-token lifespan raised so that expiry is not confused with
it): a session left alone for 150 s under the default limit is refreshed on demand
and served, an active session survives the shortened limit because the gateway
keeps refreshing it, and a session whose provider login lapsed is refused and
ended (`401` on the platform and the native route). That case failed once in six runs
(in a reverse-order run of the destructive profile, about 100 s into its 180 s loop) and
passed three times in a row alone and in its position in the other order; five polling
cadences from 10 to 40 s against the 90 s limit all kept a session alive, so the
margin is not the cause and the cause is unknown. When it recurs, run the case with
`pytest --tb=short` to read its assertion: the verifier hides failure text on purpose.
Concurrent refresh, provider partition, certificate negatives on a cluster and two gateway
replicas remain.

Preserve current signed-token, bad-claim/key, refresh/callback and resolver mTLS
tests. Add real Keycloak signing-key rotation, stale JWKS, refresh-token expiry,
session/keyring rotation across restart, concurrent refresh and provider
partition/recovery. Probe missing/expired/wrong-SAN/wrong-CA client/server
certificates and issuer/audience/client confusion with no outbound discovery
or token-driven arbitrary fetch. Test warm cache and two gateway replicas.
Use exact mounted owner identity independently of a forged gateway request;
email renaming must remain irrelevant.

**Done:** valid rotated credentials recover while expired/foreign evidence never
routes; negative TLS cases verify the intended failure rather than relying on
a client configured to ignore TLS errors.

## G16 — Real storage, read-only and retention behaviour

**P1 · Add k3d dependency/E2E profiles · R8, R9, R13, R14, R16.**

Current: source validation/rendering, static overlap races, real local workspace
sharing and missing-directory recovery exist. There is no live NFS fixture or
multi-node mounted-data/read-only matrix.

A first multi-node attempt showed the interaction G16 must test: with a
node-local dynamic class (`local-path`) and a static `localPath` source pinned to
another node, a worker that mounts both volumes is unschedulable and the
manager answers `409` for the whole acquisition deadline. The specification
accepts this ("the expected setup for a single-server deployment"), but the
condition surfaces only as an unexplained pending acquisition; the e2e platform
therefore runs on a single node and the multi-node behaviour is untested.

`tests/e2e/test_storage.py` shows on a platform of its own that a volume the
assignment marks read-only refuses writes, truncation and deletion inside the
Hermes Pod (`Read-only file system`) while a writable sibling of the same kind
and ownership accepts them, and that the host directory is unchanged.

Mount a real NFS export through the approved source; test UID/GID/permissions,
read-only restrictions, shared-path equality and absence of the private home in
every worker. Cover unreadable/missing static paths, NFS outage/recovery,
Pending/Lost/replaced claims, expansion and shrink/class-move refusal. Test
node affinity and worker/Hermes placement on multiple nodes. Preserve host
ownership/content and volume UIDs/hashes through suspend, revoke, worker loss,
runtime update and Helm uninstall; the controller must not create/chown/delete
an assigned host directory as recovery. Exercise storage changes under a live
lease together with G11.

**Done:** supported source behaviour is demonstrated with actual files and
mounts, not just API objects or the name “network home”.

## G17 — Restore complete retained state and reject bad backups

**P0 · Expand independent restore E2E · R4, R8, R17, R19.**

Current: the new-cluster test archives `/srv/hermes-data`, restores PostgreSQL,
reuses the original sealing key and checks a static workspace marker plus login.
It does not archive/assert the dynamic private-home contents or explicitly
compare before/after credential digests. The older-backup
orphan test is a real same-cluster safety case, but not a complete successful
same-cluster known-instance data recovery case.
The process case `a_restored_registry_is_rebound_only_to_a_cluster_without_its_namespaces`
(compiled controller and `rebind-cluster` against PostgreSQL and a disposable
Kubernetes API) covers the refusals: a registry bound to another cluster stops the
controller (`cluster_changed`); `rebind-cluster` refuses while the cluster holds
one of the installation's namespaces (`namespaces_exist`, nothing cleared, binding
kept); with the namespace gone it records the new identity, clears the binding,
and the controller recreates the namespace on the material it already had.
Removing the namespace check fails it. A wrong sealing key at restore is
refused by the controller's startup check (G14).
`tests/e2e/test_home_receipt.py` damages the home's initialization record on the
node and restarts the instance: a receipt with a flipped signature, and a
receipt and intent both missing on a home that holds data, keep Hermes from
starting (the initializer fails; the home's data and configuration are
byte-identical afterwards) until the genuine files are back; a missing receipt
with its intent intact is finished from the intent and rewritten identically. The
instance is reported blocked with the typed reason `invalid_initialization_receipt`
(`nativeInit` `home_unproven`) while its initializer refuses the home, from the
code the initializer leaves as its termination message; the manager's unit,
shared-store and adapter cases cover the classification, and the initializer's
integration cases cover the code it leaves. Partial storage remains.

Create a restore fixture that explicitly inventories and backs up every tested
storage source and protected key input, with writers stopped for a consistent
test backup. Restore private home settings/profiles/memory/native sessions and
static data, assert instance/material/credential identities and selected data
hashes, then verify new writes and terminal execution. Add successful same-
cluster recovery; new-cluster refusal before rebind; refusal with foreign or
non-empty destination state; wrong/missing sealing keys, partial storage and
tampered/missing receipts; continued reporting/non-adoption of orphans.

The specification intentionally limits generic dynamic-provisioner guarantees.
Choose and document a deterministic test provisioner configuration for that
profile, and retain separate deployment restore drills. Do not claim universal
CSI/NFS backup recovery or introduce platform checkpoint orchestration here.

**Done:** restoration cannot pass after silently replacing an owner's home with
a fresh one; bad backups fail without adoption or credential regeneration.

## G18 — Leader failover, observation faults and bounded work

**P1 · Extend Rust adapters/process integration and pytest/k3d · R3, R5, R16, R17.**

Current: lease-holder API tests, stale-epoch store cases, controller aborts and
queue backoff units exist. They do not cover every two-process/worker boundary
or a prolonged failing Kubernetes API.

`tests/e2e/test_leadership.py` adds two cluster cases on platforms of their own: with
two controller candidates the leader is force-killed, a standby takes the Lease
and commissions a new instance, and no existing instance's Hermes Pod,
namespace, credentials or worker keys change (one namespace per instance); with
no controller at all, Hermes keeps running untouched for a minute and the
controller's return disturbs nothing. Process pause and API partition, a
deposed controller writing after recreation, injected 409/429/5xx and slow
applies, lost notifications and replicas of the API and gateway remain.

Run competing controller processes through lease expiry, process pause/kill and
API partition. Prove a deposed controller cannot write status or delete a newly
recreated object, including worker Pods. Inject stale observations, 409/429/5xx,
slow applies/deletes, watch disconnects and lost notifications; periodic resync
must converge. Add API/gateway replicas with session/grant changes and warm
caches. Assert queue deduplication, bounded requests/backoff and no busy loop or
healthy Hermes restart caused solely by control-plane recovery. Keep release
fault hooks test-only and enumerate the executed fault locations in G03.

**Done:** stable dependencies lead to convergence with one authoritative
controller and no unobserved deletion, duplicate generation or unbounded retry.

## G19 — Capacity, long idle and repeated failure evidence

**P2 · Add scheduled bounded pytest profiles · R9, R12, R16–R18.**

Current: short liveness, individual limits, cache bounds and worker cleanup
cases exist. There is no sustained load/soak baseline or one-hour heartbeat case.

Define workload size, duration, CPU/memory/disk budgets and accepted bounds for
active owners, sessions, workers, database connections and retained resources
before writing pass thresholds. Exercise many idle/active owners, acquisition
limits, repeated login/refresh/reconnect, stalled clients/slow uploads, one-hour
WebSocket heartbeats, cron inactivity and repeated dependency failures. Track
memory/connections/tasks/queue/volume growth and recovery after load stops.
Exercise per-owner fairness and security deadlines during the bounded load.

**Done:** published fixture-sized resource/stability evidence and repeatable
limits exist. This establishes tested capacity, not an unsupported production
scale promise. Keep external model/provider benchmarks optional and separate.

## G20 — Candidate artifacts and actual release gates

**P0 · Extend packaging/CI integration tests · R8, R17, R18.**

Current: release unit tests mock build/publish commands, and image checks smoke
specific properties. The tag workflow's first job, `gate`, requires passing
`Checks` and `Cluster scenarios` runs for the exact tagged commit before the
publishing job starts (`agent_platform/release_gate.py`, with the latest run per
workflow deciding, a run for any other commit ignored, and anything not completed
and successful refusing); `tests/unit/test_release_gate.py` covers the decision,
the page format of the API's answer, the workflow names, and that publication
`needs` the gate. It does not yet compare the artifact digests those runs tested
with the ones published, and the cluster scenarios are dispatched by hand. The
tag workflow also
requires the chart and project versions in the tagged commit to equal the tag,
while ordinary commits keep the `0.0.0` placeholder. That rule is now
`agent_platform.release.verify_release_source` and `verify_placeholder_source`,
called by the tag workflow before any login or publication, and
`tests/unit/test_release_versions.py` covers the version grammar including
`-dev.N`, the trigger pattern against it, each of chart and project being stale,
the placeholder never being a release, and this tree being a placeholder tree or
the exact release commit.

Test installing the packaged chart with all three exact built image digests,
then supported upgrade and uninstall retention. Verify labels/licenses,
non-root workers, context completeness, static UI assets and absence of secrets
and fault-injection features in the actual artifacts. Exercise a release
binary with the fault environment set during a real command: it must not abort
at the test hook. Add failure-path tests
that validation errors prevent promotion/chart publication; an unrelated passing
source revision cannot satisfy the candidate gate (done for the revision, not for
the artifact digests). Local
artifact tests need no public push; publication remains separately authorized.

**Done:** release eligibility refers to the tested candidate, not merely a tag
or an unrelated green branch, and packaged behaviour matches source tests.

## G21 — Operation/status/actor/header and byte-boundary matrix

**P1 · Expand native HTTP integration and Python black-box cases · R2, R5–R7, R10–R12, R18.**

Current: real endpoint tests and schema checks exist. `hermes-http/tests/wire_boundaries.rs`
sends raw HTTP/1.1 through `hermes_testkit::raw` and proves, for the
administration API, the exact 16 KiB body bound (declared at limit − 1, the
limit and limit + 1; chunked at the limit and limit + 1), the exact 8 KiB serialized `adminConfig`
bound (template and assignment layers; ASCII, two-byte and four-byte text;
pretty-printed and escaped wire spellings), cancelled bodies, the `Content-Type`,
`Idempotency-Key` and `If-Match` grammar with repeated headers, and 24 malformed
or ambiguous JSON shapes. Each refusal has an accepted control and leaves audit
events, catalog, assignments and the key's receipt unchanged. The execution and
resolver listeners have the exact-bound, chunked and repeated-header cases. These
cases found three defects, now fixed: `If-Match` accepted `"01"` and `"+1"`
against the contract pattern, a repeated `Content-Type` was resolved by parser
order, and `createAssignment` problems were located inside the spec instead of
the submitted document. The gateway's upload limit is proven exactly (declared
length at a lowered limit and at 64 MiB; a streamed body at the limit and one
byte over), which found a streamed over-limit upload answered `502
dependency_unavailable` instead of `413`; that is fixed. `tests/e2e/test_contract_matrix.py` generates a case per secured
administration operation from `openapi.yaml` (43 of 45): with no credential it is
refused `401 invalid_identity_evidence`, with a valid token of a person who holds
no grant `403 access_denied`, both `no-store`/`nosniff`, with no cookie, no echoed
token, and with audit, assignments, secrets, templates, backends and
administrators exactly as before. Logout is the one exception: it checks Origin
and CSRF first (`403` for both) and expires the caller's cookies when it refuses
(the decision recorded under G13). A unit check pins the generated set to the
contract. `tests/e2e/test_gateway_interface.py` is the gateway's ledger for a client
without a session: 29 rows (login with local and refused return paths, callback
without proof, with an unknown state and with a provider error, `me`, both
logouts with and without the Origin, the signed-out and liveness pages, page
loads, other methods and a WebSocket upgrade on the proxied application, an
unknown platform path) each with its exact status, body or redirect, the
no-store/nosniff/no-referrer headers, the cookies it sets and no reflection of
what was sent, plus a case that every login flow is fresh and bound to the
browser by a Secure, HttpOnly, Lax, host-only proof cookie. The execution and
resolver listeners are checked response by response against their contracts in
`execution_api.rs` and `resolver_api.rs`. `tests/e2e/test_gateway_session.py` is the signed-in half, with the cookies
replayed by hand: `me` names the owner and the session's CSRF token and nothing
else; a logout is refused for no or a foreign Origin and for a missing, wrong,
malformed or another session's token, and no refusal ends anything on the server
(the replayed cookies still work, and another person's session is untouched);
duplicated, missing, tampered and cross-browser cookies get `401`; both logouts
end the session on the server. `tests/e2e/test_unknown_resources.py` is generated from the
contract: each of the 21 administration operations that take a path parameter,
with a well-formed identifier that names nothing and with a malformed one, answers
`404 not_found` (private, inert, naming nothing the client sent) and commits
nothing; the two version listings of a catalog answer an empty page for an unknown
name, as the contract declares, and `404` for a malformed one; a control reaches
the same resources when they exist. `tests/e2e/test_dashboard_interface.py` is the dashboard's
ledger on the administration host: 25 rows without a session (login with local
and refused return paths, the callback, both logout outcomes, the signed-out and
liveness pages, page loads and their redirects, the API, assets and unknown
paths), each with its status, body or redirect, headers, cookies and no
reflection; and signed-in cases with the cookies replayed by hand: the session
names the administrator and this browser's CSRF token; a browser mutation is
refused for no or a foreign Origin and for a missing, wrong or another session's
token, changes nothing and sets no cookie, and the exact request is applied; both
logout outcomes; duplicated, missing, tampered and cross-browser cookies; and a
request that presents a bearer token and a session together, refused as
ambiguous. `tests/e2e/test_preconditions.py` is the precondition ledger: each of the ten
operations that declare `412` (a unit check pins the set to the contract) is refused
`412 stale_version` for another version and `422 invalid_request` for none, privately
and with nothing changed, and goes through with the current version; the named
conflicts (`assignment_exists`, `administrator_grant_exists`, `last_administrator` in
all three ways, `secret_in_use`, `version_not_draft` for every wrong-state edit,
publish or withdraw of a template or a backend version) each leave the state
untouched; and an idempotency key repeats its command's result, refuses another body
with `idempotency_conflict` and is a separate change for another command. The cases
found two gaps in the specification, both amended: a missing `If-Match` was `422` on
every operation but declared on five of ten, and the state a version must be in to be
withdrawn was unstated. Still open: session expiry and refresh at the gateway, and
the unavailable variants of the administration operations (covered case by case at the
store and HTTP levels).

For each composite operation identity list allowed actors and expected
unauthenticated/forbidden/stale/conflict/invalid/unavailable variants; validate
declared schemas plus `ETag`/`If-Match`, idempotency, CSRF/origin, caching,
content type, cookies and redirects where applicable. Cover missing/duplicate/
malformed headers, duplicate JSON keys, unsafe numeric/deep/Unicode input,
unknown fields, exact 16 KiB request/8 KiB admin-config and configured/default
upload boundaries, chunked bodies and premature cancellation. Assert error
redaction, no cross-owner disclosure, no committed effects on rejection and
no upstream requests on authentication/routing denial. Use raw HTTP when a
friendly client would normalize away the malformed case.

**Done:** declared interface outcomes and important invalid wire variants have
executed cases and independently asserted headers/effects. Source operation
presence and a few JSON schema samples are insufficient completion criteria.

## G22 — Accurate testing documentation and CI profiles

**P1 · Documentation corrected in this review; enforcement still required.**

Replace claims of implemented property tests, nightly execution or exhaustive
coverage with the actual evidence. The current PR/main workflow already runs
the full native script; no scheduled workflow currently implements the extended
profiles. Keep current executable commands available until the new pytest/k3d
commands really exist. Do not label the old script as pytest or a direct k3s
run as k3d.

Implement the strategy's explicit native/integration/core/extended/release
profiles and required-case reporting, then update command documentation and
`TESTS.md` together. A missing dependency, zero collection or unexpected skip
in a requested profile must fail. Any remaining scope limitation or accepted
gap needs a reason, responsible owner and review point rather than a `pass`.

The Python/Rust baseline profiles and strict level/marker checks are
implemented and described in the [execution ledger](../../tests/verification/README.md).
A plain `uv run pytest -q` and the `python` profile select only tests that need
no infrastructure; the capabilities `native`, `cluster` and `destructive` have
their own `python-native`, `python-cluster` and `python-destructive` profiles,
which fail when their input (`HERMES_TEST_NATIVE_IMAGE` or `HERMES_TEST_K3D`) is
absent. The k3d profiles are dispatched by hand through
`.github/workflows/cluster.yml` and are not a PR gate; the profiles retain the
existing PR/main system job. They do not implement the extended/soak or
exact-artifact release profiles, and the browser, database and multi-node
capabilities have no profile of their own. TypeScript cases are not in the
ledger.

**Done:** documented commands and schedules match CI, and each reported result
identifies the executed profile, candidate, cases and coverage limitations.

## Order of work and preservation rules

1. The baseline case ledger exists (G03, partly). Keep it and the existing
   native/shared/Rust/TypeScript suites and their PR gates running.
2. Build and test ownership-safe fixtures and reproducible inputs (G04, G05),
   then the canonical k3d lifecycle including restore (G02).
3. Split the existing scenarios into pytest with equivalent assertions and
   independent setup (G01). Add live network/admission and exact timing evidence
   first (G06–G08), then execution/policy/secret/restore critical cases
   (G11, G12, G14, G17).
4. Fill the remaining clause/interface/browser/storage/identity/crash gaps
   (G09, G10, G13, G15, G16, G18, G21), preserving the native proving boundary.
5. Require exact-candidate results in publication gates (G20, G22); add extended
   compatibility/NFS/multi-node and defined load/soak profiles (G19).

Retire an old assertion/entry point only when its replacements are collected
and passing with equivalent or stronger oracles. A deliberate narrower claim
must be recorded as a gap, not hidden by moving or renaming the test. These
changes adapt the test system; they do not authorize new application features,
weaker specification deadlines, a new runtime design or deployment-side scripts.
