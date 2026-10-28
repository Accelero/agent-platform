# Hermes instances, storage and admin settings

The requirements are in the [specification](../SPEC.md), and the
mechanisms are in the [architecture](../ARCH.md).

This guide covers what an administrator and an operator configure for a
personal Hermes instance:

- Hermes templates;
- admin settings and admin secrets;
- storage sources, volumes and mounts;
- the lifecycle;
- backups.

Terminal backends have their own [guide](terminal-backends.md).

Every Hermes instance requires the manager's read-only owner policy at
`/run/hermes/owner/context.json` and terminal policy at
`/run/hermes/execution-ca/config.yaml`. Missing or invalid policies prevent
authentication and terminal execution. Environment variables and user settings
cannot substitute for these policies.

## Installation inputs (operator)

The chart's `hermesManager` values carry everything that bounds what the
controller may do. The chart renders these values into the controller
configuration and into the static admission policies themselves:

```yaml
hermesManager:
  installation:
    id: 7d6c0e1a-0000-4000-8000-000000000001
    issuer: https://sso.example.org/realms/agents
  dataIdentity: { uid: 1000, gid: 1000 }
  storageSources:
    - id: nvme                       # dynamic volumes on the server's NVMe
      storageClass: { name: local-nvme }
    - id: host-data                  # existing folders on the server
      localPath: { node: server-1, root: /srv/hermes-data }
    - id: nas                        # existing network homes
      nfs: { server: nas.internal, root: /export/homes }
```

- **Storage sources** cannot be added from the dashboard; changing them is a
  chart upgrade. The dashboard lists them read-only.
- **`dataIdentity`** is the UID/GID that every Hermes and terminal backend
  container runs as. The repository's Hermes and `agent-runtime` images use
  `1000:1000`.
- **Existing folders.** For `localPath` and `nfs` sources, the deployment
  creates each folder an administrator will assign, owned by the data
  identity. The manager never creates, changes ownership of or deletes host
  directories.
- **Node NVMe as dynamic storage.** Install a local-path provisioner whose
  storage class points at the NVMe root. Give it a deterministic path pattern
  (for example `{{ .PVC.Namespace }}/{{ .PVC.Name }}`) so the data can be
  found and restored from backups.

Before installing the chart, generate the admin-secret sealing key pair once
and keep both files in the deployment's secret store (and its backups; without
the private key, stored admin secrets and instance credentials cannot be
opened):

```bash
hermes-manager generate-sealing-key --private-key-file sealing.key --public-key-file sealing.pub
```

The private key goes into the controller's Secret (`secret-sealing.key`), the
public key into the API's (`secret-sealing.pub`). The command never overwrites
a file.

