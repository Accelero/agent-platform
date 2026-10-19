# Agent platform

A Kubernetes application containing the Multica board, Hermes personal assistant,
HTTPS runtime manager and isolated agent runtimes. One versioned Helm chart owns
service wiring, gateway configuration and runtime isolation. Images and charts
are built on GitHub-hosted runners independently of deployment tooling.

The application chart is the deployment artifact. **0.5.1** is the current
release. GHCR deployment packages support anonymous public pulls once their
package visibility is public. Source and package visibility are managed separately.

## Artifacts

| Artifact | Versioned reference |
| --- | --- |
| Hermes assistant/controller | `ghcr.io/accelero/agent-platform/hermes:0.5.1` |
| Runtime manager | `ghcr.io/accelero/agent-platform/runtime-manager:0.5.1` |
| Agent command runtime | `ghcr.io/accelero/agent-platform/agent-runtime:0.5.1` |
| Patched Multica backend | `ghcr.io/accelero/agent-platform/multica-backend:0.5.1` |
| PostgreSQL/restic administrator tools | `ghcr.io/accelero/agent-platform/tools:0.5.1` |
| Application chart | `oci://ghcr.io/accelero/agent-platform/charts/agent-platform`, version `0.5.1` |

The packaged chart pins all five images by manifest digest in `values.yaml`.
The upstream Multica frontend also uses its reviewed digest. Containers remain
registry downloads; their filesystem layers are not embedded in the chart.
Image `latest` tags are convenient for inspection. Deployments select one chart
version or digest and use that chart's image defaults for a coherent application.

The GitHub Release attaches the same `agent-platform-0.5.1.tgz` that is published
to GHCR, plus an optional informational `artifacts.json`. Consumers need only the
chart. There is no configuration ZIP or custom release container package in this
release architecture. Obsolete `execution-manager`, `worker` and `release`
packages are retired.
Earlier releases that reference those package names are no longer deployable.
Superseded agent-runtime 0.2.0 through 0.4.0 and tools 0.4.0 images were also
retired after the [0.4.1 release qualification](docs/releases/0.4.1.md).
Release 0.5.1 adds chart-owned first-deployment bootstrap and explicit enrollment
repair Jobs. It retains the Hermes credential-mount correction. The chart gives the Hermes
pod filesystem group 1000 so its non-root process can read the group-restricted
runtime-manager token, CA and SSH key with mode 0440.

## Configuration and installation

Helm combines application defaults with administrator values. Its templates
produce Deployments, Services, gateway ConfigMaps, native runtime-manager YAML,
and matching Kubernetes admission/RBAC/network rules. Optional bootstrap Jobs
initialize the configured owner, workspace and assistant after verified recovery. The manager subsequently
creates short-lived agent-runtime Pods when an agent needs command execution.

```bash
helm install agents oci://ghcr.io/accelero/agent-platform/charts/agent-platform \
  --version 0.5.1 --namespace agents --values values.yaml
```

See the [chart interface and configuration example](charts/agent-platform/README.md).
The [lightweight example](examples/lightweight/values.yaml) enables the six core
Deployments. Provision its database, retained PVCs, named Secrets and initialized
native Hermes state before installation. Controllers are disabled in defaults
until those inputs are ready. Optional browser, memory and additional agent
services remain disabled unless deliberately configured with supported images.

An infrastructure tool such as Pulumi provisions that environment, supplies
values and deploys the chart. It does not generate the manager's native policy,
import Python from an artifact, or build/push images. Provider logins, SMTP keys,
personal seed files, backup orchestration and live state stay outside this repo.
The `tools` image supplies Restic, PostgreSQL 17 command-line tools, Python and
Kubernetes/S3/YAML libraries for short-lived administrator Jobs. Deployments
project their own reviewed backup/seed/restore scripts and mount persistent data.
An alternative tools image can provide these dependencies; publishing them once
keeps backup and recovery independent of package installation at runtime.

## Development

Use Python 3.14, uv, Docker Buildx and Helm 3:

```bash
uv sync --locked
uv run ruff check agent_platform tests services/tools/platform_bootstrap
uv run ruff format --check agent_platform tests services/tools/platform_bootstrap
uv run pytest -q
helm lint charts/agent-platform
```

For an explicit local build without publishing:

```bash
uv run python -m agent_platform.release --version 0.5.1 --revision "$(git rev-parse HEAD)"
```

Outputs are under ignored `.local/releases/`. Source defaults retain local `dev`
image tags; packaging injects release pins into a staging copy, preserving source.
See [AGENTS.md](AGENTS.md) for repository guidance.

## Release process

1. Review passing checks and bump chart/project versions together. Let the trusted
   main cache workflow finish before tagging to reuse its build layers.
2. Push an explicit immutable tag such as `v0.5.1`.
3. GitHub Actions builds/tests and publishes five versioned images, renders/tests
   the chart with their exact digests, and packages it. Image convenience tags are
   updated before the OCI chart is published. The chart is published last so a new
   stable chart always references images that already exist.
4. The workflow creates a GitHub Release with that exact chart archive and the
   informational receipt. It needs no cloud or deployment credentials.
5. GitHub creates new GHCR package names as private. An administrator makes each
   new runtime/chart package public once in package settings. Later versions keep
   that visibility. Anonymous checks must pass before deployment without secrets.

The repository-scoped `GITHUB_TOKEN` publishes artifacts. Source repository and
package visibility are separate; a private source repository's GitHub Release
assets require repository access, so anonymous deployment uses the public GHCR
chart. Never overwrite an existing version or recreate registry cache packages.

## Release privacy

Personal prompts, identities, profiles, skills, memory, credentials and live
settings belong in private deployment inputs and mounted storage. This repository
ships generic software and public upstream prompt templates. Its published images
do not need your private deployment checkout.

`release-policy.json` explicitly lists the owned files allowed in each Docker
context and in the chart. Release and cache builds stage fresh contexts from those
lists. Vendored files must match their recorded upstream hashes. Extra local files,
including ignored configs, never enter the contexts. Unexpected chart files,
inline prompt defaults, recognizable credentials and personal email addresses in
owned payloads fail the privacy preflight before any build or push. Example-domain
addresses and generic local identities are allowed. Existing Git author attribution
is approved for publication.

```bash
uv run python -m agent_platform.privacy
```

Review changes to the input lists and source content together. Match details are
kept out of diagnostics. Deployment tools supply private values only at install
time; they do not regenerate or republish the chart with those values.
Builds retain minimal supply-chain provenance and source/digest labels. The
builder's automatic GitHub event-payload capture is disabled, keeping incidental
event details out of future image attestations.

## Build cache

BuildKit uses GitHub Actions cache API v2 with one scope per image. Trusted `main`
pushes build all five targets with `type=cacheonly`. The separate tag release job
restores these caches with `--github-cache`. Local builds use local BuildKit cache.
Runtime cache credentials stay in masked runner environment variables. Cache
warming publishes no images or charts. The default 10 GB storage limit is retained.

## Provenance and upgrades

Imported prototype revisions and original hashes remain in
[provenance/import.json](provenance/import.json). Runtime sources rename the imported
`services/execution-manager` to `services/runtime-manager` and `runtimes/workers`
to `runtimes/agent-runtime`. The old policy adapter is replaced by chart templates.
Agent runtimes use the `worker` Unix account and `/home/worker` home.
Current settings use only the `runtime-manager` service, token and CA paths.
Old manager service aliases and credential mounts are intentionally unsupported.
The Multica credential boundary, maintenance fence,
non-root execution and serialized runtime handoff remain enforced.
