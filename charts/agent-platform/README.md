# Agent platform application chart

A chart is the versioned installation package for this application of interacting
services, similar in scope to a Compose application. Helm renders Kubernetes
resources; Kubernetes runs and reconciles them. An installed Helm release is an
instance of this chart, separate from the GitHub Release that distributes it.

The chart packages Multica backend/frontend, Hermes, two same-origin HTTP
gateways and a runtime manager, together with configuration and isolation rules.
Agent-runtime Pods are created on demand by the manager rather than pre-created
by Helm. Browser/search/memory and further agents remain optional and disabled.

## Native configuration generation

Administrators supply ordinary Helm values. `templates/_runtime.tpl` derives both:

1. `runtime-manager-config`, a ConfigMap whose `config.yaml` is the native manager
   configuration: clients and token-file references, named terminal
   templates, lease/idle/recycle settings, runtime images, UID/GID, volumes and
   task/profile workspace mappings.
2. The admission allowlist used by `ValidatingAdmissionPolicy`: matching runtime
   roles, literal environment values, selected PVC/Secret
   mounts and bounded per-task path patterns. The policy rejects
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
      hermes:
        session_idle_seconds: 60
        workdir_scope:
          kubernetes_claim: my-task-data
sandbox:
  namespace: agent-execution
```

Helm merges this with defaults, writes the client credential path into native YAML,
mounts that selected Secret key into the manager, and admits only the same task
claim and non-root identity. The default runtime image comes from the chart's
`images.agent-runtime` digest. An explicit template `image` overrides that fallback.
Hermes may override the image and select any configured template by name.
There are no image allowlists or per-profile template permissions. Templates
reject duplicated roles/volumes, unsupported volume
sources, writable/unselected Secret mounts and unsafe subpaths. The values schema
rejects manually supplied `sandbox.roles`, preventing a second competing policy.
Changes to generated configuration update Pod checksums and roll the affected
application. No downloaded Python or secondary configuration bundle is needed.

`templates/_application.tpl` owns internal service URLs and native application
environment variables. Gateway nginx configs are packaged files. Database/mail,
public URL, owner and resource/storage choices are values. Model configuration
and login belong to native Hermes state.

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
- `hermes`: initial native root/profile configs, dashboard authentication, username and concurrency.
  `dashboardAuth.mode` selects `basic` or `oidc`.
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
Postgres password/JWT secret and the Hermes dashboard/API keys; `multica-mail`
with configured mail keys; manager bearer and TLS Secrets;
`execution-controller-ssh` with its Hermes private key; and the sandbox's selected
SSH public-key Secret. Manager TLS clients select `tls.crt`
as their CA. The chart initializes missing native configs without a persona, model
login, identity or skills. Deployment-specific identities and skills can be seeded
separately, and Multica enrollment remains opt-in.

Hermes model/provider selection and model credentials belong to its native
persistent home. A fresh deployment has no model login; the administrator uses
the native dashboard or `hermes model` to configure it. There is no required
provider Secret. API-key providers can use native `.env` files or the generic
`workloads.hermes.env` and `workloads.hermes.secretEnv` interfaces. Workload lists replace the defaults, so
retain required application authentication entries when supplying a custom list.

Codex subscription OAuth uses Hermes's native login flow and writable `auth.json`,
not an API-key environment variable. Preserve the native home across restarts so
refresh tokens and model settings survive. An external OAuth credential file can
be imported through native Hermes tooling; read-only mounting it as the active
auth store prevents Hermes from persisting refreshed credentials. External
Secrets need their own backup policy; environment variables alone are not a
backup of native state.

The chart does not generate or seed credentials. Seed/export, database provisioning,
backups, bucket credentials, PVC backing and VPN ingress are infrastructure duties.
Mutable native settings live in retained storage. Helm supplies initial config
values and execution policy, while existing native config files are preserved. Claims and a
chart-created sandbox namespace are retained on uninstall; losing the node disk
still requires a separately designed backup/restore strategy.

The Hermes gateway supports the normal dashboard, including chat, native
authentication/session APIs, uploads and WebSocket connections. It forwards HTTP
methods to Hermes, which enforces authentication, ownership and origin checks.
Uploads have a 64 MiB gateway limit; streaming and WebSocket connections use a
one-hour upstream idle timeout. Multica remains available as another task entry
point. Native Hermes and runtime-manager settings govern task concurrency.

## Native profiles and terminal workers

Release 0.6.3 provides these interfaces. Earlier releases retain the older
personal-assistant execution preset.

The default chart has one native root Hermes configuration, no named profiles and
no personal-assistant preset. `hermes.config` supplies its initial native YAML.
`hermes.profiles.<name>` supplies named profiles, each merged over `hermes.config`
when its config file is first created. Named profiles receive their own execution
profile identity. Missing configs are initialized at startup; existing configs,
model credentials and user preferences are never overwritten by a Helm upgrade.
Change an existing native config through native Hermes administration. A terminal
image change takes effect when Hermes creates a new execution environment; active
leases finish before the manager replaces their worker generation.

A profile selects a named terminal template and optional image through the managed-execution plugin:

```yaml
terminal:
  backend: managed_execution