After installing the chart, run the three operator steps as one-shot Jobs,
each enabled for one `helm upgrade` and then disabled (see the
[chart guide](../../charts/agent-platform/README.md#operator-steps)):

1. `operations.migrate` applies the schema with the schema owner's database
   URL;
2. `operations.initialize` records the installation and the cluster's identity
   (the UID of `kube-system`), as the controller's ServiceAccount, in an empty,
   migrated database;
3. `operations.grantAdministrator` grants the first administrator from the
   `identity.json` in its Secret, which holds the exact
   `{"issuer": …, "subject": …}`. Set `recover` to restore a grant when grants
   already exist. The command is audited and never prints the identity.

### Gateway and route resolution

The gateway and the manager's resolver authenticate each other with mutual
TLS. Issue the gateway a client certificate whose URI SAN is its service
identity, and name exactly that identity in the chart; a certificate from the
same CA with any other identity gets only denials. The gateway needs its own
Keycloak client, with the exact callback
`https://hermes.example.org/_platform/oidc/callback`, S256 PKCE and an audience
mapper, and a PostgreSQL role that can use only `gateway_sessions`:

```yaml
hermesManager:
  publicOrigin: https://hermes.example.org
  adminOrigin: https://hermes-admin.example.org
  api:
    gatewayIdentity: spiffe://agents.example.org/hermes-gateway
  gateway:
    existingSecret: hermes-gateway
    oidc:
      clientId: hermes-gateway
      jwksUrl: https://sso.example.org/realms/agents/protocol/openid-connect/certs
      audience: hermes-gateway
    sessionKeys: {active: gateway-2026-10, ids: [gateway-2026-10]}
```

The chart generates each role's configuration from these values; the
[chart guide](../../charts/agent-platform/README.md#what-the-chart-installs)
lists the keys of each role's Secret.

- `publicOrigin` and `adminOrigin` must be different hosts, so host-only
  cookies never cross between the two services.
- Every instance is reached only at
  `hermes.<namespace>.svc.<clusterDomain>:8080`, with the namespace taken from
  the resolver's answer. The gateway has no registry or Kubernetes access.
- `gateway.maxUploadMib` can only lower the 64 MiB upload bound.

## Hermes templates

A template version defines an instance. Draft versions can be edited.
Publishing makes a version immutable. Withdrawing a version prevents new
selections but leaves existing assignments on it.

```json
{
  "schemaVersion": 2,
  "image": "ghcr.io/accelero/agent-platform/hermes@sha256:<64 hex>",
  "resources": { "cpuMillis": 1000, "memoryMib": 2048 },
  "home": { "source": "nvme", "sizeGib": 5 },
  "mounts": {
    "workspace": { "mountPath": "/workspace" }
  },
  "defaultVolumes": {
    "workspace": { "source": "nvme", "sizeGib": 10 }
  },
  "adminConfig": {
    "model": { "provider": "custom", "base_url": "https://llm.internal/v1", "default": "team-default" },
    "agent": { "max_turns": 30 }
  },
  "secrets": [
    { "name": "llm-gateway-key", "env": "OPENAI_API_KEY" }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `image` | Digest-pinned Hermes image that implements the platform's init and run entry points |
| `home` | The private Hermes home at `/opt/data`: native state, settings, credentials, memory and sessions. Always a dynamic volume. Never mounted in a terminal backend |
| `mounts` | Volume names this template mounts into Hermes, with their paths and optional `readOnly` |
| `defaultVolumes` | Volumes created for an assignment that does not define them itself |
| `adminConfig` | Admin settings enforced over the user's settings (see below) |
| `secrets` | Admin secrets delivered to the instance as environment variables (`env`) or read-only files (`file`, under `/run/hermes/admin-secrets/`) |

An assignment pins an exact template version. Changing it shows a preview
(resources, settings, storage and restart effects) and is blocked when the
new version would change a volume's storage class or shrink it. Applying a
version to many assignments is one bulk command, and the response gives the
result for each assignment.

## Admin settings

The managed `config.yaml` is read-only inside the instance and layered over
the user's own configuration. Highest precedence first:

1. **Platform-enforced:**
   - `terminal.backend=managed_execution`;
   - the image-owned `managed_execution` plugin;
   - its connection settings;
   - protected plugin enable and disable lists.

   Templates and assignments cannot change these, and validation rejects any
   attempt.
2. **Assignment `adminConfig` overrides**, for example a different default
   model for one user.
3. **Template `adminConfig`.**
4. **The user's settings** on the private home, editable in the native
   dashboard.

Managed values are never written into the user's file, and the native
dashboard drops a user's changes to managed keys while keeping the rest of
their settings. If an override is removed later, the user's own value, if they
had one before, applies again. A settings change restarts the instance
(Recreate) without reseeding the home.

`adminConfig` must not contain secret values; use admin secrets instead. It is
limited to 8 KiB.

## Admin secrets

Administrators create secrets by name. Each has a kind:

- `env`: delivered as an environment variable;
- `file`: delivered as a read-only file under `/run/hermes/admin-secrets/`;
- `registry`: delivered as image pull credentials for private Hermes or
  terminal images.

How values are handled:

- **Write-only.** Writing a value creates a new version. The API and dashboard
  show only metadata: name, kind, latest version, update time and how many
  templates and assignments reference it. Who wrote it is in the audit log.
- **Delivery.** Only referencing instances receive the value. A new version
  restarts them.
- **Sealing.** Values are sealed with the controller's public key. The API
  that stores them cannot decrypt them.

A delivered secret can be read by code in that instance, including the agent
acting for its owner. Admin secrets keep values out of templates, logs and
the dashboard; they do not hide values from the owner. For a shared LLM
provider, prefer a model gateway that issues per-user keys, so revoking one
user does not require rotating everyone's key.

## Volumes and mounts

An assignment defines the instance's volumes by name:

```json
{
  "volumes": {
    "workspace": { "source": "nvme", "sizeGib": 20 },
    "home":      { "source": "nas", "path": "bob" },
    "datasets":  { "source": "host-data", "path": "datasets/shared-bob", "readOnly": true }
  }
}
```

- **Dynamic volumes** (`storageClass` sources) take `sizeGib`. The controller
  creates a PVC. Sizes can grow but never shrink.
- **Static volumes** (`localPath`, `nfs`) take a relative `path` inside the
  source root. The controller creates a `Retain` PersistentVolume pointing at
  `root/path` (with node affinity for `localPath`) and a PVC bound to it.
- **Path rules.** Paths are clean relative paths: no leading `/`, no `.` or
  `..`, no empty segments. Two instances' paths on one source never overlap,
  so `bob` and `bob/projects` cannot belong to different instances.
- **Missing folders.** A missing or inaccessible folder blocks the instance:
  its status is `blocked` with `storage_unavailable` and the storage condition
  `mount_failed`, taken from the kubelet's mount failure. It is never replaced
  by an empty directory. Once the folder exists, the instance starts without
  any further command.

Mounts are declared by the consumers, not by the volume:

- the Hermes template's `mounts` place volumes in the Hermes container;
- each terminal backend version's mount slots place volumes in that backend's
  Pods (see [terminal backends](terminal-backends.md)).

A volume is mounted only where a template or backend version names it.

### Sharing a volume between Hermes and terminal backends

Mount the same volume name in the template and in the backend version. This
is the normal setup for `workspace`:

- files Hermes writes there appear in the terminal at the same path;
- files that shell commands create appear in Hermes at the same path.

Files uploaded through the native dashboard land in the private home
(`/opt/data`), because Hermes roots its file manager there; they are not
visible in terminal backends.

The rules that make this work:

- **Same path.** A shared volume must have the same mount path on both
  sides. The assignment is rejected otherwise (`mount_path_mismatch`), because
  paths that differ between Hermes and the shell break file references.
- **Same identity.** Every container runs as the data identity, so files
  created on either side can be read and written by the other.
- **Access.** Either side may mount it read-only. An assignment can mark a
  volume read-only, which applies everywhere.
- **Private home stays private.** The private home (`/opt/data`) can never be
  shared. It holds native credentials and the user's own provider keys.
- **Scheduling.** Both Pods must be able to mount the volume at the same
  time:
  - NFS and RWX classes work on any node;
  - `localPath` and node-local classes pin both Pods to the volume's node,
    which suits a single-server deployment;
  - on a multi-node cluster, use NFS for shared volumes.

## Lifecycle

| Action | Effect |
| --- | --- |
| Create the assignment with access `enabled` | Namespace, volumes, credentials, managed configuration and Deployment created; home initialized once; `Ready` after the native authentication check |
| Change template version, settings, secrets, terminal lifetime mode or a volume mounted in Hermes | Desired generation increases; the Deployment restarts with Recreate; volumes and the home are retained |
| Change terminal backends or volumes mounted only in terminal backends | New terminal environments use the new definition; existing leases finish; Hermes does not restart |
| Suspend or revoke | Access denied within 5 s; terminal work drained (at most 5 min); Hermes scaled to zero and its Deployment removed; volumes, credentials and namespace retained (`Retained`) |
| Re-enable | Same instance and data; a new Deployment starts on the retained volumes |
| Retry | Re-runs reconciliation without changing desired state |

When an update or stop sends SIGTERM to Hermes:

- its gateway and dashboard processes get a 20 s exit budget, then SIGKILL;
- the Pod grace period is 45 s;
- active agent runs can be interrupted;
- there is no automatic rollback.

No command deletes an instance's data: suspension, revocation and Helm
uninstall never delete instance namespaces, volumes or host data.

## Backups

Backups are deployment-owned. In a deployment that backs up to S3:

- back up PostgreSQL, as `pg_dump` or WAL archiving;
- back up every storage source root: the NVMe provisioner root,
  `localPath` roots and NFS exports;
- keep the manager's keyrings and the chart's existing Secrets in the
  deployment's secret store.

Restoring:

- **Same cluster.** Stop the manager, restore PostgreSQL and the data, then
  start the manager. It converges from the restored desired state.
- **New cluster.** Restore PostgreSQL and the data, install the chart, then run
  `hermes-manager rebind-cluster` (the chart's `operations.rebindCluster` Job)
  instead of `initialize`. It confirms the new cluster has no owned
  namespaces, records the new cluster identity, clears namespace bindings and
  retires old terminal environments. The controller then recreates every
  instance against the restored paths.
- **Dynamic volumes.** They come back on the same host paths only if the
  provisioner's path pattern is deterministic. Verify this in a restore drill.

The manager never adopts namespaces or volumes it has no record of. Every
minute the controller compares the namespaces and PersistentVolumes carrying
the installation label with the registry and records the rest as orphans, for
example the namespace of an instance created after the restored backup. The
dashboard's Storage page and `GET /api/v1/orphans` list them with when they were
first and last seen. Delete or keep them by hand; the manager never touches
them. `rebind-cluster` clears the report, since it describes the old cluster.
