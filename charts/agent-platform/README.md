# Agent platform application chart

A chart is the versioned installation package for this application of interacting
services, similar in scope to a Compose application. Helm renders Kubernetes
resources; Kubernetes runs and reconciles them. An installed Helm release is an
instance of this chart, separate from the GitHub Release that distributes it.

Version **0.4.0** packages Multica backend/frontend, Hermes, two same-origin HTTP
gateways and a runtime manager, together with configuration and isolation rules.
Agent-runtime Pods are created on demand by the manager rather than pre-created
by Helm. Browser/search/memory and further agents remain optional and disabled.

## Native configuration generation

Administrators supply ordinary Helm values. `templates/_runtime.tpl` derives both:

1. `runtime-manager-config`, a ConfigMap whose `config.yaml` is the native manager
   configuration: clients and token-file references, allowed deployment/profile
   templates, lease/idle/recycle settings, runtime images, UID/GID, volumes and
   task/profile workspace mappings.
2. The admission allowlist used by `ValidatingAdmissionPolicy`: matching runtime
   roles, exact image references, literal environment values, selected PVC/Secret
   mounts and bounded per-task path patterns. The policy rejects different images,
   extra containers, host volumes, API tokens and unapproved credential mounts.

There is one source of settings. For example:

```yaml
runtimeManager:
  authSecret: my-manager-tokens
  tlsSecret: my-manager-tls
  uid: 1000
  gid: 1000
  clients:
    hermes:
      deployment: hermes-stack
      tokenKey: hermes-token
  config:
    templates:
      personal-assistant:
        session_idle_seconds: 60
        workdir_scope:
          kubernetes_claim: my-task-data
sandbox:
  namespace: agent-execution
```

Helm merges this with defaults, writes the client credential path into native YAML,
mounts that selected Secret key into the manager, and admits only the same task
claim and non-root identity. The default runtime image comes from the chart's
`images.agent-runtime` digest. A reviewed explicit template `image` override feeds
both outputs too. Templates reject duplicated roles/volumes, unsupported volume
sources, writable/unselected Secret mounts and unsafe subpaths. The values schema
rejects manually supplied `sandbox.roles`, preventing a second competing policy.
Changes to generated configuration update Pod checksums and roll the affected
application. No downloaded Python or secondary configuration bundle is needed.

`templates/_application.tpl` owns internal service URLs and native application
environment variables. Gateway nginx configs are packaged files. Database/mail,
public URL, owner, provider credentials and resource/storage choices are values.

## Installation inputs

Install one release per namespace, with Kubernetes 1.30+ and a CNI that enforces
NetworkPolicy. Service names are stable within that namespace. Defaults leave
workloads disabled until credentials, native homes and isolation inputs are ready.
The [lightweight example](../../examples/lightweight/values.yaml) enables the six
core Deployments and consumes external storage/database/Secrets.

Important values:

- `images`: release-pinned Hermes, runtime-manager, agent-runtime, backend and
  operations tools references. Packaged releases populate these exact manifest digests.
- `multica`: public URL, allowed owner, database connection/CA reference and outbound
  mail Secret/key names. Credentials are named references, not inline values.
- `hermes`: dashboard username, concurrency and provider Secret/key names.
- `runtimeManager`: native templates/clients, non-root identity, token and TLS Secret
  references. The server certificate must cover `runtime-manager` and the service
  DNS names used by clients. Mounts contain only selected existing keys.
- `workloads`: enable components and override resources, probes, ordered environment
  values, images or storage mounts. Set both gateways enabled for same-origin access.
- `persistence`: create retained claims or use `existingClaim` for externally managed
  storage. Runtime-visible claims must also exist in the sandbox namespace and
  refer to the intended storage; same names across namespaces do not share data.
- `sandbox`: execution namespace, service accounts, resource quota, public/private
  network destinations and Pod Security version. Admission roles are generated.
- `networkPolicy`: service dependencies and `namespaceIngress`, for example
  `{multica-gateway: [tailscale], hermes-gateway: [tailscale]}`. Gateways deny ingress
  until an explicit source is configured. The chart also accepts the older explicit
  `externalIngress` port rules when deliberately required.
- `existingSecret`: shared static app Secret; workloads/individual `secretEnv` entries
  can select their own named Secrets. `imagePullSecrets` supports private registries.
- `multicaGithub`: optional GitHub App integration and narrowly scoped webhook ingress.

For the lightweight defaults, provision `agent-platform-secrets` with Multica's
Postgres password/JWT secret and the Hermes dashboard/API keys; `multica-mail` and
`hermes-provider-credentials` with the configured mail/provider keys; manager bearer
and TLS Secrets; `execution-controller-ssh` with its Hermes private key; and the
sandbox's selected SSH/profile-login Secrets. Manager TLS clients select `tls.crt`
as their CA. Initialize Hermes home/profile files, identities and skills before
starting controllers, then register their Multica roster through normal auth.

The chart does not generate or seed credentials. Seed/export, database provisioning,
backups, bucket credentials, PVC backing and VPN ingress are infrastructure duties.
Mutable native settings live in retained storage, not Helm values. Claims and a
chart-created sandbox namespace are retained on uninstall; losing the node disk
still requires a separately designed backup/restore strategy.

## Release and upgrade

The pipeline tests images, injects exact image digests into a staging chart, renders
its lightweight deployment, and publishes that `.tgz` to GHCR and GitHub Releases.
No custom metadata image or ZIP is needed. Select the chart's SemVer version or OCI
digest. Tools implementing `latest` should select the newest stable chart once,
then deploy its immutable digest with its own image defaults.

Controllers are single-replica Recreate Deployments to avoid concurrent writers.
Runtime manager state remains on `manager-state`; native runtime paths use the
`worker` Unix account. Configure clients with `https://runtime-manager:8090`,
`/run/secrets/runtime-manager-token` and `/etc/runtime-manager/tls/ca.crt`.
The obsolete manager alias, token/CA paths and `legacyServiceAlias` option are
removed. Old client configurations must be updated before starting the application.

The obsolete `execution-manager`, `worker` and `release` GHCR packages are removed.
Historical chart/Git tags are retained, but releases depending on those deleted
packages are no longer supported. Publish a new version for all subsequent changes.
