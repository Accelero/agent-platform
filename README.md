# Agent platform

Generic Hermes and Multica runtimes, worker isolation and a Helm chart, built on
GitHub-hosted runners and published to GHCR independently of deployment tooling.
The initial release is **0.1.0**. This source repository is private; deployment
artifacts are intended for anonymous public pulls.

## Artifacts

| Artifact | Versioned reference |
| --- | --- |
| Hermes controller | `ghcr.io/accelero/agent-platform/hermes:0.1.0` |
| Execution manager | `ghcr.io/accelero/agent-platform/execution-manager:0.1.0` |
| Worker | `ghcr.io/accelero/agent-platform/worker:0.1.0` |
| Patched Multica backend | `ghcr.io/accelero/agent-platform/multica-backend:0.1.0` |
| PostgreSQL/restic administrator tools | `ghcr.io/accelero/agent-platform/state:0.1.0` |
| OCI Helm chart | `oci://ghcr.io/accelero/agent-platform/charts/agent-platform`, version `0.1.0` |
| Matching release metadata | `ghcr.io/accelero/agent-platform/release:0.1.0` |

Images also receive `latest`. `release:latest` selects the matching image/chart
manifest digests and is advanced last. Deployment consumers resolve that pointer
once per run, then use its exact references so updates roll out coherently.
Versioned releases remain available for rollback.

A GitHub Release contains `artifacts.json`, `contract.zip` and the packaged chart.
The small contract ZIP carries chart defaults and the matching execution-policy
adapter. Consumers can validate policy and prepare values without this checkout.
The `state` image supplies binaries/libraries only; each deployment mounts its
reviewed backup/seed/restore scripts and configuration read-only at runtime.

## Development

Install Python 3.14, uv, Docker Buildx and Helm 3, then:

```bash
uv sync --locked
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
```

For an explicit local build without publishing:

```bash
uv run python -m agent_platform.release --version 0.1.0 --revision "$(git rev-parse HEAD)"
```

Build outputs go under ignored `.local/releases/`. Personal seeds, provider login,
SMTP settings, S3 keys and live state belong in the consuming infrastructure repo.
See [AGENTS.md](AGENTS.md) for layout and maintenance rules, and the
[chart documentation](charts/agent-platform/README.md) for its generic interface.
The [lightweight example](examples/lightweight/values.yaml) is a render fixture,
not a complete installation: its external database, claims and Secrets must exist.

## Release process

1. Review the sources and passing checks. Bump the chart/project version for a new release.
2. Push an explicit tag such as `v0.1.0`.
3. The release workflow validates the tag against source versions and builds on
   `ubuntu-latest` with no deployment credentials. It publishes all five images,
   the versioned OCI chart and their matching release bundle, then creates the
   GitHub Release with permanent assets.
4. GitHub initially creates GHCR packages as private. The package administrator
   must set the six runtime/metadata packages and chart package to public once in
   package settings. Cache packages can stay private. Anonymous registry checks
   must pass before deploying without pull credentials.

The workflow's repository-scoped `GITHUB_TOKEN` handles publishing. No registry
push token belongs in a deployment cluster. Existing release tags are immutable
by policy; use a new semantic version for changes rather than overwriting a release.

## Provenance

The initial runtime sources were imported from the reviewed prototype revision
recorded in [provenance/import.json](provenance/import.json), including the native
Multica authorization patch, maintenance fence and serialized worker handoff.
Only the upstream Multica server and license notices are retained. Source files
are ordinary reviewed files here, with no neighboring checkout or source archive
required for builds. Third-party components retain their original licenses.
