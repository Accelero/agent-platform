# Agent platform chart

Platform chart, version 0.1.0. Packages internal supporting services, agent
controllers, the execution manager and worker execution policies. Controllers
are disabled by default until administrators supply initialized native homes,
credentials and sandbox policy. It has no Flux dependency, seed hooks or
generated credential Secrets.

Install one release per namespace. Service/workload names are stable so native
clients use Kubernetes DNS without release-name-specific configuration.

Important values:

- `existingSecret`: default static credential reference. A workload can select
  its own `existingSecret`; each `secretEnv` key can also select a named Secret.
- `workloads`: enabled components, Deployment/StatefulSet kind, prebuilt images, ordered environment values,
  resources, probes and mounts. Password references are emitted before URLs
  that use Kubernetes environment expansion.
- `persistence`: capacity, access modes, optional storage class/volume name,
  or `existingClaim` for externally managed storage.
- `networkPolicy`: default-deny ingress with explicit service dependencies.
- `imagePullSecrets`: preprovisioned pull credentials for private images.
- `sandbox`: execution namespace, worker service account, role image/mount/env
  allowlists, admission policies, network permissions and resource quota.
- `multicaGithub`: optional GitHub App identity and existing Secret references
  for the backend, plus a TLS ingress limited to webhook/setup routes. This
  optional integration is deferred by the lightweight stack.

Chart-created claims are retained on uninstall. Retention does not preserve a
VPS's disk on VPS destruction. Existing claims/Secrets are not owned by the
chart. Native mutable agent configuration is stored outside Helm values.

Shared PostgreSQL, RabbitMQ and append-only Redis use single-replica StatefulSets
and separate headless Services for stable identities. Their explicit PVCs are
retained rather than coupled to a StatefulSet's deletion or replica count.
Hermes/Codex controllers remain single-replica Recreate Deployments: a second
writer never starts against the same native home during an upgrade.

The chart has no seed hooks. The consuming deployment initializes empty homes/skills before
controllers are enabled, then register the Multica roster separately. The
bootstrap ingress rule allows those trusted platform-namespace Jobs to call
Multica without granting access to jobs created by the execution manager in the
separately restricted execution namespace.

Default `:dev` image values are placeholders. Select published GHCR image digests
from the matching release receipt. The independent
[release workflow](../../.github/workflows/release.yml) publishes this chart as an
OCI artifact. Version 0.1.0 is the initial release; the consumer resolves the
matching `release:latest` metadata before rendering values. Public packages need
no pull Secrets. Enable controllers after administrator bootstrap supplies native
state and sandbox policies. See the [repository README](../../README.md).

The manager uses a file-level bearer Secret mount so its Kubernetes service-account
volume can coexist under /run/secrets. Supply a native kubeconfig in the manager
ConfigMap using service-account tokenFile and CA paths, without embedding a token.

## Worker execution boundary

The `templates/sandbox/` resources install with this release even while
controllers are disabled. They create or reference `sandbox.namespace`, enforce
restricted Pod Security and validate worker Pods against `sandbox.roles`.
The manager service account lives in the release namespace; its RoleBinding
allows only Pod get/create/delete in the execution namespace. Worker service
accounts receive no API token. Worker Pods are created dynamically by the
execution manager, not by Helm.

Configure each role's exact images, literal environment and selected PVC/Secret
mounts. The administrator-owned worker configuration and the policy adapter derive
matching manager and admission configuration. Secret mounts require exact keys
and read-only access. Storage and credential provisioning remain separate;
this chart contains named references only and never seeds agents.

Network policies allow controller SSH on port 2222, DNS, configured tool
services and permitted public Internet destinations. Operator alone receives
configured app API/gateway access through `sandbox.network.operatorAppEndpoints`.
List private cluster/node/VPN CIDRs in `sandbox.network.excludedCIDRs` and use a
CNI that enforces these policies. Service aliases resolve tool names into the
controller namespace. The execution namespace is retained on uninstall.

`sandbox.policyIdentity` defaults to `agent-sandbox` and determines policy names
and namespace selectors.
