# Agent platform application chart

A chart is the versioned installation package for this application of interacting
services, similar in scope to a Compose application. Helm renders Kubernetes
resources; Kubernetes runs and reconciles them. An installed Helm release is an
instance of this chart, separate from the GitHub Release that distributes it.

Version **0.4.1** packages Multica backend/frontend, Hermes, two same-origin HTTP
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

## First deployment and explicit enrollment repair

Enable `bootstrap.enabled` to deploy the automatic Job and the permanently
suspended `<release>-bootstrap` CronJob template. Set `multica.ownerEmail`,
`bootstrap.workspace`, a stable `bootstrap.installation`, and the restore gate
and lifecycle command contract below. This currently enrolls the shipped Hermes
controller/profile and its `agent-runtime` template. Terminal Pods receive task
context; they do not register a second daemon. This is separate from future
multi-user runtime provisioning.

The Job waits for native schema migrations, then checks application users and
workspaces. Existing/restored installations are successful no-ops, including
changed or absent enrollment credentials. Native IDs and live settings are not
reconciled from Helm values. The installation journal and the existing onboarding
Secret allow interrupted, known bootstrap work to resume without duplicate PATs,
users, workspaces or agents. Unknown/foreign state is never adopted automatically.

Deployment integration must provide:

- `bootstrap.recoveryGate`: a ConfigMap with `record.json` containing
  `installation`, `ready: true`, `fresh: true|false` and `namespaceUIDs`, mapping
  the platform and execution namespaces to their current UIDs. Publish it only
  after archive selection and restore finish. `fresh` means the backup archive
  was positively checked empty, not that storage could not be read. A restored
  empty database cannot become a fresh installation.
- `bootstrap.lifecycle.before`, `after` and `checkpoint`: command argument arrays.
  Before mutation, create a verified checkpoint, fence dispatch, drain work and
  stop all native writers. After successful reconnection, restore dispatch and
  writers. Finally publish a verified encrypted checkpoint including the native
  database, home and both enrollment Secrets. Commands must fail on any missing
  durability prerequisite. The Job captures private command output.
- `bootstrap.volumes`, `volumeMounts`, `env` and `envFrom`: private deployment
  inputs for those commands; keep credentials in referenced Secrets. Optional
  `agentSeedFile` loads initial agent instructions from a mounted private JSON
  file, keeping prompts out of public charts/images/releases.
- `bootstrap.leaseName`: the installation-wide operation Lease (default
  `agent-state-operation`). Backup/recovery commands must honor it. A child
  command in the same Pod may reuse the parent's lease without releasing it.
  Use `existingServiceAccount` when lifecycle commands require additional
  deployment-owned permissions. The chart grants only enrollment permissions;
  the Job is an administrator workload, never a worker Pod.

The automatic Job uses a new name for each Helm revision; install/upgrade must
wait for Jobs. The manual template never runs on a schedule. For a normal retry:

```bash
kubectl -n agents create job --from=cronjob/agents-bootstrap agents-bootstrap-manual
kubectl -n agents wait --for=condition=complete job/agents-bootstrap-manual --timeout=1200s
```

For explicit repair, generate a one-off Job and add `--force` to its container
arguments before creating it (the template and Helm values stay unchanged):

```bash
kubectl -n agents create job --from=cronjob/agents-bootstrap agents-bootstrap-repair \
  --dry-run=client -o json |
  jq '.spec.template.spec.containers[0].args = ["--force"]' |
  kubectl -n agents create -f -
```

Force requires the original valid native `daemon.id`; restore it from backup if
missing or corrupt. It refuses to silently mint a replacement runtime identity.
Force replaces the fixed runtime's daemon/profile PATs, preserves native daemon,
runtime, workspace and agent IDs and user preferences, verifies reconnection,
retires explicitly replaced PATs and checkpoints the result. It never wipes the
database, changes S3 keys or the backup encryption password, or overrides pending
recovery. Failures retain progress and state for retry. A completed mutation with
an interrupted checkpoint retries only checkpoint publication; it does not replay
auth writes or automatically resume restored task dispatch.
