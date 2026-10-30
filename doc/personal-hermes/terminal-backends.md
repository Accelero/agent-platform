# Terminal backends

Hermes runs shell commands and file access in terminal backend containers
over SSH. An administrator defines terminal backends and grants them per
assignment. The user picks one per Hermes profile. The manager creates the
container only if the instance's assignment allows that backend, mounts only
the volumes the assignment binds, and retires the container when it is no
longer needed.

## Define a terminal backend (administrator)

A terminal backend has an ID, for example `python-tools`, and immutable
published versions:

```json
{
  "image": "ghcr.io/accelero/agent-platform/agent-runtime@sha256:<64 hex>",
  "sshContract": "managed-ssh-v2",
  "uid": 1000,
  "gid": 1000,
  "resources": { "cpuMillis": 2000, "memoryMib": 4096, "ephemeralGib": 10 },
  "limits": { "idleSeconds": 900, "maxLeaseSeconds": 3600, "maxEnvironments": 4 },
  "mounts": {
    "workspace": { "mountPath": "/workspace" },
    "home":      { "mountPath": "/home/worker/home", "optional": true }
  },
  "registrySecret": null
}
```

- **Image.** The image is pinned by digest. Custom images must implement
  `managed-ssh-v2`:
  - non-root user `worker` with the version's declared UID/GID;
  - SSH on port 2222;
  - the authorized key at `/etc/ssh/authorized_keys/worker`;
  - the provisioned host key at `/etc/ssh/hostkeys/ssh_host_ed25519_key`;
  - shell and file transfer.

  The repository's `agent-runtime` image implements this contract.
- **UID and GID.** Each is an integer from 1 through 4294967294. The
  administrator chooses them in the backend version; no installation allow-list
  applies, and neither user nor agent can override them. They must match the
  image's `worker` user. Hermes and its private home keep the data identity.
  Shared volumes are reliably usable with equal identities; different identities
  follow ordinary Unix permissions and may fail. There is no mapping or warning.
  Use backend-only volumes to give an agent another identity.

  Build `runtimes/agent-runtime/Dockerfile` with
  `--build-arg HOST_UID=<uid> --build-arg HOST_GID=<gid>`, or derive from
  `agent-runtime` and recreate the `worker` user with those IDs. The release
  provides one default image; no additional identity variants are published.
- **Mount slots.** Each slot names a volume and the path where it appears in
  the container. The assignment decides which actual volume fills the slot.
  A slot is required unless it is marked `optional`. If a required slot is
  unbound, the backend cannot be used for that instance; nothing is mounted
  in its place.
- **Private images.** `registrySecret` names a `registry` admin secret for
  private registries. The default image is never substituted.

A worker that fails is drained and not replaced, and its reason is shown on
the instance's environments in the dashboard:

| Reason | Cause |
| --- | --- |
| `image_unavailable` | The image cannot be pulled: wrong reference or digest, or missing registry credentials |
| `start_failed` | The container exits or keeps crashing |
| `contract_failed` | Admission/rendering refuses the worker, or SSH answers with a wrong host key, refused client key or shell UID different from the backend record |

A missing or mismatched image user can prevent startup (`start_failed`) or
break SSH authentication (`contract_failed`), depending on the image. The
readiness probe verifies `id -u`; it does not query the shell's GID. Connection
failures and SSH timeouts remain pending and are checked again.

Hermes receives `422 backend_failed` for a failed environment. Correct the
registry secret, or publish a corrected version and select it in the
assignment; the next acquisition after the failed environment has retired
starts a new worker.

## Grant backends to an assignment (administrator)

```json
{
  "terminal": {
    "mode": "per_session",
    "default": "python-tools",
    "allowed": [
      { "id": "python-tools", "version": 2 },
      { "id": "gpu-tools", "version": 1 }
    ]
  },
  "volumes": {
    "workspace": { "source": "nvme", "sizeGib": 20 },
    "home": { "source": "nas", "path": "bob" }
  }
}
```

With this grant, Bob's `python-tools` environments mount the shared
`workspace` at `/workspace` and his network home at `/home/worker/home`. If
`gpu-tools` declares only the `workspace` slot, its environments mount only
that.

Saving the assignment validates three things:

- every volume source and path is approved; static paths may be shared
  within or across assignments;
- every required slot of every allowed backend is bound;
- each volume mounted both in Hermes (through the template) and in a backend
  uses the same path in both.

