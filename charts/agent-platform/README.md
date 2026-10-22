# Agent platform application chart

The chart installs the Rust personal Hermes manager: its API/dashboard,
controller and router-auth roles. The manager commissions one Hermes instance per
user and provisions that user's terminal workers on demand. Per-user namespaces,
Deployments, PVCs and worker Pods are created by the controller from database
templates and policy, not rendered by Helm. See the
[native commissioning guide](../../docs/personal-hermes/native-commissioning.md)
and the [terminal backend guide](../../docs/personal-hermes/terminal-backends.md).

An optional, standalone Multica board backend remains in the chart. It is not
integrated with the manager or personal Hermes instances.

## Installation inputs

Install one release per namespace, with Kubernetes 1.30+ and a CNI that enforces
NetworkPolicy. Every role is disabled by default.

- `images`: release-pinned `hermes-manager`, `hermes`, `agent-runtime` and
  `multica-backend` references. Packaged releases inject exact manifest digests.
  The chart deploys `hermes-manager` (and `multica-backend` when enabled);
  infrastructure uses the `hermes` and `agent-runtime` digests in instance
  templates and terminal runtime versions.
- `hermesManager`: installation identity, per-role native configuration,
  existing Secret names and listener ingress peers (below).
- `imagePullSecrets`: references for private manager images.

## Rust manager and dashboard

The `hermesManager` section enables the
`hermes-manager` image target. The same non-root image contains both Rust
executables and the compiled React dashboard. `api`, `controller` and
`routerAuth` can be enabled independently. The dashboard is served only when the
API's `admin.browser` session configuration is present; without it the API runs
in API-only mode. The [manager example](../../examples/manager/values.yaml) is a
nonsecret shape reference for the API/dashboard role. Configure public origins,
issuer and exact client IDs for your environment. The chart writes native JSON
configuration and fixes API ports 8443/8444/8445 and router ports 8446/8447.

Supply private files through each role's separately named existing Secret,
mounted read-only at `/run/manager/secrets`. Database URLs, client secrets, TLS
keys, encryption keys and any bootstrap identity belong there, never in values,
ConfigMaps or the chart archive. Keyring files may reference sibling Secret files.
Use the [operator guide](../../docs/personal-hermes/dashboard.md) for PostgreSQL
grants and optional read-only Keycloak directory permissions.

Only the controller mounts a Kubernetes token. Before enabling it, render, review
and apply the manager's namespace/RBAC/admission bootstrap using the
[`render-bootstrap` procedure](../../docs/personal-hermes/native-commissioning.md#installation),
then supply that existing `serviceAccountName`. The chart does not create a
privileged catch-all role. Initialize a fresh registry with verified inventory,
or migrate an existing registry in maintenance, through explicit operator
commands before starting workloads. No Helm hook automatically
initializes/migrates a database or grants administrators.

Services are ClusterIP. Configure `hermesManager.ingress` peers independently for
admin, mTLS resolver, execution, router public and router internal listeners.
Empty peer lists deny ingress. Personal Hermes Pods need the execution listener
(8445). Configure external ingress separately with correct TLS/origins; do not
expose the resolver/execution listeners as the admin site. Secret rotation
requires restarting the affected role.

Build and test locally, without publication or an existing deployment kubecontext:

```bash
uv sync --locked
uv run python -m agent_platform.manager_build
helm lint charts/agent-platform
helm package charts/agent-platform --destination .local/charts
uv run python -m tests.personal_hermes.dashboard_chart \
  --chart .local/charts/agent-platform-0.7.0.tgz --image hermes-manager:local
```

The fixture needs Docker, Helm, OpenSSL and k3d v5.9.0. It creates a disposable
local digest-pinned k3s cluster, imports the local image and installs the actual
packaged chart using synthetic TLS/identity/PostgreSQL state. It verifies
administrator denial/grants, directory service-token isolation and Pod restart,
then removes its containers/cluster. For full native lifecycle regression on the
same local toolchain, run the existing Kubernetes fixture with `--k3d --native`
and a disposable `--postgres-container`. It never reads or changes a deployment
kubecontext. Source chart image defaults remain local `dev` names; pass the
imported local image as the fixture does.

## Optional Multica board

`workloads` keeps the Multica backend, web frontend, same-origin gateway and an
optional shared PostgreSQL StatefulSet, all disabled by default. `multica`
supplies the public URL, allowed owner, database URL/CA Secret and mail Secret
keys; `multicaGithub` optionally wires a GitHub App and webhook Ingress.
`persistence` creates retained claims only when enabled without `existingClaim`.
Personal Hermes instances have no board integration yet.

## Release and upgrade

The release pipeline tests images, injects exact image digests into a staging
chart, renders the manager example and publishes that `.tgz` to GHCR and GitHub
Releases. Select the chart's SemVer version or OCI digest and deploy it with its
own image defaults. Manager roles are single-replica Recreate Deployments.

The 0.7.0 chart removes the shared Hermes Deployment, its gateways, the Python
runtime manager, sandbox namespace, bootstrap Jobs, optional browser/search/memory
services and the `tools` image. There is no in-place upgrade from a 0.6.x
installation.
