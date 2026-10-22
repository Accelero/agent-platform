# Agent platform

Personal Hermes agents on Kubernetes, commissioned and governed by one Rust
manager.

- **Hermes commissioning.** Infrastructure writes per-user mappings and
  digest-pinned templates into PostgreSQL. The manager creates one private
  namespace per user, with retained home and workspace PVCs and a non-root Hermes
  Deployment. It applies image and configuration updates and retains user state
  on removal.
- **Terminal backends.** Users create Hermes profiles and select a terminal
  backend per profile (`terminal.runtime_id`) in the native dashboard. The bundled
  `managed_execution` plugin requests that backend from the manager. The manager
  enforces the instance's allowlist and owner storage policy. It creates the
  SSH worker on demand, pins its host key and retires it when the session closes,
  the worker idles or policy changes.
- **Administration.** The manager's React dashboard and versioned API manage
  assignments, templates, terminal runtimes, storage bindings and execution
  policy. Keycloak authenticates administrators; PostgreSQL grants authorize them.

Start with the [implementation status](docs/personal-hermes/implementation.md),
then the [native commissioning guide](docs/personal-hermes/native-commissioning.md)
and the [terminal backend guide](docs/personal-hermes/terminal-backends.md). The
plugin–manager wire interface is the
[execution contract](contracts/hermes-manager/execution.openapi.yaml).

## Artifacts

Source version **0.7.0** (unreleased). Released 0.6.x charts shipped the earlier
shared-Hermes platform and are not upgradable to this design.

| Image | Role |
| --- | --- |
| `ghcr.io/accelero/agent-platform/hermes-manager` | Rust API/dashboard, controller and router-auth executables |
| `ghcr.io/accelero/agent-platform/hermes` | Per-user Hermes image with the `managed_execution` plugin |
| `ghcr.io/accelero/agent-platform/agent-runtime` | Default `managed-ssh-v2` terminal worker image |
| `ghcr.io/accelero/agent-platform/multica-backend` | Optional standalone Multica board backend; not integrated yet |

The chart `oci://ghcr.io/accelero/agent-platform/charts/agent-platform` pins
these images by digest. It deploys the manager roles; the controller creates
per-user resources. See the [chart guide](charts/agent-platform/README.md).

## Layout

- `services/hermes-manager/`: Rust workspace (domain, application, adapters,
  executables) and the React/TypeScript dashboard.
- `contracts/hermes-manager/`: administration, execution, router and router-auth
  OpenAPI contracts and template schemas.
- `runtimes/hermes/`: Hermes image, personal init/run entrypoints, terminal
  policy patches and the `managed_execution` plugin.
- `runtimes/agent-runtime/`: SSH terminal worker image.
- `plugins/hermes-route-resolver/`: Traefik/Yaegi route resolver for future routing.
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
uv run python -m agent_platform.release --version 0.7.0 --revision "$(git rev-parse HEAD)"
```

Outputs are under ignored `.local/releases/`. Source defaults retain local `dev`
image tags; packaging injects release pins into a staging copy.
See [AGENTS.md](AGENTS.md) for repository guidance.

## Release process

1. Review passing checks and bump chart/project versions together. Let the trusted
   main cache workflow finish before tagging to reuse its build layers.
2. Push an explicit immutable tag such as `v0.7.0`.
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
