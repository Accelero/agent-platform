# Agent platform

Personal Hermes agents on Kubernetes, commissioned and governed by one Rust
manager and installed with one Helm chart. Every assigned person gets their
own Hermes instance behind one shared URL, with storage, settings and terminal
backends chosen by an administrator.

## What it does

- **A personal Hermes per person.** An administrator assigns a published
  template version to an exact Keycloak identity. The manager commissions that
  person's own instance in its own namespace: persistent private home, assigned
  volumes, its own credentials and keys, and one always-running Hermes, so
  native cron jobs fire without a browser. Signing in alone never creates
  anything.
- **One URL for everyone.** `hermes-gateway` signs people in through Keycloak
  and forwards each request only to the signed-in person's instance. People
  without an assignment get a denial page. Forged identity headers, foreign
  cookies and foreign WebSocket tickets reach nothing, and Hermes checks its
  owner again with its own login.
- **Central administration.** A dashboard and a versioned API manage who gets
  Hermes, versioned templates and terminal backends (Draft, Published,
  Withdrawn), volumes, write-only admin secrets, administrators and the audit
  log. Admin settings, such as the LLM provider, are enforced over the user's
  own settings; the user's other settings stay theirs. Admin secrets reach
  only the instances that reference them and are never returned.
- **Terminals on demand.** Hermes runs commands in SSH worker Pods, requested
  by the bundled `managed_execution` plugin. The manager starts them only for
  backends the assignment allows, mounts only the assigned volumes, supports
  private registries, reports pull, start and contract failures instead of
  falling back, and retires idle workers.
- **Storage from approved sources.** Storage classes, local paths on a node and
  NFS exports, declared by the operator. Paths never overlap between people. A
  shared workspace has the same path in Hermes and in the workers; the private
  home is never mounted in a worker.
- **Safe lifecycle.** Suspending or revoking someone denies new access within
  5 s and closes open connections within 10 s; re-enabling keeps the same data
  and credentials. Outages of the manager, the database or Keycloak fail
  closed.
- **Crash-safe and restorable.** All manager state is in PostgreSQL. A process
  killed at any point resumes without duplicating or destroying anything. A
  backup restored onto a new cluster comes back with `rebind-cluster`, and
  unknown namespaces are reported as orphans, never adopted.

## Documentation

1. [`doc/SPEC.md`](doc/SPEC.md): what the platform must do — requirements R1–R19 and
   the acceptance matrix.
2. [`doc/ARCH.md`](doc/ARCH.md): how it is built — components, mechanisms, security
   model, code layout and design decisions.
3. Guides: [instances, storage and admin settings](doc/personal-hermes/instances.md),
   [terminal backends](doc/personal-hermes/terminal-backends.md), the
   [dashboard](doc/personal-hermes/dashboard.md) and the
   [chart](charts/agent-platform/README.md).
4. [Implementation status](doc/personal-hermes/implementation.md): what is
   built, the evidence and the verification commands.
5. [`TESTS.md`](services/hermes-manager/TESTS.md): every requirement,
   acceptance row and contract operation mapped to its tests.

The OpenAPI contracts in [`contracts/hermes-manager/`](contracts/hermes-manager/)
are the authority for every interface: administration, execution (the plugin's
wire protocol), route resolution and the gateway.

## Artifacts

Source versions stay at the `0.0.0` placeholder. Stable releases are
`MAJOR.MINOR.PATCH`; builds of unmerged work are `MAJOR.MINOR.PATCH-dev.N`
prereleases above the latest stable version, published through the same pipeline
and never resolved as the latest release.

| Image | Role |
| --- | --- |
| `ghcr.io/accelero/agent-platform/hermes-manager` | Rust manager (`api` with the dashboard, `controller`, operator commands) and `hermes-gateway` |
| `ghcr.io/accelero/agent-platform/hermes` | Per-user Hermes image with the `managed_execution` plugin |
| `ghcr.io/accelero/agent-platform/agent-runtime` | Default `managed-ssh-v2` terminal worker image |

The chart `oci://ghcr.io/accelero/agent-platform/charts/agent-platform` pins
these images by digest. It deploys the manager roles and the gateway, the
controller's RBAC, the static admission policies, NetworkPolicies and one-shot
operator Jobs; the controller creates the per-user resources. See the
[chart guide](charts/agent-platform/README.md).

## Layout

- `services/hermes-manager/`: Rust manager, gateway and the React/TypeScript
  dashboard; see its [README](services/hermes-manager/README.md).
- `contracts/hermes-manager/`: administration, execution, route resolution and
  gateway OpenAPI contracts and the template and backend schemas.
- `runtimes/hermes/`: Hermes image, personal init and run entrypoints, terminal
  policy patches and the `managed_execution` plugin.
- `runtimes/agent-runtime/`: SSH terminal worker image.
- `charts/agent-platform/`: application chart.
- `examples/manager/`: nonsecret manager render fixture.
- `containers/docker-bake.hcl`: the three release image targets.
- `agent_platform/`: release builder, privacy preflight and bounded commands.
- `tests/`: Python tests and the `tests/personal_hermes/` cluster and
  end-to-end fixtures.

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

Chart tests call `helm`; it must be on `PATH`. The Rust, dashboard, k3s and
end-to-end checks are listed in the
[verification commands](doc/personal-hermes/implementation.md#verification-commands).
CI (`.github/workflows/checks.yml`) runs all of them on pull requests and on
pushes to `main`. For an explicit local build without publishing:

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
   informational receipt; `-dev.N` releases are marked as prereleases. It needs
   no cloud or deployment credentials.
5. New GHCR package names start private; an administrator makes them public once.

Never overwrite an existing version or recreate registry cache packages.

## Release privacy

Personal prompts, identities, profiles, skills, memory, credentials and live
settings belong in private deployment inputs and mounted storage, never in this
repository or its images. `release-policy.json` lists the owned files allowed in
each Docker context and in the chart. Release and cache builds stage fresh
contexts from those lists. The privacy preflight rejects unexpected chart files, inline prompt
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
