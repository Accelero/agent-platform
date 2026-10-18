# Agent platform repository guidance

This repository owns generic runtime images, the Helm chart and versioned GHCR
releases. It contains no Pulumi program, cloud credentials, personal profiles,
mutable application state or deployment-specific backup scripts.

## Layout

- `agent_platform/`: shared BuildKit/cache configuration, release builder and
  bounded command execution.
- `containers/docker-bake.hcl`: the five supported release image targets.
- `runtimes/`: Hermes adapters and agent-runtime build sources.
- `services/`: runtime manager, patched Multica backend and generic state tools.
- `tools/platform/`: pinned command-line tools used by runtime images.
- `charts/agent-platform/`: self-contained application chart and native configuration templates.
- `examples/lightweight/`: nonsecret render fixture with optional services disabled.
- `vendor/multica/`: pinned upstream server source and upstream license notices.
- `provenance/`: origin revisions, original source hashes and extraction notes.
- `tests/`: release invariants and offline runtime smoke payloads.

## Checks

Use Python 3.14 and `uv sync --locked`. After edits run:

```bash
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
```

Keep imported runtime/vendor code changes focused and review their Docker build
compatibility. Generic code must not import a deployment repository. Preserve
non-root workers, scoped mounts and the Multica human-token authorization boundary.
Keep external commands bounded and credentials out of output and build contexts.
Review the staged diff and `git diff --check` before committing.

## Releases

A release requires an explicit `vMAJOR.MINOR.PATCH` tag. The GitHub-hosted release
workflow validates, builds and publishes matching images and their self-contained chart.
Do not overwrite an existing release version. Keep source/version OCI labels and
upstream license notices. Publish the chart only after matching images pass validation.
The chart owns configuration generation and exact image defaults; do not reintroduce
a required metadata container, configuration ZIP or downloaded Python adapter.
Public image layers and chart/receipt data must contain no secrets or personal data.
Publication and repository visibility changes require user authorization.

Build caches use GitHub Actions API v2 with distinct scopes per image. Keep the
trusted main-branch cache workflow separate from tag-only release publishing.
Never recreate GHCR cache packages or pass cache credentials in command arguments.
Keep the default cache storage limit unless the user authorizes paid storage.