plugins:
  enabled: [managed_execution]
  entries:
    managed_execution:
      settings:
        template: hermes
        image: registry.example.org/team/terminal:1
        mode: per_profile_on_demand
```

For a new chart-managed profile, put this under
`hermes.profiles.<name>`. The plugin requests a worker from the manager and then
connects over SSH. It does not use the built-in `ssh` or `docker` backend, and
`terminal.docker_image` is not its image setting. Leaving the plugin's `image`
empty selects the deployment's fallback image. The Hermes native plugin API reads
these settings from the active profile, including task overlays. An explicit
image is persisted with the execution environment so manager restarts and worker
recycling keep that selection.

Managed SSH execution is an administrator policy, not a per-profile choice. The
image's managed Hermes layer (`/etc/hermes/config.yaml`) requires
`terminal.backend: managed_execution` and binds every profile, including the
per-profile terminal scope Hermes builds for routed dashboard and Multica turns.
A profile's own `terminal.backend` or `TERMINAL_ENV` cannot select local or
Docker execution. Since 0.6.4 the plugin is bundled with the image as a backend
plugin, so profiles created later in the dashboard receive it without a restart;
if it is unavailable, terminal calls fail instead of running in the controller.

Authenticated clients can use any configured template from any profile, including
new profiles created at runtime. `runtimeManager.config.default_template` defaults
to `hermes`; an omitted plugin template selects it. The chosen template supplies
its default image unless Hermes sends an explicit image override. There are no
profile-to-template permissions, image catalogs or image admission restrictions.
Templates continue to own mounts, resource limits and lifecycle defaults. Each
client token still binds its deployment identity and cannot access another
client's leases. Changed template/image selections wait for active leases to
finish before replacing an existing worker.

The release packages the Ubuntu-based `agent-runtime` with Multica CLI and the
restricted task checkout proxy, and pins it as the default template image. Specialized personal,
developer, browser and operator images are deployment responsibilities. Custom
images must implement the worker contract: non-root UID/GID matching
`runtimeManager.uid`/`gid` (default 1000), SSH on TCP 2222, user `worker`, authorized
keys at `/etc/ssh/authorized_keys/worker`, and a writable `/workspace`. Retain the
entrypoint when extending the generic image. Images needing other software can
be built independently and selected from native Hermes settings. The SSH server
accepts explicit environment forwarding; use profile `.env` values and
`terminal.env_passthrough` names for terminal credentials. Hermes reads a
profile's `env_passthrough` names once per gateway process, so a newly added
name takes effect after a gateway restart (available in the dashboard); changed
`.env` values apply immediately. Multica task variables (`MULTICA_TOKEN`,
`MULTICA_TASK_*` and related) are forwarded to task workers by the plugin for
every profile and need no `env_passthrough` entry.

### Persistent storage and namespace permissions

Templates define `kubernetes.volumes` and `kubernetes.volume_mounts`; these support
selected PVCs, Secret keys and emptyDir volumes, with mount paths, safe subpaths
and read-only flags. Each profile can select a different template and storage
layout. Helm lists replace defaults, so include all required SSH and workspace
volumes/mounts when replacing these lists. Persistent claims must already exist
in the configured execution namespace and be writable by the configured worker
identity where required. Normal Kubernetes access-mode and attachment constraints
apply when profiles share a claim.

The default execution template mounts an ephemeral workspace and the selected
SSH public key. It does not mount the controller's native config, credentials,
profile home, identity or skills. Add reviewed deployment mounts explicitly when
needed. Task execution can mount its single validated Multica workdir through
`workdir_scope`; it cannot name an arbitrary claim or mount its parent directory.

A template's `profile_workspace` maps ordinary profile sessions to a persistent
claim. A `{profile}` token in `kubernetes_subpath` and `controller_root` gives
every Hermes profile its own subdirectory, for example `kubernetes_subpath:
"{profile}"` with `controller_root: /opt/data/workspaces/{profile}`. The plugin
creates a missing profile directory on the controller before requesting a
worker, and admission accepts only valid profile names. A literal subpath shares
one directory between all profiles using the template.

The chart-owned execution namespace defaults to `<application namespace>-execution`,
for example `agents-execution` when the application namespace is `agents`. Long
namespace names receive a bounded suffix with a hash. `sandbox.namespace` can
explicitly select another dedicated namespace; using the application namespace
itself is rejected because its admission, quota and network policies serve a
different purpose. The manager ServiceAccount lives in the application
namespace, with a RoleBinding granting only Pod `get`, `create` and `delete` in
that one execution namespace. It has no cluster-wide admin, PVC management or
Secret-reading permission. Worker PVC references resolve only in their own
namespace. Admission further restricts them to the Helm-approved claims, mount
paths and subpaths. Hermes requests cannot supply a namespace, PVC name or
container mount specification. Infrastructure administrators define templates
through Helm; trusted native Hermes config selects their names and worker images.

### Private registry credentials

Registry credentials are objects keyed by registry address, because each login
belongs to one registry. Passwords may be registry access tokens:

```yaml
sandbox:
  registryCredentials:
    ghcr.io:
      username: robot-account
      password: <read-packages-token>
    registry.example.org:5000:
      username: another-account
      password: <registry-token>
