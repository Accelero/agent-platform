# Testing

pytest runs every test in this repository: Python suites, the wrapped Go unit
tests, image tests and end-to-end stacks. Tests are organized by component and by
one of three levels. One suite is one component at one level, and each suite runs
on its own CI runner, named after that suite.

## Levels

| Level | Boundary | Location |
| --- | --- | --- |
| `unit` | One unit in process. Python unit tests may read and write temporary files but may not start a process or open a network socket; the suite plugin fails such a test. Unit tests of another language run with that language's tool chain, wrapped by a pytest test that reports each native test separately. | `<component>/tests/unit/` |
| `integration` | One component across a real boundary: a TLS listener, a SQLite file, `helm`, `kubectl`, a disposable PostgreSQL container or the component's own container image. | `<component>/tests/integration/` |
| `e2e` | Several components together, from the client to the last adapter or across real images. | `tests/e2e/` |

The directory sets the level. The suite plugin marks each collected test with its
level and rejects any collected test outside a known `unit/`, `integration/` or
`e2e/` directory. Do not add level markers by hand. A test that needs a process,
socket or image belongs in `integration/`, even when it checks one function.

## Components and suites

A suite is named `<component>-<level>`. End-to-end tests form the single global
`e2e` suite. `tests/support/suites.py` is the registry, and a component without
tests at a level has no suite there.

| Suite | Location | Scope |
| --- | --- | --- |
| `platform-unit` | `agent_platform/tests/unit/` | Build, cache, release and privacy tooling; workflow, suite-registry and requirement-table invariants |
| `platform-integration` | `agent_platform/tests/integration/` | Release packaging with real `helm` and `git`; the suite plugin in real pytest sessions |
| `chart-integration` | `charts/tests/integration/` | Chart renders through real Helm and the packaged Hermes gateway Nginx configuration |
| `manager-unit` | `services/runtime-manager/tests/unit/` | Planner, admission, lifecycle, scopes and Pods against a fake clock and namespace |
| `manager-integration` | `services/runtime-manager/tests/integration/` | TLS API, SQLite upgrade, the `kubectl` adapter, chart-rendered configuration and the manager image |
| `tools-unit` | `services/tools/tests/unit/` | Recovery and bootstrap decisions |
| `tools-integration` | `services/tools/tests/integration/` | Restore into PostgreSQL 18, chart-rendered bootstrap RBAC and the tools image |
| `multica-unit` | `services/multica/tests/unit/` | The patched Multica Go tests, run by `go test` in the backend's Docker test stage |
| `multica-integration` | `services/multica/tests/integration/` | Native bootstrap against the built backend image and a disposable PostgreSQL |
| `hermes-unit` | `runtimes/hermes/tests/unit/` | The managed execution plugin and native configuration initialization |
| `hermes-integration` | `runtimes/hermes/tests/integration/` | The plugin against the real manager over HTTPS, task scripts and the Hermes image |
| `worker-integration` | `runtimes/agent-runtime/tests/integration/` | The worker image: SSH identity, host key pinning and shutdown |
| `e2e` | `tests/e2e/` | Hermes plugin through TLS to the manager and Kubernetes adapter; the Hermes image with real Hindsight and pgvector |

The chart's tests live in `charts/tests/` because the release policy allows only
reviewed files inside `charts/agent-platform/`. Shared fixtures and helpers live
in `tests/support/`, which the root `conftest.py` loads for every suite.
Component tests may import `tests.support`, but no test module imports another
test module. pytest uses `--import-mode=importlib`, so module names may repeat
across components.

## Markers

| Marker | Meaning |
| --- | --- |
| `unit`, `integration`, `e2e` | Set from the directory; never written by hand |
| `image` | Builds or runs a container image |
| `artifact` | Verifies a release image. The release builder runs these again against the pushed digests before it publishes the chart |
| `manual` | Needs credentials the repository does not hold; deselected unless `--manual` is given |

The only `manual` test is `test_optional_real_model_retain_and_recall`. It
qualifies Hindsight against a real LLM when `HINDSIGHT_TEST_LLM_PROVIDER`,
`HINDSIGHT_TEST_LLM_MODEL` and `HINDSIGHT_TEST_LLM_API_KEY` are set. It is
outside every CI suite and the release gate; the mock-LLM Hindsight cases are the
gated compatibility proof.

## Images under test

The `platform_image` fixture supplies each of the five release images by bake
target name. By default it builds the target once per session with
`docker buildx bake` from the same staged, reviewed contexts as a release and
tags it `agent-platform-test/<target>:local`. On CI it reads, but never writes,
the main-branch GitHub Actions cache. When `AGENT_PLATFORM_TEST_IMAGES` holds a
JSON object of target names to image references, the fixture uses those
references and builds nothing. The release builder sets it to the pushed digests
when it runs the `artifact` tests.

## Running locally

Use Python 3.14 and `uv sync --locked`. Helm 4 and Docker with Buildx are needed
for integration and image tests, plus OpenSSL and the OpenSSH client.

```bash
uv run pytest                                  # every suite
uv run pytest --suite manager-unit             # one suite
uv run pytest --suite hermes-unit,tools-unit   # several suites
uv run pytest -m 'not image'                   # skip container builds
uv run pytest --suite e2e --shard 2/3          # one shard of a suite
uv run pytest --manual -k real_model           # the credentialed Hindsight case
uv run python -m tests.support.suites list     # suites and their runner needs
```

A test whose prerequisite is missing skips locally. In CI, where `CI` is set, a
skip is a failure, so every selected test must run.

## CI

| Workflow | Trigger | Tests |
| --- | --- | --- |
| `Checks` (`checks.yml`) | Pull requests and pushes to `main` | None. Ruff, the privacy preflight, `helm lint`, the lightweight render, and test collection with suite assignment |
| `Tests` (`tests.yml`) | Called by `Release`; manual dispatch | The selected suites, one runner per suite or shard |
| `Release agent platform` (`release.yml`) | Version tags | A version check first, then all suites through `Tests`. The publish job starts only after every suite passes, and reruns the `artifact` tests on the pushed images |
| `Build image cache` (`cache.yml`) | Pushes to `main` | None. It builds the five image caches |

`tests.yml` takes a `suites` input, either `all` or a comma-separated list of
suite names. Its `plan` job collects every test, checks that each one belongs to
a registered suite, and expands the selection into a job matrix from
`tests/support/suites.py`. That registry holds each suite's shard count, timeout
and whether it needs Docker or Helm. Each matrix job is named after its suite,
for example `manager-integration` or `e2e (2/3)`, and runs
`uv run pytest --suite <name> --shard <i>/<n>`. Its JUnit report is uploaded with
seven-day retention.

To run tests on `main` commits later, add a job to `checks.yml` that calls
`tests.yml` with a narrower `suites` value, such as
`platform-unit,manager-unit,tools-unit,hermes-unit`.
A suite that grows too long gets more shards in the registry. Shards split the
suite's tests in collection order into contiguous parts of nearly equal size, so
each shard builds its own module fixtures once.
