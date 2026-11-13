# Agent platform application chart

A chart is the versioned installation package for this application of interacting
services, similar in scope to a Compose application. Helm renders Kubernetes
resources; Kubernetes runs and reconciles them. An installed Helm release is an
instance of this chart, separate from the GitHub Release that distributes it.

The chart packages Multica backend/frontend, Hermes, two same-origin HTTP
gateways and a runtime manager, together with configuration and isolation rules.
Agent-runtime Pods are created on demand by the manager rather than pre-created
by Helm. Browser/search/memory and further agents remain optional and disabled.
Their default images are RabbitMQ 4.4, Redis 8.8 (Redis Ltd.'s RSALv2/SSPLv1/AGPLv3
licensing) and a current SearXNG build. RabbitMQ 4.4 cannot open a 3.13 data
directory directly: upgrade through 4.0 with all stable feature flags enabled,
or start from an empty queue volume.

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
- `hindsight`: optional external PostgreSQL URL/CA Secret references and LLM provider,
  model, endpoint and optional API-key Secret reference. Enable with
  `workloads.hindsight.enabled`; see [Hindsight](#hindsight).
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
backup transport/policy, bucket credentials, PVC backing and VPN ingress are infrastructure duties.
Application recovery mechanics and their `files/recovery.json` inventory ship
with the tools image; see the [recovery contract](../../docs/recovery.md).
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

`mode` selects the worker lifetime. The default, `per_profile_on_demand`, shares
one worker per profile across its conversations, starts it at first use and
removes it after the template's `on_demand_idle_seconds`. `per_session` gives each
conversation its own worker, removed after `session_idle_seconds`.
`per_profile_always_running` keeps the profile worker running until it has been
unused for `recycle.inactive_after_seconds`. Per-profile workers are rebuilt at
the template's daily `recycle.at` once their active leases finish. Multica task
invocations and other runs outside the profile workspace always use their own
`per_session` worker, whatever the profile selects, because a shared worker cannot
carry per-task mounts. Earlier releases defaulted to `per_session`.

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
runtime manager upgrades the previous SQLite schema in place at startup,
preserving environment identities, live leases and worker Pod names. Surviving
workers are reused after restart. Run only one manager process against its
`manager-state` database; an unsupported newer schema is refused.

## Runtime manager lifecycle

The manager reconciles observed worker Pods with SQLite records through one
lifecycle planner. Starting Pods never expire under the idle timers. Image and
container failures are recorded and returned with their reason instead of being
retried on every acquire. Changes to templates or images and the daily recycle
wait for live leases, up to a bounded drain deadline.

All timers below are seconds under `runtimeManager.config`:

| Setting | Default | Meaning |
| --- | --- | --- |
| `lease_ttl_seconds` | 120 | Lifetime of an unrenewed lease; minimum 30. |
| `maintenance_interval_seconds` | 10 | Reconciliation interval. |
| `start_timeout_seconds` | 300 | Maximum time for a requested Pod to become Ready. |
| `failure_backoff_seconds` | 60 | How long a failed attempt is reported before a fresh attempt is allowed. A changed selection or template configuration ends it early. |
| `drain_deadline_seconds` | 3600 | Maximum wait for live work before replacing a retiring worker. Remaining leases are revoked at the deadline. |
| `max_lease_seconds` | 86400 | Maximum lease age even with heartbeats or acquire replays; must be at least the lease TTL. |
| `request_memory_seconds` | 86400 | Retention after a lease ends for its acquire request ID. Replaying an ended request returns `lease_not_live`. |
| `orphan_grace_seconds` | 300 | Minimum age of an unknown managed Pod before reporting or deletion. |
| `orphan_scan_seconds` | 60 | Orphan scan interval, checked on maintenance ticks. |

`runtimeManager.config.orphan_policy` defaults to `report`. The authenticated
`GET /v1/orphans` endpoint lists managed Pods no environment record names, with
`name`, `created` and `first_seen` timestamps. Each is logged once while present;
none is adopted. Setting the policy to `delete` also removes them after the
grace period. This helps recover quota after restoring an empty manager database.
The namespace-scoped manager Role permits `get`, `list`, `create` and `delete`
on Pods; it has no cluster-wide authority.

The [API contract](../../contracts/runtime-manager/execution.openapi.yaml) defines
responses and stable error codes. Pending and draining responses carry
`Retry-After: 1`. The Hermes plugin retries temporary 409 and 503 responses until
`acquire_timeout_seconds`, surfaces permanent worker failures with the manager's
message, and stops using a worker immediately when a heartbeat says its lease
ended. Heartbeats and releases do not wait for Kubernetes calls. Profiles still
assert their own identity and may select any configured template or image.

### Worker account and optional host key

The generic worker binds its `worker` account at startup to the container's UID
and GID using nss_wrapper. Keep its entrypoint and writable `/tmp`, `/home/worker`
and `/workspace` mounts when extending it. SSH sessions use that account, including
when `runtimeManager.uid` and `gid` differ from the image's build-time defaults.

To pin the SSH server identity, create an ed25519 host-key Secret in both the
application and execution namespaces (or one synchronized Secret per namespace),
then set:

```yaml
sandbox:
  hostKey:
    secretName: worker-host-key
    privateKey: ssh_host_ed25519_key
    publicKey: ssh_host_ed25519_key.pub
```

The key names above are the defaults. Worker templates mount only the private
key, read-only, at `/etc/ssh/hostkeys/ssh_host_ed25519_key`; the entrypoint copies
it into a private mode-0600 file. Hermes receives only the public key at
`/opt/ssh-host/host-key.pub`. The plugin uses a private known-hosts file and
`StrictHostKeyChecking=yes`, rejecting every other host key. All workers sharing
the private key share the same SSH server identity.

The option is disabled by default. With no host-key Secret the plugin trusts the
first key for each managed generation and rejects later changes. Enabling the
option requires a worker built with the new entrypoint: older images still work
with the manager but cannot present the provisioned key, so the plugin rejects
them when pinning is enabled. The chart references Secrets; it does not generate
or publish private keys. Deployment configuration and Secret creation belong in
the private deployment repository.

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

## Hindsight

Enable `workloads.hindsight.enabled: true` to run the supported, digest-pinned
Hindsight API with its in-process worker. Overlay
[`examples/hindsight/values.yaml`](../../examples/hindsight/values.yaml) on the
lightweight example. Choose the LLM provider/model and existing Secret names;
the example's model name is a placeholder. No owned image or upstream subchart is
needed. `shared-postgres` remains unsupported, disabled scaffolding.

Prepare a dedicated PostgreSQL database and login, separate from Multica's.
An administrator must create `vector`, `pg_trgm` and `btree_gin` in `public` before
Hindsight's non-superuser database owner runs migrations. For example, using `psql`
as the database administrator (choose the password at the prompt):

```sql
CREATE ROLE hindsight LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;
\password hindsight
CREATE DATABASE hindsight OWNER hindsight;
\connect hindsight
CREATE EXTENSION vector WITH SCHEMA public;
CREATE EXTENSION pg_trgm WITH SCHEMA public;
CREATE EXTENSION btree_gin WITH SCHEMA public;
```

Store the complete PostgreSQL URL in a mode-0600 file outside this checkout, then
create the selected Secret without placing the URL in command arguments:

```sh
kubectl -n "$NAMESPACE" create secret generic hindsight-database \
  --from-file=url=/secure/path/hindsight-url
kubectl -n "$NAMESPACE" create secret generic hindsight-postgres-ca \
  --from-file=ca.crt=/secure/path/postgres-ca.crt
kubectl -n "$NAMESPACE" create secret generic hindsight-llm \
  --from-file=api-key=/secure/path/llm-api-key
helm template agents charts/agent-platform \
  -f examples/lightweight/values.yaml -f examples/hindsight/values.yaml
```

Use a `postgres://` or `postgresql://` URL. For verified TLS, append
`?sslmode=verify-full&sslrootcert=/etc/postgres/ca.crt` and set
`hindsight.database.caSecret`; this works for migrations and runtime connections.
The server certificate must cover the database hostname in the URL. The container
rejects an empty or non-PostgreSQL URL without printing it. For a provider that needs
no key, leave `hindsight.llm.apiKeySecretRef.name` empty. An optional
`hindsight.llm.baseUrl` must contain no credential, query or fragment. Credentials
belong only in existing Secrets. The database's NetworkPolicy, if present, must
admit Pods labelled `app.kubernetes.io/name: hindsight` from the application namespace.

A **fresh root home** automatically selects Hindsight in Local External mode with
bank `default`. An explicit `hermes.config.memory.provider` (including `""`)
preserves that choice and suppresses connection seeding. The chart gives Hermes
`HINDSIGHT_MODE` and `HINDSIGHT_API_URL` as internal fallbacks but never
`HINDSIGHT_BANK_ID`, which would override every home's own bank; workload `env`
cannot set any of the three. Named profiles never inherit the root provider
automatically. Existing homes and every named profile
are configured in Hermes; Helm upgrades preserve their files and credentials.

**Dashboard activation:** select the intended profile and choose Hindsight as the
memory provider. On first activation in a home, Hermes installs the plugin's
Python dependencies (`hindsight-client`, `hindsight-embed`) into that home's own
environment, downloading what the image's package cache lacks from PyPI. This
needs HTTPS egress from the Hermes Pod to `pypi.org` and `files.pythonhosted.org`.
The first activation on a volume downloads about 80 MB, takes about 30 seconds and
adds about 770 MB under the root home's `installs/` and `cache/`. Named profiles
reuse that store and cache: each further profile builds its own environment there
in about 10 seconds, downloads little or nothing and adds about 200 MB. Hermes
then asks for a gateway restart. Save these fields in this order:

1. Set Mode to **Local External**. The panel initially offers Cloud.
2. Set API URL to `http://hindsight:8888`. The panel initially offers a public URL.
3. Set Bank ID to the intended bank. The panel default is `hermes`; the root home
   uses `default`. Banks are created on first retain.
4. Leave API key empty.

Saving submits the panel's full field set for the selected mode, including
defaults, to `hindsight/config.json`; the empty API key is not stored.
Only the selected memory provider changes in `config.yaml`.

**Saving the dashboard's default Cloud mode and public URL sends conversations to
the public service.** A saved JSON URL overrides the Pod's internal URL. Check both
Mode and API URL whenever activating an existing home. Apply changes to a new
session, or restart the Hermes gateway for existing sessions, because each session
keeps its initialized memory provider. The image tests exercise a newly initialized
provider after each change.

For scripted activation, the following native command writes connection settings
only if absent and changes the selected provider. If `hindsight/config.json`
already exists, check its `mode`, `api_url` and bank first. An existing Cloud/public
configuration is preserved by this command and can send conversations externally.

<!-- hindsight-activate -->
```sh
kubectl -n "$NAMESPACE" exec deploy/hermes -c hermes -- \
  /command/s6-setuidgid hermes sh -ceu '
    export HOME=/opt/data HERMES_HOME=/opt/data
    umask 077
    mkdir -p "$HERMES_HOME/hindsight"
    [ -e "$HERMES_HOME/hindsight/config.json" ] ||
      printf "%s\n" "{\"mode\":\"local_external\",\"api_url\":\"http://hindsight:8888\",\"bank_id\":\"default\"}" \
        > "$HERMES_HOME/hindsight/config.json"
    /opt/hermes/.venv/bin/hermes config set memory.provider hindsight
  '
```

For a named profile, use `HERMES_HOME=/opt/data/profiles/<name>` and replace the
bank in the JSON with your chosen ID. Set `HERMES_HOME` explicitly; `-p` did not
update the intended profile file in the qualification probe. New sessions or a
gateway restart apply these changes too.

**Profiles created later:** restart Hermes to sync the installed plugin into the
new home, then activate it with the dashboard or the commands above. A profile
created with `--clone` copies the provider selection but neither `hindsight/` nor
`plugins/`. Until configured and restarted, it may use the root's default bank or
trigger a plugin catalog install. Configure the bank explicitly.

**Disable:** first run this command in every active home (change `HERMES_HOME` for
named profiles), then set `workloads.hindsight.enabled: false`. Start a new session
or restart the gateway to deselect the initialized provider.

<!-- hindsight-deactivate -->
```sh
kubectl -n "$NAMESPACE" exec deploy/hermes -c hermes -- \
  /command/s6-setuidgid hermes sh -ceu '
    export HOME=/opt/data HERMES_HOME=/opt/data
    /opt/hermes/.venv/bin/hermes config set memory.provider ""
  '
```

Disabling leaves banks, Secrets and retained claims intact. The old
`hindsight-login` PVC is no longer mounted or created by default; an existing PVC
remains because of its retention annotation. Delete it manually only after
confirming it is unneeded. No data PVC is needed. The default uses the image's
preloaded models without a cache mount and sets `HF_HUB_OFFLINE=1`, so it needs
no access to Hugging Face. An operator's `HF_HUB_OFFLINE` environment entry is
preserved without a duplicate. Set `persistence.hindsight-cache.enabled`
or `existingClaim` to mount the optional retained cache at
`/home/hindsight/.cache`. This mount hides the image's preloaded models. A cache
claim needs access to Hugging Face for the first model download unless it is
already populated. The chart does not add `HF_HUB_OFFLINE` when a cache claim is
selected; caches are replaceable.

**Access and memory behaviour:** the API has no authentication key. Only the Hermes
Pod has ingress under the chart's NetworkPolicy. Rendering fails if Hindsight is
enabled with `networkPolicy.enabled: false`, because those policies are its only
access control. This depends on a CNI that enforces policies. Any process in that
Pod can read every bank; bank IDs are logical partitions, not authorization per
person. Sandbox workers, the manager and other
workloads have no access. Multica tasks unset the Hindsight connection environment
and use derived homes with the external provider disabled. Hermes's built-in file
memory stays on. The plugin retains every turn and recalls observations by default;
cron and subagent contexts skip writes. Offline image tests use a mock LLM and
explicit recall types; they do not prove real-model memory quality.

**Operation:** the numeric UID/GID is 1000, the stable worker ID is `hindsight`, and
updates use Recreate. Local qualification measured cold startup at 20.42 s, idle
cgroup memory at 752 MiB and retain peak at 966 MiB (process RSS peak 1188 MiB).
Defaults request 500m CPU / 1536Mi memory with a 4Gi limit; size for your workload.
Startup probes allow 300 s, matching the wrapper's default startup deadline.
Readiness uses `/health`, which fails when PostgreSQL is unavailable;
`/health/live` is the unauthenticated startup endpoint. There is no liveness probe.
Migration failure prevents serving requests. Unreachable LLM verification warns
and permits startup; retain retries wait for recovery. Accepted queued retains
survive restart with the stable worker ID (24-document qualification recovered in
19.79 s). API/database calls have timeouts; default Hermes recall waits up to 3 s,
and the memory manager caps a synchronous override at 8 s. A failed or missing bank
lookup does not prevent ordinary chat. See the
[qualification record](../../docs/hindsight/specification.md#qualified-upstream).

**Backup and restore:** durable state is the Hindsight database, native Hermes homes
and Secrets. Enabling Hindsight enrolls it in no backup schedule. Deployment tooling
owns backup coverage and must honor the installation operation Lease. Stop
`hindsight` and `hermes` before restoring the database, and restore native settings
so they keep their original bank IDs. If a deployment wants `hindsight` included in
`bootstrap.writers`, add it there explicitly only when its Deployment is enabled.

## Release and upgrade

The first upgrade to a chart containing this Hindsight integration restarts all
six core Deployments, even with Hindsight disabled, because `checksum/application`
changes.

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
  `checkpoint` runs after every enrollment and publishes a verified encrypted
  checkpoint including the native database, home and both enrollment Secrets.
  `before` and `after` run only for an explicit `--force` repair, which changes
  existing state: before mutation, create a verified checkpoint, fence dispatch,
  force-stop all native writers; after successful reconnection, restore
  dispatch and writers. First enrollment of a fresh installation changes no live
  state, so it neither stops nor scales writers: the Hermes daemon starts once its
  atomically written login file exists. Retries of a first enrollment keep that
  path; journals written by releases before 0.6.9 keep the cold cycle. Commands
  must fail on any missing durability prerequisite. The Job captures private
  command output and reports only the failed step, exception type and hook exit
  code.
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

The API 1 recovery inventory currently excludes Hindsight. A consumer using this
contract must keep Hindsight disabled until its database joins the inventory.

The backend maintenance path is derived from the recovery descriptor and its
uploads mount. A conflicting explicit `multica.maintenanceFile` is rejected.
