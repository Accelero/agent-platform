# Agent platform repository guidance

This repository owns generic runtime images, the Helm chart and versioned GHCR
releases. It contains no Pulumi program, cloud credentials, personal profiles,
mutable application state or deployment-specific backup scripts.

## Layout

- `agent_platform/`: release builder and bounded command execution.
- `containers/docker-bake.hcl`: the five supported release image targets.
- `runtimes/`: Hermes adapters and execution-worker build sources.
- `services/`: execution manager, patched Multica backend and generic state tools.
- `tools/platform/`: pinned command-line tools used by runtime images.
- `charts/agent-platform/`: generic chart and its deployment contract.
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
workflow validates, builds and publishes matching images, chart and metadata.
Do not overwrite an existing release version. Keep source/version OCI labels and
upstream license notices. Advance `release:latest` only after validation succeeds.
Public image layers and release metadata must contain no secrets or personal data.
Publication and repository visibility changes require user authorization.
