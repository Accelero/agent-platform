# Agent platform application chart

The chart installs personal Hermes: the manager's `api` and `controller` roles
and `hermes-gateway`, all from the one `hermes-manager` image. The controller
commissions one Hermes instance per assigned user and their terminal backends;
instance namespaces, Deployments, volumes and worker Pods come from PostgreSQL,
not from Helm. See the [instances guide](../../doc/personal-hermes/instances.md)
and the [terminal backend guide](../../doc/personal-hermes/terminal-backends.md).

## Installation inputs

Install one release per namespace, with Kubernetes 1.30+ (for
ValidatingAdmissionPolicy) and a CNI that enforces NetworkPolicy.
`hermesManager.enabled` is off by default.

- `images`: release-pinned `hermes-manager`, `hermes` and `agent-runtime`
  references. Packaged releases inject exact manifest digests.
  The chart deploys `hermes-manager`; administrators use the `hermes` and
  `agent-runtime` digests in templates and terminal backends.
- `hermesManager`: installation identity and issuer, `dataIdentity`,
  `storageSources`, the public and administration origins, per-role settings
  and existing Secret names (below), and ingress peers.
- `imagePullSecrets`: references for private images.

The [manager example](../../examples/manager/values.yaml) is a nonsecret shape
reference.

## What the chart installs

| Object | Purpose |
| --- | --- |
| `hermes-manager-api` Deployment and Service | Administration API and dashboard (8443), route resolver over mTLS (8444), execution API (8445). No Kubernetes token |
| `hermes-manager-controller` Deployment and ServiceAccount | Leader-elected reconciliation. The only Pod with a Kubernetes token |
| `hermes-gateway` Deployment and Service | The Hermes hostname: sign-in, routing and proxying (8443). No Kubernetes token |
| Controller RBAC | Cluster rights to create instance namespaces and static volumes and to bind the instance role; the instance role; its leader Lease |
| ValidatingAdmissionPolicies | The installation's static policies, with storage roots, Hermes data identity and names rendered in as literals. Worker Pods use the non-zero UID/GID declared by their backend version. They mirror `hermes-render`; the chart tests compare them |
| NetworkPolicies | The resolver admits only the gateway, the execution API only instance Hermes Pods, the controller nothing |

Each role reads one existing Secret, mounted read-only at
`/run/hermes/secrets`, with these keys. The chart never renders a Secret, and
values hold no secret material:

| Role | Keys |
| --- | --- |
| `api` | `database-url`, `admin-tls.crt`, `admin-tls.key`, `execution-tls.crt`, `execution-tls.key`, `resolver-tls.crt`, `resolver-tls.key`, `resolver-client-ca.crt`, `secret-sealing.pub`; with browser sign-in `session-database-url`, `session-key-<id>` per session key and, for a confidential client, `oidc-client-secret`; with the directory `directory-client-secret` |
| `controller` | `database-url`, `secret-sealing.key`, `execution-ca.crt` |
| `gateway` | `database-url` (role limited to `gateway_sessions`), `tls.crt`, `tls.key`, `session-key-<id>` per session key, `resolver-ca.crt`, `resolver-client.crt`, `resolver-client.key`; for a confidential client `oidc-client-secret` |

With `oidcPrivateCa: true`, every role's Secret also carries `oidc-ca.crt`.
The execution certificate must name `hermes-manager-api.<namespace>.svc`, and
so must the resolver certificate. The gateway's client certificate carries the
URI SAN given as `api.gatewayIdentity`.

## Operator steps

Migration, initialization and the first administrator are explicit one-shot
Jobs, enabled for one upgrade and then disabled:

1. `operations.migrate` runs `hermes-manager migrate` with the schema owner's
   `database-url` from its own Secret.
2. `operations.initialize` records the installation and the cluster identity,
   as the controller's ServiceAccount.
3. `operations.grantAdministrator` grants the identity in its Secret's
   `identity.json` (`{"issuer": …, "subject": …}`); set `recover` to restore a
   grant when grants already exist.

After restoring PostgreSQL on a new cluster, `operations.rebindCluster` runs
`hermes-manager rebind-cluster` as the controller's ServiceAccount instead of
`initialize`.

The roles refuse to start until the registry is migrated and initialized for
this installation.

## Exposure

Services are ClusterIP; set `gateway.service.type` for a LoadBalancer. List the
peers that may reach the administration API and the gateway in
`hermesManager.ingress`; empty lists deny ingress. The gateway terminates TLS
itself, so a proxy in front of it passes TLS through. Never expose the resolver
or execution listeners. Secret rotation requires restarting the affected role.

Build and test locally, without publication or a deployment kubecontext:

```bash
uv sync --locked
uv run python -m agent_platform.manager_build
helm lint charts/agent-platform
uv run pytest -q tests/integration/test_manager_chart.py
```

## Release and upgrade

The release pipeline tests images, injects exact image digests into a staging
chart, renders the manager example and publishes that `.tgz` to GHCR and GitHub
Releases. Select the chart's SemVer version or OCI digest and deploy it with its
own image defaults. The controller runs one active replica by leader election;
`api` and `gateway` can run several. Source charts carry the `0.0.0`
development placeholder until a release sets the version.

`hermesManager.dataIdentity` is the identity of every Hermes container and the
default for workers unless a backend declares another non-zero UID/GID. Backend
identities have no installation allow-list. Shared-volume access with different
identities follows Unix permissions; backend-only volumes support another
identity. The private Hermes home stays at the data identity.