See [instances](instances.md#volumes-and-mounts) for the volume rules.

`mode` sets the worker lifetime for the instance:

| Mode | Behaviour |
| --- | --- |
| `per_session` (default) | One environment per conversation; closing the conversation retires it |
| `per_profile_on_demand` | One environment per profile, retired after `idleSeconds` without leases |
| `per_profile_always_running` | One environment per profile, kept without idle retirement once created |

Commands use the declared working directory. The plugin recovers a session's
explicit directory under `/workspace` after Hermes's container guard; it also
accepts an explicitly supplied worker directory such as `/drive`. An omitted
directory or Hermes's generic `/root` default becomes `/workspace`, including
for scheduled jobs. A backend used without an explicit directory therefore
needs a usable `/workspace`. A directory other than `/workspace` selects
`per_session` mode for that environment, even with a profile lifetime setting.

Mode is written into the instance's managed configuration, so changing it
restarts Hermes. Changing the backend default, allowed versions or backend-only
volume bindings applies to newly acquired environments without restarting
Hermes. Changing a volume mounted by the Hermes template also restarts Hermes.

## Select a backend in Hermes (user)

In the native Hermes dashboard, open a profile's Terminal settings and set
**`terminal.runtime_id`** to an allowed backend ID, for example `gpu-tools`.
An empty value uses the assignment's default. The value is stored in that
profile's `config.yaml` on the private home:

```yaml
terminal:
  runtime_id: gpu-tools
```

The selection is a request, not a permission:

- the manager checks it against the current assignment every time an
  environment is acquired;
- an unknown or disallowed ID fails the terminal call, with no local or
  other-backend fallback;
- editing YAML directly gives the same result;
- an administrator can pin `terminal.runtime_id` in `adminConfig`.

Personal instances always use the image-owned `managed_execution` plugin:

- the managed configuration enables it and pins its connection settings;
- profiles, project copies and environment variables cannot replace it, or
  select local, Docker or built-in SSH execution;
- missing or invalid managed configuration blocks terminal use instead of
  falling back.

## Plugin–manager interface

The [execution contract](../../contracts/hermes-manager/execution.openapi.yaml)
is the only interface between the plugin and the manager. The manager rejects
unknown fields.

| Plugin step | Request | Body |
| --- | --- | --- |
| A command needs a terminal | `POST /execution/v1/leases/acquire` | `session`, `mode`, `request` (idempotency ID), optional `runtime_id`, `parent_lease`, `runtime` |
| While the lease is in use | `POST /execution/v1/leases/heartbeat` | `lease` |
| Turn ends or SSH setup fails | `POST /execution/v1/leases/release` | `lease` |
| A per-session environment closes | `POST /execution/v1/sessions/close` | `session`, optional `runtime_id`, `runtime` |

- **Connection.** `manager_url` (ending in `/execution`), `token_file`,
  `ca_file`, `ssh_key`, `mode` and `acquire_timeout_seconds` come only from the
  managed configuration at `/run/hermes/execution-ca/config.yaml`.
- **Identity.** The bearer credential alone identifies the instance; no
  request selects a scope. Profile isolation travels in `runtime.hermes_home`,
  `session` and `runtime_id`.
- **Retries.** `409` (pending) and `503` are retried with the same `request`
  ID until `acquire_timeout_seconds`. Any other status fails the call,
  including `422 backend_failed` for a worker that failed.
- **Delegated work.** Subagents join a live parent lease (`parent_lease`) in
  the same environment.

`tests/unit/test_managed_execution_selection.py` validates every plugin request
against the contract. Change the contract, the manager handler and the
plugin together.

## Acquisition and SSH

1. The manager authenticates the credential, requires access `enabled` and
   resolves `runtime_id` within the assignment's allowed versions.
2. It finds or creates the environment for that instance, backend version,
   assignment execution generation and session or profile. It returns `409`
   while the environment is not ready, and `422 backend_failed` once its
   worker has failed.
3. The controller creates the worker Pod:
   - non-root;
   - no Kubernetes token;
   - mounts: the bound volumes, the authorized key and the instance's host
     key.

   It waits for readiness, then logs in over SSH with the instance's client
   key and checks the host key it provisioned. Only then is the environment
   `Ready`.
4. The next acquire returns the environment, generation, lease and TTL, plus
   SSH `host`, `port`, `user` and `host_key`. The plugin pins that host key for
   that environment and generation, and uses the instance's mounted client
   key. No private key leaves the Hermes credential mount.
5. Hermes runs commands and transfers files directly over SSH. The manager
   never handles command bytes.

NetworkPolicy allows SSH to a worker only from its own Hermes Pod and the
controller.

## Changes and cleanup

Environments are retired, by deleting the exact Pod UID and recording the
retirement only once absence is observed, when:

- the session closes (`per_session`);
- it has had no live lease for the version's `idleSeconds` (`per_session`
  and `per_profile_on_demand`; `per_profile_always_running` never idles out);
- access is suspended or revoked;
- the assignment's backends or volumes change, or its backend is removed, and
  the old environment has no live leases;
- its worker failed (see above);
- the Pod is lost, replaced or terminating;
- the 5-minute drain deadline passes.

Retirement and its effects:

- Live leases, including joined child leases, are respected until the drain
  deadline.
- Expired and released leases are final. A later request creates a new
  environment instead of reviving the old one.
- Retirement never touches volume data.
- Commands interrupted by worker loss are not replayed.

Not supported:

- **Prewarming.** A worker is created on the first acquisition, not before.
- **Service egress.** Terminal backends get no egress to cluster Services.
