# Personal Hermes specification

What the personal Hermes platform must do. The
[architecture](ARCH.md) describes how these requirements are met, the
[implementation status](personal-hermes/implementation.md) lists the evidence, and
[`TESTS.md`](../services/hermes-manager/TESTS.md) maps every requirement and
acceptance row to its tests.

## Scope

A Helm-installed Kubernetes application that gives selected people in an
SSO realm their own Hermes agent:

- **Explicit.** An administrator decides who gets a Hermes instance, which
  template it uses and which terminal backends and storage it may use.
- **Private and persistent.** Every instance has its own namespace, storage,
  native sessions and credentials. It keeps running without browser traffic,
  so native cron works.
- **One address.** Everyone uses the same Hermes URL; the verified Keycloak
  identity selects the instance.
- **Terminals on demand.** Hermes runs shell commands and file access in
  terminal backend containers over SSH. The manager creates them only for
  backends the instance is allowed to use, with only the storage the
  administrator assigned.
- **Administered centrally.** Administrators use a dashboard on the manager's
  versioned API. Admin settings in the instance configuration are enforced
  over the user's own Hermes settings, for example the LLM provider.
- **Durable and crash-safe.** All manager state is in PostgreSQL.

Not in scope:

- coordinated checkpoint orchestration (backups stay deployment-owned; see
  [Backup and restore](#backup-and-restore));
- SMB storage;
- non-empty terminal service egress;
- worker prewarming;
- board integration;
- team instances.

## Terminology

| Term | Meaning |
| --- | --- |
| Identity | Exact OIDC issuer and verified subject. Email and display names are labels, never keys |
| Administrator | Identity with an active PostgreSQL administrator grant |
| Assignment | The administrator's decision for one identity: access state, Hermes template version, volumes, allowed terminal backends and admin setting overrides |
| Instance | The commissioned result of an assignment: a stable ID, a private namespace, storage, credentials and one Hermes Deployment |
| Hermes template | Versioned instance definition: image digest, resources, private home size, default volumes and mounts, admin settings and admin secret references |
| Admin settings | Hermes configuration (`adminConfig`) enforced over the user's own settings |
| Admin secret | Write-only secret value, such as an LLM API key or registry credential, referenced by templates or assignments |
| Terminal backend | Versioned SSH worker definition: image digest, resources, idle and lifetime limits and mount slots. Hermes requests one by ID (`runtime_id` on the wire) |
| Storage source | Installation-approved place where volumes may live: a storage class, a local node path root or an NFS export root |
| Volume | Named, owner-bound storage of an instance, from one storage source |
| Mount | A volume mounted at a path in the Hermes container or in a terminal backend |
| Environment | One terminal backend Pod incarnation for an instance, runtime and session or profile |
| Lease | Time-bounded permission for Hermes to use an environment |
| Gateway | The Rust reverse proxy that authenticates people and forwards their traffic to their instance |

## Requirements

| ID | Requirement |
| --- | --- |
| R1 | Keycloak authenticates identities. PostgreSQL in the manager is the only source of assignments, templates, terminal backends, volumes, admin secrets and administrator grants. |
| R2 | Only an explicit administrator command enables an assignment. Login, group membership or directory presence never enroll anyone. |
| R3 | Enabling an assignment converges to exactly one instance for that identity, including under concurrent commands and restarts. |
| R4 | Every instance has independent storage, native session keys, model authentication and execution credentials. A new instance inherits nothing from another owner. |
| R5 | One hostname routes by verified `(issuer, subject)`. A missing, disabled, unknown or foreign assignment is denied, with no fallback to any other backend. |
| R6 | Native Hermes chat, settings, history, uploads, HTTP mutations and WebSockets work through the gateway. Native authentication stays enabled and verifies the exact owner. |
| R7 | The dashboard uses only the administration API. The API without the dashboard supports every administration workflow. |
| R8 | Reconciliation never overwrites the user's settings, profiles, memory, sessions or files, and never reseeds an initialized home. |
| R9 | Suspension or revocation denies new access within 5 s, closes open connections within 10 s and stops the instance's terminal work within the drain bound. All volumes and credentials are retained until a separate purge decision. |
| R10 | Admin settings take precedence over the user's settings. Settings an administrator does not set stay editable by the user. Platform-enforced settings cannot be overridden by anyone. |
| R11 | An admin secret value is delivered only to the instances whose template or assignment references it, and is never returned by any API or shown in the dashboard after it is written. |
| R12 | An instance can use only the terminal backends its assignment allows. A user can choose any allowed backend per Hermes profile; an unknown or disallowed choice fails without fallback. Image, resources and mounts come only from manager records. |
| R13 | Volumes can only come from installation-approved storage sources. Volume paths of different instances never overlap. Users cannot add, widen or redirect mounts. The private Hermes home is never mounted in a terminal backend. |
| R14 | A volume mounted in both Hermes and a terminal backend has the same mount path and the same data identity (UID/GID) in both, so files and paths agree. |
| R15 | Only the manager controller holds Kubernetes credentials. Its rights are limited to owned namespaces and approved storage by RBAC and static admission policies that the chart installs and the controller cannot change. |
| R16 | Each enabled instance runs one Hermes replica continuously. Terminal backends start on demand and have independent lifetimes. |
| R17 | Every acknowledged administration change is durable before acknowledgement. After an abrupt process kill at any point, the system converges without duplicate instances, regenerated credentials, duplicate worker generations or lease grants, revived leases or deletion of anything it did not observe. |
| R18 | Credentials and secret values never appear in API responses, audit records, logs, errors, images, chart values or build contexts. |
| R19 | The installation can be restored from deployment-owned backups of PostgreSQL and the data under its storage sources, on the same or a new cluster, without adopting resources it did not create. |

## Administration model

Administrators configure:

| Object | Contents |
| --- | --- |
| Hermes template version | Image digest; CPU and memory; private home size and storage source; default volumes and their mount paths; `adminConfig`; admin secret references |
| Terminal backend version | Image digest; `managed-ssh-v2` contract; UID/GID; CPU, memory and limits; idle and lifetime bounds; mount slots (volume name, path, read-only) |
| Admin secret | Name, kind (`env`, `file` or `registry`), value (write-only) |
| Assignment | Identity; access; template version; volumes (source plus path, or size); allowed terminal backend versions and default; terminal lifetime mode; optional `adminConfig` overrides and secret references |
| Administrator grant | Identity; whether it can manage grants |

Storage sources are installation configuration (Helm values), not dashboard
records. They decide what the controller may ever mount, so they are reviewed
with the release configuration and also bound into the admission policies. The
dashboard shows them read-only.

For example, an administrator gives Bob:

- the template `personal-default`, version 3;
- a 20 GiB `workspace` volume on the local NVMe storage class, mounted at
  `/workspace` in Hermes and in every terminal backend;
- his existing network home `homes/bob` on the approved NFS export, mounted
  read-write at `/home/worker/home`, in the `python-tools` backend only;
- the terminal backends `python-tools` (the default) and `gpu-tools`.

Bob can change his own Hermes settings and choose either backend per profile.
He cannot mount another folder, use another image or override admin
settings.

Template versions and terminal backend versions are immutable once
published. An assignment pins exact versions. Changing them is an explicit,
audited assignment change; changing many assignments is one bulk command
that reports the result for each assignment.

Every mutation:

- authorizes the actor against a current PostgreSQL grant;
- requires an `Idempotency-Key`;
- uses `If-Match` for updates;
- writes an audit event in the same transaction.

Browser mutations also check `Origin` and a CSRF token.

The dashboard never exposes model keys, native tokens, chat history, private
files, raw Pod specifications or an impersonation function. The optional
Keycloak directory lookup is read-only.

## User experience

1. A user opens the common Hermes URL. The gateway signs them in with
   Keycloak.
2. With an enabled, ready instance, the gateway forwards to it. Native Hermes
   performs its own OIDC login, which reuses the SSO session, and verifies that
   the user is its exact owner.
3. Without an assignment, or with access suspended or revoked, the user sees a
   denial page (`403`). While the instance is being commissioned, they see an
   owner-only status page (`202`). When the manager is unavailable they see
   `503`. No path leads to another backend.
4. In the native dashboard, the user selects a terminal backend per profile
   (`terminal.runtime_id`). An empty selection means the assignment's default.
5. Logging out ends the platform session and expires the native cookies,
   even if the instance is unreachable.

## Lifecycle

Assignment access is `enabled`, `suspended` or `revoked`. Only administrator
commands change it. Suspension and revocation keep the identity, instance
ID, volumes and credentials. Re-enabling restarts the same instance on the
same data.

Instance phases are derived from observation:

| Phase | Meaning |
| --- | --- |
| `Provisioning` | Enabled; objects being created or updated; not yet ready |
| `Ready` | Desired generation observed; Deployment available; home initialization verified; native authentication gate verified |
| `Stopping` | Not enabled; terminal work draining and Hermes scaling down |
| `Retained` | Not enabled; nothing running; all data and credentials kept |
| `Blocked` | Needs an administrator, with a typed reason (for example storage unavailable, namespace conflict, invalid template, invalid initialization receipt) |

Conditions report storage, native initialization, native authentication and
terminal state separately. `Ready` does not claim the owner has completed
model-provider onboarding.

Template or admin setting changes restart Hermes with a Recreate rollout.
Hermes receives SIGTERM, with a 20 s application budget inside a 45 s Pod
grace period. Active agent runs can be interrupted. There is no automatic
rollback.

Permanent purge of an instance's data is not part of the platform. No data is
deleted through suspension, revocation, directory changes or Helm uninstall.

## Storage

### Storage sources

| Kind | Volumes it provides | Typical use |
| --- | --- | --- |
| `storageClass` | New, dynamically provisioned volume of a given size | Private Hermes home; new workspaces. With a local-path provisioner this is node NVMe |
| `localPath` | Existing directory under an approved root on an approved node | Pre-existing folders on the server's disks |
| `nfs` | Existing directory under an approved export root on an approved server | Network homes and shared project folders |

SMB (CSI) is the next storage kind.

Each source has an ID, a kind and its approved scope:

- `storageClass`: the class name;
- `localPath`: the node and the root path;
- `nfs`: the server and the export root.

The rules for volumes:

- **Paths.** A volume from a `localPath` or `nfs` source names a relative
  path inside the root. Absolute paths, `..`, empty segments and control
  characters are rejected. Two instances' volume paths on the same source never
  overlap (neither may be a prefix of the other).
- **Existing data.** Static volumes (`localPath`, `nfs`) refer to existing
  directories. The deployment creates them, owned by the installation's data
  identity. The manager never creates, chowns or deletes host directories. A
  missing or unreadable directory blocks that volume with a condition, and is
  never replaced by an empty directory.
- **Retention.** All volumes are retained on suspension and revocation.
  PersistentVolumes created for static volumes use the `Retain` reclaim
  policy.
- **Sizes.** Dynamic volumes cannot shrink, and cannot change storage class
  without a separate migration.

### Mounts

- **Private home.** The Hermes container always mounts its private home at
  `/opt/data`. It holds native state, credentials and settings, and is never
  mounted anywhere else.
- **Shared volumes.** The template mounts its volumes into Hermes, and each
  terminal backend version mounts its slots. A volume mounted in both must use
  the same path (R14). The assignment cannot be saved otherwise
  (`mount_path_mismatch`).
- **Read-only.** A mount is read-only if the template or backend slot says
  so, or if the assignment marks the volume read-only. Assignments can only
  restrict access.
- **Data identity.** All Hermes and terminal backend containers run as the
  installation's data UID/GID, by default `1000:1000`. A terminal backend
  version with a different UID/GID is rejected.
- **Required slots.** A backend slot that the assignment does not bind makes
  that backend unusable for the instance, unless the slot is optional. Missing
  storage never falls back to an empty or different volume.
- **Scheduling.** Volumes shared between Hermes and terminal backends must be
  mountable by both Pods at once. NFS and RWX classes work on any node.
  `localPath` and node-local storage classes pin all consumers to that node,
  which is the expected setup for a single-server deployment.

## Terminal backends

- **Immutable versions.** Terminal backend versions are immutable and pinned
  by image digest. Custom images must implement `managed-ssh-v2`: non-root
  user `worker` with the data UID/GID, SSH on port 2222, the provisioned
  authorized key and host key, shell and file transfer. Private registries use
  `registry` admin secrets. Pull or compatibility failures are reported, never
  replaced by the default image.
- **Lease protocol.** The `managed_execution` plugin requests a backend by
  `runtime_id` with the instance's execution credential. The manager resolves
  it within the current assignment and returns a lease and SSH endpoint only
  after the worker is ready and its SSH host key is verified. A pending
  worker returns `409`, so the plugin retries with the same request ID; a
  worker that failed to pull, start or meet the contract returns
  `422 backend_failed`; a dependency failure returns `503`.
- **No local fallback.** Denial gives an error, never local execution or a
  different backend.
- **Lifetime modes.** The assignment's terminal lifetime mode
  (`per_session`, the default; `per_profile_on_demand`;
  `per_profile_always_running`) is written into the managed configuration. An
  acquire request with a different mode is rejected. Idle and maximum
  lifetimes come from the backend version.
- **Changes.** Changes to an assignment's backends or volumes apply to new
  environments. Existing leases keep the environment they were granted;
  superseded environments drain when their leases end. A worker's image or
  mounts are never modified in place.
- **Removed backend.** Removing a backend from an assignment stops new
  grants for it and drains its environments.
- **Data on worker loss.** Worker loss, retirement or replacement never
  deletes volume data. Interrupted commands are not replayed.

## Configuration and admin secrets

Hermes configuration precedence, highest first:

1. **Platform-enforced:** `terminal.backend=managed_execution`, the
   image-owned plugin, protected connection paths and plugin disable lists.
2. Assignment `adminConfig` overrides.
3. Template `adminConfig`.
4. The user's own settings, on the private home.

Layers 1–3 form the read-only managed configuration. They are not copied into
the user's file, so removing an override exposes the user's retained value
again. `adminConfig` is limited to 8 KiB and must not contain secret values;
use admin secrets instead.

How admin secrets work:

- **Write-only storage.** They are stored encrypted. Only the controller can
  decrypt them.
- **Delivery.** A referenced secret is copied into the instance's namespace
  as an environment variable, a read-only file under
  `/run/hermes/admin-secrets/`, or an image pull secret.
- **Rotation.** Rotating a secret creates a new version, and referencing
  instances restart to receive it.
- **What it protects.** A secret delivered to an instance can be read by
  code running in that instance, which includes the agent acting for the
  owner. Admin secrets keep values out of templates, the API and the dashboard.
  They do not hide values from the owner's own agent. Use per-user or
  revocable keys, or a model gateway that issues per-user keys, for shared
  provider access.

## Crash recovery and persistence

The rules at each boundary:

- **Administration change.** Commit desired state, audit event and
  idempotency receipt together before acknowledging. A retry with the same key
  returns the recorded result; the same key with a different body returns
  `409`.
- **Reconciliation.** It is level-triggered from PostgreSQL desired state and
  observed Kubernetes state, and repeatable. A restart resumes by observation
  and needs no in-memory record.
- **Credential and material generation.** Generated once per instance and
  purpose, and committed before use. Never regenerated because a response was
  lost.
- **Home initialization.** Guarded by a signed receipt on the home volume.
  Never reseeded.
- **Deletion.** Uses UID and resource-version preconditions from a fresh
  observation. Completion is recorded only after absence is observed.
- **Leases.** Granted, renewed and released in single transactions, using
  database time. Expired and released leases are final.

Further rules:

- **Database unavailable.** No new grants, no administration writes, routing
  denied. There is never a fallback to fresh or local state.
- **Manager restarts.** Healthy Hermes and worker Pods are not restarted
  merely because the manager restarted.
- **PostgreSQL durability.** It must run with `fsync`, `full_page_writes` and
  `synchronous_commit` enabled; the manager refuses to start otherwise.
- **Startup checks.** Startup verifies schema version, installation identity,
  issuer and cluster identity before doing anything.

## Interfaces

| Interface | Caller | Contract |
| --- | --- | --- |
| Administration API `/api/v1/*`, browser auth `/auth/*`, dashboard | Administrators, external clients | `contracts/hermes-manager/openapi.yaml` |
| Route resolution `POST /internal/v1/routes/resolve` | Gateway (mTLS) | `contracts/hermes-manager/router.openapi.yaml` |
| Execution `/execution/v1/*` | `managed_execution` plugin (instance credential) | `contracts/hermes-manager/execution.openapi.yaml` |
| Gateway public `/_platform/*` and native logout | Browsers | `contracts/hermes-manager/gateway.openapi.yaml` |
| CLI | Operators, chart | `hermes-manager {api, controller, migrate, check, initialize, grant-administrator, rebind-cluster, generate-sealing-key}`, `hermes-gateway` |

The OpenAPI files are the authority for each interface.

Error semantics: `401` unauthenticated, `403` not permitted, `404` not
found, `409` idempotency or state conflict, `412` stale `If-Match`, `422`
invalid or unapproved input, `503` dependency unavailable or maintenance.
Responses never reveal another owner's data.

## Operational bounds

| Setting | Requirement |
| --- | --- |
| Route resolution | 2 s deadline; failure denies |
| Authorization freshness | HTTP decisions at most 5 s old; open WebSockets re-checked every 5 s and closed within 10 s of denial or failure |
| Identity evidence | Gateway refreshes provider state at most every 60 s; evidence older than 120 s is rejected |
| External identity revocation | Effective within 130 s, including open WebSockets |
| Upload size | 64 MiB unless a deployment lowers it |
| WebSocket idle | Up to 1 h with native heartbeats |
| Terminal drain | At most 5 min after access or policy change, then forced stop |
| Kubernetes and HTTP calls | Explicit deadlines, bounded concurrency, capped backoff |
| Administration request bodies | 16 KiB; `adminConfig` 8 KiB |

## Kubernetes

- **Namespaces.** One namespace per instance, named from the installation and
  the random instance ID, never from personal data.
- **Credentials.** The API, gateway, Hermes and terminal Pods have no
  Kubernetes token. The controller has namespace bootstrap rights restricted
  to its prefix, rights inside owned namespaces, and the right to create
  PersistentVolumes for approved static sources. It has no cluster-wide Secret
  access, impersonation or arbitrary role binding.
- **Admission.** Static admission policies installed by the chart restrict:
  - namespace names and labels;
  - who may write in owned namespaces;
  - Pod security: non-root, no privilege escalation, no host namespaces or
    hostPath, digest-pinned images, no token automount;
  - PersistentVolume sources: approved roots, nodes and servers only, with
    `claimRef` into an owned namespace and `Retain`.

  Their parameters come from the installation values.
- **Network.** Default-deny NetworkPolicies, with exactly these exceptions:
  - the gateway reaches the native Hermes port;
  - the controller reaches the native port and worker SSH for readiness
    probes;
  - Hermes reaches the manager execution API, its own workers' SSH and
    approved egress;
  - workers accept SSH only from their own Hermes and the controller.

  The deployment must verify that its CNI enforces NetworkPolicy.

## Hermes image and plugin

- **Image.** This repository builds the Hermes image from pinned upstream
  Hermes plus reviewed platform changes, and bundles the `managed_execution`
  plugin. It downloads no code at startup, and contains no personal profiles,
  prompts, memory or credentials. Templates select the image by digest.
- **Plugin.** The plugin is the only terminal provider in personal instances.
  Its connection settings come from the managed configuration. It
  authenticates every request with the instance's execution credential over
  verified TLS. It never forwards the user's browser cookie or token.
- **Execution credential.** The manager derives instance, owner and scope from
  the credential alone. Caller-supplied user, instance, namespace, Pod or
  container identifiers are never trusted. Disabled or revoked instances fail
  closed. Execution credentials grant nothing outside the execution API.
- **SSH host key.** The plugin pins the SSH host key returned with the lease
  and uses the instance's mounted SSH client key. No private key appears in an
  API response.

## Backup and restore

Backups are deployment-owned. The manager supplies what makes them
restorable:

- **What to back up.** PostgreSQL (dump or WAL archive) and the data under
  every storage source root (for example node NVMe paths and NFS exports), for
  example to S3. Encryption keys and Secrets referenced by the chart are
  backed up separately by the deployment.
- **Same cluster.** Restore PostgreSQL and data while the manager is stopped,
  then start it. The controller reconciles from the restored desired state.
- **New cluster.** After restoring, run `hermes-manager rebind-cluster`. It
  verifies that no owned namespaces exist in the new cluster, records the new
  cluster identity, clears namespace bindings and retires recorded terminal
  environments. The controller then recreates namespaces and volume bindings
  against the restored paths. Without this explicit command, a changed cluster
  identity blocks the controller.
- **Dynamic volumes.** To be restorable they need deterministic host paths,
  for example a local-path provisioner with a path pattern derived from
  namespace and claim names. The restore test proves static volumes; verify
  a provisioner's path pattern in your own restore drill. Otherwise, keep
  restorable data on static sources.
- **Never adopt.** The manager never adopts namespaces or volumes it has no
  record of. It reports them as orphans, in the administration API and the
  dashboard.

## Acceptance matrix

| Scenario | Required result |
| --- | --- |
| Two assigned identities, one URL | Distinct instances, settings, sessions, uploads and native owner checks |
| Same path, different verified subjects | Each reaches only its own instance; forged headers, email or unverified token claims change nothing |
| Valid but unassigned identity | Denial page; no commissioning side effect |
| Concurrent enable, retry and controller kill | One stable instance; credentials generated once |
| Email change or new subject with an old email | Ownership unchanged; the new subject gets nothing |
| Forged headers, foreign cookie or ticket, callback race | No cross-owner access |
| Native HTTP, upload and WebSocket through the gateway | Login, mutations, upload, ticketed WebSocket and logout work |
| Manager, database or Keycloak outage | Fail closed within the bounds; no fallback |
| Suspend during an open chat and during provisioning | New access denied within 5 s, connection closed within 10 s, terminal work drained, data retained |
| Re-enable after revoke | Same instance ID, same data and credentials |
| Admin setting and admin secret | Enforced over the user's setting; the user's other settings unchanged; secret delivered only to referencing instances and never returned by the API |
| Bob's example assignment | `/workspace` shows the same files in Hermes and both backends; `homes/bob` appears only in `python-tools`; another owner's path cannot be assigned to Bob (overlap rejected) |
| Missing static volume directory | Volume blocked with a condition; no empty-directory substitute |
| User requests a disallowed backend or edits profile YAML | Denied without fallback; no image or mount change |
| Custom backend image and private registry | Runs with the registry secret; pull or contract failure reported without fallback |
| Backend or volume change during an active lease | Active lease keeps its environment; new acquisitions use the new definition; superseded environment drains |
| Revoke during terminal acquisition | No new lease after revocation; pending worker drained |
| Hermes with no browser traffic | One replica keeps running; a cron job fires and acquires a terminal |
| Idle terminal cleanup | Worker retired; Hermes and its data untouched |
| Kill the manager at every injected fault point | Converges to the same state without duplicates or unobserved deletion |
| Execution credential used against another interface or instance | Denied |
| Restore PostgreSQL and data on a new cluster | After `rebind-cluster`, every instance returns with its data and the same IDs; unknown namespaces are reported, not adopted |
| UI build and dependency boundary | Static assets served by Rust; no credentials or live data in bundles; presentation code cannot reach the database or Kubernetes |
