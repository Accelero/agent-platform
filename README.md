# Agent platform

Personal Hermes agents on Kubernetes, commissioned and governed by one Rust
manager and installed with one Helm chart.

- **Instances.** Administrators decide which SSO users get a Hermes agent, with
  which template, volumes and terminal backends. The manager creates a private
  namespace per user with persistent storage and one always-running Hermes,
  enforces admin settings (for example the LLM provider) and admin secrets, and
  retains all data on suspension or revocation.
- **Terminal backends.** Users pick a terminal backend per Hermes profile
  (`terminal.runtime_id`). The bundled `managed_execution` plugin requests it
  from the manager. The manager creates an SSH worker only for backends that
  user is allowed to use, mounts only the volumes the administrator assigned,
  and retires the worker when it is no longer needed.
- **Routing.** One Hermes URL for everyone. A Rust gateway authenticates the
  user with Keycloak and forwards them to their own instance.
- **Administration.** A React dashboard on the manager's versioned API.
  Keycloak authenticates administrators and PostgreSQL grants authorize them.
  All manager state is in PostgreSQL, and the manager recovers from crashes
  without duplicating or destroying anything.

Read these in order:

1. [specification](docs/personal-hermes/specification.md) (what);
2. [architecture](docs/personal-hermes/architecture.md) (how);
3. the guides for [instances](docs/personal-hermes/instances.md),
   [terminal backends](docs/personal-hermes/terminal-backends.md) and the
   [dashboard](docs/personal-hermes/dashboard.md).

The [implementation status](docs/personal-hermes/implementation.md) records
what is proven and what is still open, and the
[rewrite guide](docs/personal-hermes/manager-rewrite.md) records how the
previous design was replaced. The plugin–manager wire interface is the
[execution contract](contracts/hermes-manager/execution.openapi.yaml).

## Artifacts

Source versions stay at the `0.0.0` placeholder. Stable releases are
`MAJOR.MINOR.PATCH`; builds of unmerged work are `MAJOR.MINOR.PATCH-dev.N`
prereleases above the latest stable version, published through the same pipeline
and never resolved as the latest release.

| Image | Role |
| --- | --- |
| `ghcr.io/accelero/agent-platform/hermes-manager` | Rust manager (`api` with the dashboard, `controller`) and `hermes-gateway` |
| `ghcr.io/accelero/agent-platform/hermes` | Per-user Hermes image with the `managed_execution` plugin |
| `ghcr.io/accelero/agent-platform/agent-runtime` | Default `managed-ssh-v2` terminal worker image |
| `ghcr.io/accelero/agent-platform/multica-backend` | Optional standalone Multica board backend; not integrated yet |

The chart `oci://ghcr.io/accelero/agent-platform/charts/agent-platform` pins
these images by digest. It deploys the manager roles and the gateway, with the
controller's RBAC and the static admission policies; the controller creates
per-user resources. See the [chart guide](charts/agent-platform/README.md).

## Layout

- `services/hermes-manager/`: Rust manager, gateway and the React/TypeScript
  dashboard; `TESTS.md` maps every requirement to its tests.
- `contracts/hermes-manager/`: administration, execution, route resolution and
  gateway OpenAPI contracts and template schemas.
- `runtimes/hermes/`: Hermes image, personal init/run entrypoints, terminal
  policy patches and the `managed_execution` plugin.
- `runtimes/agent-runtime/`: SSH terminal worker image.
- `charts/agent-platform/`: application chart.
- `services/multica/`, `vendor/multica/`: optional board backend and pinned upstream source.
- `agent_platform/`: release builder, privacy preflight and bounded commands.
- `tests/`: Python unit tests and `tests/personal_hermes/` acceptance fixtures.

## Development

Use Python 3.14, uv, Docker Buildx, Helm 3 and the Rust toolchain in
`services/hermes-manager/rust-toolchain.toml`:

```bash
uv sync --locked
uv run ruff check agent_platform tests
uv run ruff format --check agent_platform tests
uv run pytest -q
helm lint charts/agent-platform
uv run python -m agent_platform.privacy
```

Chart tests call `helm`; it must be on `PATH`. Rust and acceptance checks are
listed in the [verification commands](docs/personal-hermes/implementation.md#verification-commands).
For an explicit local build without publishing:

```bash
uv run python -m agent_platform.release --version 0.7.0-dev.1 --revision "$(git rev-parse HEAD)"
```

Outputs are under ignored `.local/releases/`. Source defaults retain local `dev`
image tags; packaging injects release pins into a staging copy.
See [AGENTS.md](AGENTS.md) for repository guidance.

## Release process

1. Review passing checks and set the chart (`version`, `appVersion`) and
   `pyproject.toml` versions to the release version together; the workflow
   rejects a tag that does not match them. Let the trusted main cache workflow
   finish before tagging to reuse its build layers.
2. Push an explicit immutable tag: `vMAJOR.MINOR.PATCH`, or `vMAJOR.MINOR.PATCH-dev.N`
   for a prerelease of unmerged work.
3. GitHub Actions builds, tests and publishes the versioned images, renders and
   tests the chart with their exact digests, and publishes the chart last.
4. The workflow creates a GitHub Release with that chart archive and an
   informational receipt. It needs no cloud or deployment credentials.
5. New GHCR package names start private; an administrator makes them public once.

Never overwrite an existing version or recreate registry cache packages.

## Release privacy

Personal prompts, identities, profiles, skills, memory, credentials and live
settings belong in private deployment inputs and mounted storage, never in this
repository or its images. `release-policy.json` lists the owned files allowed in
each Docker context and in the chart. Release and cache builds stage fresh
contexts from those lists, and vendored files must match recorded upstream
hashes. The privacy preflight rejects unexpected chart files, inline prompt
defaults, recognizable credentials and personal email addresses before any
build, reporting rule names only:

```bash
uv run python -m agent_platform.privacy
```

## Build cache

BuildKit uses the GitHub Actions cache API v2 with one scope per image. Trusted
`main` pushes warm the caches with `type=cacheonly`; the tag release job restores
them with `--github-cache`. Cache warming publishes nothing, and the default 10 GB
storage limit is retained.
