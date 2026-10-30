# Collected-case verification

[`cases.json`](cases.json) is the reviewed inventory of exact Python and Rust
test identities and their proving levels. It includes every parameter, both
instantiations of the shared store contracts, and separate feature/ignored-test
profiles. It is a baseline inventory of existing examples. Full specification
clause, actor, response, header and timing coverage remains tracked in
[G03](../../doc/testing/gaps.md#g03--trace-executed-obligations-not-function-names).

Run from the repository root with Python 3.14 and `uv sync --locked`:

```bash
uv run python -m tests.support.verification inventory python --output .local/verification/python-collection.json
uv run python -m tests.support.verification inventory rust --output .local/verification/rust-collection.json
uv run python -m tests.support.verification run python --output .local/verification/python.json
HERMES_TEST_NATIVE_IMAGE=personal-hermes-native-test:local uv run python -m tests.support.verification run python-native --output .local/verification/python-native.json
HERMES_TEST_K3D=/path/to/k3d PATH=$PATH:/path/to/helm uv run python -m tests.support.verification run python-cluster --output .local/verification/python-cluster.json
HERMES_TEST_K3D=/path/to/k3d PATH=$PATH:/path/to/helm uv run python -m tests.support.verification run python-destructive --output .local/verification/python-destructive.json
uv run python -m tests.support.verification run rust --output .local/verification/rust.json
uv run python -m tests.support.verification run rust-faults --output .local/verification/rust-faults.json
MANAGER_UI_DIST="$PWD/services/hermes-manager/ui/dist" uv run python -m tests.support.verification run rust-browser --output .local/verification/rust-browser.json
```

Python execution needs Helm on `PATH` for the chart cases. The `python` profile
and a plain `uv run pytest -q` select only tests that need no infrastructure:
a test that needs a cluster, database, native image, browser or exclusive
infrastructure carries that capability marker and belongs to the profile that
provides it. `python-native` needs `HERMES_TEST_NATIVE_IMAGE` to name the image
under test; `python-cluster` needs `HERMES_TEST_K3D` to name a k3d binary at the
pinned version, builds one run-owned cluster of one server and two agents, and
proves NetworkPolicy enforcement before any case runs; its browser journeys also
need Playwright's Chromium for the locked version (`uv run playwright install
chromium`) and the NSS `certutil` tool. `python-destructive` runs the cases that break their platform on one of their own. All fail rather than skip
when their input is absent. Rust inventory needs
the pinned toolchain and compiles test targets, then asks their actual libtest
binaries for their collected names. Collection does not create infrastructure
or need a running database, browser or Docker daemon. Rust execution needs the
disposable PostgreSQL fixture in the
[implementation guide](../../doc/personal-hermes/implementation.md#verification-commands).
The browser profile also needs the production UI build, installed UI dependencies
and Playwright Chromium, as described in the
[dashboard guide](../../doc/personal-hermes/dashboard.md).

The existing `tests.personal_hermes.kubernetes` driver supplies disposable
cluster prerequisites and runs `rust-cluster` and `rust-controller`. Invoking
either profile directly requires its fixture environment; absent prerequisites
fail. These profiles still use the existing direct-k3s driver. They do not
implement the canonical k3d lifecycle or independently collected E2E scenarios.

`inventory` verifies collection only. `run` verifies collection and execution:
all expected cases must be present, have their reviewed level, and pass.
Missing, new, skipped, xfailed, xpassed, setup-failed and teardown-failed cases
fail the gate. Cargo still executes its normal command, including doctests;
its exit code must also succeed. Deliberately ignored cases outside a Cargo
profile are excluded from that profile's baseline and required by their own
profiles. Removing the shared contract macro entry or the fault feature cannot
be satisfied by a helper function with the same name.

Reports record source revision, dirty-tree status, catalogue digest, profile,
start/end times and individual outcomes. Python records phase durations; Rust
case durations are not measured. Execution also writes a sibling JUnit XML.
Only reviewed identities appear verbatim: unreviewed IDs are hashed so an
accidental parameter value does not enter reports. Failure bodies, captured
output, environments and credentials are omitted. CI uploads only these JSON
and XML files, with seven-day retention. A failing case can be diagnosed locally
with its ordinary `uv run pytest` or `cargo test` command; these entry points
remain available.

The obligation inventory resolves the rows in `services/hermes-manager/TESTS.md`
to exact catalogued IDs. Both logout operations use contract filename and
operation ID. Standalone native helpers appear under `uncollected` with their
source and G01 gap in the catalogue; they provide no collected-case result.
The existing native CI job continues to execute those scripts. TypeScript
unit/presentation tests retain their existing gates and are outside this
inventory. Dirty-tree reports are local diagnostic evidence, not release
eligibility for an exact source/artifact candidate.

When adding or intentionally replacing a test, inspect the actual collection,
review its assertions and proving level, and edit the catalogue in the same
change. Use behavioural parameter IDs, never input values or credentials.
`uv run pytest --collect-only -q` shows Python IDs before catalogue review;
Cargo's `--list` output supplies compiled Rust names. Review the catalogue diff
alongside the tests. There is no automatic baseline-update path in CI. Removing
an `uncollected` entry requires a collected replacement and corresponding
updates to traceability and the gap register.
