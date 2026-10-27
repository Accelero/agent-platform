# Agent platform repository guidance

This repository owns generic runtime images, the Helm chart and versioned GHCR
releases. It contains no Pulumi program, cloud credentials, personal profiles,
mutable application state or deployment-specific backup scripts.

## Layout

- `agent_platform/`: shared BuildKit/cache configuration, release builder,
  privacy preflight and bounded command execution.
- `containers/docker-bake.hcl`: the four supported release image targets.
- `services/hermes-manager/`: Rust manager (`api` and `controller` roles),
  `hermes-gateway` and the React dashboard.
- `contracts/hermes-manager/`: OpenAPI contracts (administration, execution,
  route resolution, gateway) and template schemas.
- `runtimes/hermes/`: per-user Hermes image and the `managed_execution` plugin.
- `runtimes/agent-runtime/`: default SSH terminal worker image.
- `charts/agent-platform/`: self-contained application chart.
- `examples/manager/`: nonsecret manager render fixture.
- `services/multica/`, `vendor/multica/`: optional standalone board backend,
  pinned upstream server source and upstream license notices.
- `provenance/`: origin revisions, original source hashes and extraction notes.
- `tests/`: release invariants, chart and contract tests and acceptance fixtures.

## Manager design

`docs/personal-hermes/specification.md` (what) and `architecture.md` (how)
define the manager and gateway. `manager-rewrite.md` records how the previous
design was replaced. Manager code follows these rules:

- Every desired-state write is a `Command` run through the command envelope.
- Reconciliation is level-triggered: a pure planner, then idempotent
  server-side apply.
- A non-repeatable effect uses one of the five named mechanisms in the
  architecture. Add a new one only with a design change.
- `hermes-core` imports no I/O crate.
- Lifecycle states are typed enums, never string literals.
- No `serde_json::Value` crosses a port, except opaque `adminConfig`.
- Every requirement and acceptance row maps to tests in
  `services/hermes-manager/TESTS.md`.

## Checks

Use Python 3.14 and `uv sync --locked`. After edits run:

```bash
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
```

Run the Rust checks in `services/hermes-manager/README.md` after manager edits.
The OpenAPI contracts are the authority for every interface. Keep the plugin's
requests conformant to `execution.openapi.yaml`; change the contract, the Rust
handler and the plugin together.

Keep imported runtime/vendor code changes focused and review their Docker build
compatibility. Generic code must not import a deployment repository. Preserve
non-root workers, scoped mounts and the Multica human-token authorization boundary.
Keep external commands bounded and credentials out of output and build contexts.
Review the staged diff and `git diff --check` before committing.

## Releases

GHCR already holds stable versions 0.1.0–0.6.9 of the previous design. Source chart
and project versions stay at the `0.0.0` placeholder. Do not add release numbers to
documentation. A release requires an explicit `vMAJOR.MINOR.PATCH` tag; builds of
unmerged work use `vMAJOR.MINOR.PATCH-dev.N` prereleases above the latest stable
version, which deployments never resolve as the latest release. The GitHub-hosted release
workflow validates, builds and publishes matching images and their self-contained chart.
Do not overwrite an existing release version. Keep source/version OCI labels and
upstream license notices. Publish the chart only after matching images pass validation.
The chart owns configuration generation and exact image defaults; do not reintroduce
a required metadata container, configuration ZIP or downloaded Python adapter.
Public image layers and chart/receipt data must contain no secrets or personal data.
Treat personal prompts, identities, profiles, skills and memory as sensitive too.
Keep them in the private deployment repository or live mounted storage. Upstream
public prompt templates remain part of pinned software, separate from personal seeds.
`release-policy.json` enumerates chart and owned build inputs. Both release and
cache builds stage only these reviewed files; vendored inputs must match their
recorded upstream hashes. Never widen a context to include a deployment checkout.
Run `uv run python -m agent_platform.privacy` before publication. It reports rule
names without matched content. Keep ordinary Git attribution only with the
maintainer's approval; the existing attribution has been explicitly approved.
Publication and repository visibility changes require user authorization.

Build caches use GitHub Actions API v2 with distinct scopes per image. Keep the
trusted main-branch cache workflow separate from tag-only release publishing.
Never recreate GHCR cache packages or pass cache credentials in command arguments.
Keep the default cache storage limit unless the user authorizes paid storage.
