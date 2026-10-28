# Agent platform repository guidance

This repository owns generic runtime images, the Helm chart and versioned GHCR
releases. It contains no Pulumi program, cloud credentials, personal profiles,
mutable application state or deployment-specific backup scripts.

## Layout

- `agent_platform/`: shared BuildKit/cache configuration, release builder,
  privacy preflight and bounded command execution.
- `containers/docker-bake.hcl`: the three release image targets (`hermes`,
  `agent-runtime`, `hermes-manager`).
- `services/hermes-manager/`: Rust manager (`api` and `controller` roles),
  `hermes-gateway` and the React dashboard.
- `contracts/hermes-manager/`: OpenAPI contracts (administration, execution,
  route resolution, gateway) and template schemas.
- `runtimes/hermes/`: per-user Hermes image and the `managed_execution` plugin.
- `runtimes/agent-runtime/`: default SSH terminal worker image.
- `charts/agent-platform/`: self-contained application chart.
- `examples/manager/`: nonsecret manager render fixture.
- `tests/`: release invariants, chart and contract tests and acceptance fixtures
  (`tests/personal_hermes/`: the k3s suite runner, the native end-to-end run,
  its fake model and registry proxy).
- `doc/`: `SPEC.md` (specification) and `ARCH.md` (architecture); the guides
  and the implementation status are under `doc/personal-hermes/`.

## Manager design

`doc/SPEC.md` (what) and `doc/ARCH.md` (how) define the manager and gateway;
keep the guides in `doc/personal-hermes/` and the implementation status there in step
with the code. Documentation describes what is built: no plans, phases or
migration history. Manager code follows these rules:

- Every desired-state write is a `Command` run through the command envelope.
- Reconciliation is level-triggered: a pure planner, then idempotent
  server-side apply.
- A non-repeatable effect uses one of the five named mechanisms in the
  architecture. Add a new one only with a design change.
- `hermes-core` imports no I/O crate.
- Lifecycle states are typed enums, never string literals.
- No `serde_json::Value` crosses a port, except opaque `adminConfig`.
- Every requirement and acceptance row maps to tests in
  `services/hermes-manager/TESTS.md`; the `hermes-architecture` tests read the
  tables in `doc/SPEC.md` and fail on a missing row or test name.
- Contract cases in `hermes-core::app::testing::contracts` run on both the
  in-memory and the PostgreSQL store; add a port behaviour there first.

## Checks

Use Python 3.14 and `uv sync --locked`. After edits run:

```bash
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
```

Run the Rust checks in `services/hermes-manager/README.md` after manager edits.
After changes to the controller, the chart, the runtime image or the gateway,
also run the k3s suite and the native end-to-end run from the verification
commands in `doc/personal-hermes/implementation.md`, on disposable fixtures
only.
The OpenAPI contracts are the authority for every interface. Keep the plugin's
requests conformant to `execution.openapi.yaml`; change the contract, the Rust
handler and the plugin together.

Keep imported runtime/vendor code changes focused and review their Docker build
compatibility. Generic code must not import a deployment repository. Preserve
non-root workers and scoped mounts.
Keep external commands bounded and credentials out of output and build contexts.
Review the staged diff and `git diff --check` before committing.

## Releases

Source chart and project versions stay at the `0.0.0` placeholder; do not add
release numbers to documentation. A release requires an explicit
`vMAJOR.MINOR.PATCH` tag, or `vMAJOR.MINOR.PATCH-dev.N` for a prerelease of
unmerged work. GHCR already holds versions up to 0.6.9 under these package
names: never overwrite or reuse an existing version. The GitHub-hosted release
workflow validates, builds and publishes matching images and their
self-contained chart, and publishes the chart only after the images pass
validation. Keep the source and version OCI labels and the upstream license
notices in images. The chart owns configuration generation and exact image
defaults.

Public image layers and chart or receipt data must contain no secrets or
personal data. Treat personal prompts, identities, profiles, skills and memory
as sensitive too; keep them in the private deployment repository or live
mounted storage. Upstream public prompt templates remain part of pinned
software, separate from personal seeds. `release-policy.json` enumerates the
chart and every build input; release and cache builds stage only these reviewed
files. Never widen a context to include a deployment checkout. Run
`uv run python -m agent_platform.privacy` before publication; it reports rule
names without matched content. Keep ordinary Git attribution only with the
maintainer's approval; the existing attribution has been explicitly approved.
Publication and repository visibility changes require user authorization.

Build caches use GitHub Actions API v2 with distinct scopes per image. Keep the
trusted main-branch cache workflow separate from tag-only release publishing.
Never recreate GHCR cache packages or pass cache credentials in command arguments.
Keep the default cache storage limit unless the user authorizes paid storage.