```

The chart creates `<release>-terminal-registry` in the execution namespace, with
all configured logins in a standard Docker config Secret. Values are sensitive;
use protected Helm inputs or reference externally managed Secrets instead. Both
sources combine into the pull-Secret list on every manager-created worker.

Reference existing Kubernetes registry Secrets in the execution namespace:

```yaml
sandbox:
  imagePullSecrets:
  - name: private-registry
```

Use Secrets of type `kubernetes.io/dockerconfigjson` (or the supported legacy
`kubernetes.io/dockercfg` type). The chart puts these references on the worker
ServiceAccount and in manager configuration; the manager includes them on worker
Pods. Kubernetes pulls private images using those credentials. Credentials are
never sent through Hermes, read by the manager, or mounted into worker containers.
Admission permits only the configured pull-Secret references. The top-level
`imagePullSecrets` separately configures pulls for the application workloads.

The [terminal-backends overlay](../../examples/terminal-backends/values.yaml)
shows two native profiles, different images, separate PVC layouts and a registry
Secret. Its registry references and claims are examples to replace before use.
Apply it together with the lightweight example and your deployment values. The
execution manager's SQLite schema targets fresh development installations; this
change includes no migration of earlier manager state.

## Hermes dashboard OIDC

Version 0.6.0 adds `hermes.dashboardAuth`. Earlier charts do not have this
interface. Basic authentication remains the default for existing users.
Select OIDC through chart values without replacing workload environment lists:

```yaml
hermes:
  dashboardAuth:
    mode: oidc
    oidc:
      issuer: https://sso.example.org/realms/example
      clientId: hermes
      publicUrl: https://hermes.example.org
      scopes: openid email profile
```

Configure an OIDC client with authorization code flow and PKCE S256. Register the
exact redirect URI `https://hermes.example.org/auth/callback` and browser origin
`https://hermes.example.org`. The example uses a public client; no client secret
is needed. A confidential client can additionally select an existing Secret:

```yaml
hermes:
  dashboardAuth:
    oidc:
      clientSecretRef:
        name: hermes-oidc
        key: client-secret
```

This fragment extends the OIDC settings above. The chart never creates or embeds
that Secret. Restart Hermes after rotating a referenced client Secret. Configure
client access at the identity provider to admit only the owner and trusted
administrators of this shared personal instance. Successful OIDC login does not
provision another personal instance or a Multica account.

OIDC mode removes every `HERMES_DASHBOARD_BASIC_AUTH_*` environment and Secret
reference, including the default password and cookie-secret references. Existing
`DASHBOARD_PASSWORD` and `DASHBOARD_SECRET` keys are therefore unnecessary for OIDC;
the independent `API_SERVER_KEY` and provider credentials remain required. Native
Hermes handles the callback, session, logout and WebSocket authentication. Its
retained home stores its session state.

Helm validates complete HTTPS issuer/public URL settings, client ID, `openid`
scope and Secret references before installation. Direct `HERMES_DASHBOARD_OIDC_*`
and `HERMES_DASHBOARD_PUBLIC_URL` workload overrides are rejected in favor of
this interface. The issuer's discovery, token and JWKS URLs must be reachable
from the Hermes Pod with valid TLS and the same issuer as browser requests.
Configure ingress, DNS and identity-provider entitlements in the deployment.
No Keycloak installation or deployment-specific networking is generated here.

The [OIDC example](../../examples/oidc/values.yaml) is an overlay for the
lightweight example. Upgrade by setting the OIDC values and applying the release;
the resulting environment changes restart Hermes. Returning to `mode: basic`
requires the configured Basic-auth Secret keys again.

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
and lifecycle command contract below. This enrolls the root Hermes controller by default. Set `bootstrap.profile` to a
deployment-defined named profile to enroll that profile instead; bootstrap does
not create its native config. `bootstrap.profileSecret` selects the worker login
Secret in the execution namespace (default `hermes-task-login`). Terminal Pods receive task
context; they do not register a second daemon. This is separate from future
multi-user runtime provisioning.

`bootstrap.createAgent: false` registers only the Hermes runtime; workspace
members then create Multica agents and choose Hermes profiles themselves. The
default `true` creates one agent from `bootstrap.agent`. `bootstrap.runtimeVisibility:
public` lets every member of the bootstrap workspace bind agents to that runtime
(default `private`, owner only). Visibility belongs to each workspace's runtime
entry: the daemon also registers in workspaces its owner creates later, as
`private`, and the owner changes visibility there in Multica's runtime settings.

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
