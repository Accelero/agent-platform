# Profile-selected terminal backends

Users can create more Hermes profiles and choose a terminal backend for each one
in the native Hermes dashboard. The bundled `managed_execution` plugin requests
that backend from the manager. The manager enforces the instance's approved
execution policy, creates and retires the worker, and supplies a lease and
verified SSH endpoint. This page describes the implementation; the wider product
requirements remain in the [specification](specification.md).

## Configure an owner and its backend types

Terminal configuration is stored in PostgreSQL. The administration API registers
immutable runtime versions, owner-local storage bindings and an assignment's
execution policy. A runtime ID such as `python-tools` is a stable backend type
identifier; its version pins an exact image digest. It is independent of Linux
UID/GID and Kubernetes Pod UIDs. These inputs use the typed
[administration contract](../../contracts/hermes-manager/openapi.yaml); the
manager renders Kubernetes resources rather than accepting arbitrary Pod specs.
Upgrade the manager schema through migration `0018_worker_ssh_identity.sql`
using the [maintenance/migration procedure](../../services/hermes-manager/README.md)
before starting this controller.

An assignment selects a default runtime and allowed runtime versions. Each
selection also pins its resources, lease/lifetime limits and owner-specific
mounts. Storage bindings identify the exact namespace/PVC UID and the permitted
mount path, optional relative subpath and read/write access. The same runtime
image can use different owners' PVCs. For example, a binding can permit only
`projects/example` from the workspace PVC at the worker's `/workspace`.

Register runtime definitions using **`sshContract: managed-ssh-v2`** for automatic
worker provisioning. The repository's agent-runtime image implements this
contract: non-root `worker`, SSH on port 2222, the selected authorized-key file,
shell/file transfer helpers and a manager-provisioned SSH server identity. The
manager mounts only the immutable authorized-key file at `/etc/ssh/authorized_keys/worker`
with `subPath: worker`, retaining OpenSSH's strict permission checks with an
image-owned parent directory instead of a group-writable Secret projection. The image
reads `/etc/ssh/hostkeys/ssh_host_ed25519_key`, copies it to a private 0600 file
and starts sshd with that identity. Custom images must implement the same
contract. Historical `managed-ssh-v1` definitions remain readable/registerable,
but the new controller does not grant automatic SSH readiness for them.

Infrastructure still supplies the manager configuration, HTTPS endpoint/CA,
keyrings and native-instance templates/mappings described in
[native commissioning](native-commissioning.md). The controller process also
needs `/usr/bin/ssh` and a private writable temporary directory for bounded SSH
probes. No private key is passed in command arguments or logged.
The Hermes image also supplies the UID 1000 account used by personal Pods so
the OpenSSH client can resolve its non-root caller.

## Select a backend in Hermes

In the native dashboard, select the agent profile and open its Terminal
settings. Set **`terminal.runtime_id`** to an allowed backend ID, for example
`python-tools`. Empty selects the manager's configured default. The field is
provided by the repository's guarded patch to the pinned dashboard schema and
is saved through the native profile-scoped configuration API:

```yaml
terminal:
  runtime_id: python-tools
```

The selection is stored in that profile's retained `config.yaml`. Backend
selection does not authorize an image, mount or extra privileges. The manager's
current allowlist is authoritative even if a user edits YAML directly. Unknown
or disallowed IDs fail the acquisition; there is no local or different-image
fallback. Administrator-managed `terminal.runtime_id` can pin the selection.

The required `managed_execution` plugin is bundled in the image so newly created
profiles can discover it. The managed configuration enables it and pins the
manager URL, CA, execution token path and SSH client-key path independently of
mutable profile settings. Additional enabled plugins are infrastructure inputs.
Personal instances also protect `plugins.disabled` and always load this provider
from the read-only image. A user/project copy cannot replace it. Profile YAML,
profile terminal scopes and environment variables cannot select local, Docker,
built-in SSH or another provider. The managed backend is `managed_execution`;
that plugin obtains the manager-approved SSH endpoint and host identity.
Missing or invalid admin policy blocks terminal creation rather than choosing a
fallback. These are Hermes configuration controls, not a sandbox for arbitrary
third-party Python plugins running inside the Hermes process.
Native profile names never widen the credential: profile home, session and
runtime identity separate worker environments within the authenticated instance.

Changes apply when an environment is acquired; active commands and leases stay
bound to the runtime/generation already granted. Provider transport caching
includes the selected runtime and native invocation context.

## Plugin–manager interface

The [execution contract](../../contracts/hermes-manager/execution.openapi.yaml)
is the only interface between the plugin and the manager. The manager rejects
unknown fields, so the plugin sends exactly these requests under the managed
`manager_url` (which ends in `/execution`):

| Plugin step | Request | Body |
| --- | --- | --- |
| A command needs a terminal | `POST /execution/v1/leases/acquire` | `session`, `mode`, `request` (idempotency ID), optional `runtime_id`, `parent_lease`, `runtime` |
| While the lease is in use | `POST /execution/v1/leases/heartbeat` | `lease` |
| Turn ends or SSH setup fails | `POST /execution/v1/leases/release` | `lease` |
| Per-session environment closes | `POST /execution/v1/sessions/close` | `session`, optional `runtime_id`, `runtime` |

- **Connection and lifetime.** `manager_url`, `token_file`, `ca_file`,
  `ssh_key`, `mode` and `acquire_timeout_seconds` come only from the managed
  configuration that the manager writes into `/run/hermes/execution-ca/config.yaml`.
  Profiles cannot change them. The manager uses `per_session`, so closing a
  conversation retires its worker. Outside a personal instance the provider
  reports itself unavailable.
- **Selection.** Profiles choose only `terminal.runtime_id` (dashboard). They
  cannot request an image, template, mount or Pod field; the contract has no
  such fields.
- **Identity.** The opaque bearer credential alone identifies the instance;
  requests carry no caller-selected scope. Native profile isolation travels in
  `runtime.hermes_home`, `session` and `runtime_id`.
- **Retries.** A `409` (environment pending) or `503` is retried with the same
  `request` ID until `acquire_timeout_seconds`. Any other status fails the
  terminal call without fallback.

`tests/test_managed_execution_selection.py` validates every plugin request
against the contract schemas and paths. Change the contract, the Rust handler
and the plugin together.

## Acquisition and SSH

1. The plugin authenticates every manager request with the instance's mounted
   opaque execution credential over certificate-verified HTTPS. The manager
   resolves the instance and checks access, policy generation, selected runtime,
   owner storage and quotas. A requested ID cannot choose another owner.
2. A new request durably reserves the environment and request identity. The
   controller commits a reviewed worker intent, creates the Pod, binds its exact
   UID and journals the start before removing its scheduling gate. Lost replies
   recover that same authenticated creation rather than creating a duplicate.
3. Kubernetes readiness is followed by an authenticated SSH login using the
   instance's client key and the manager-provisioned server public key. The
   controller checks the worker UID and re-observes the same Pod before recording
   readiness. Pending workers have no usable lease/endpoint grant.
4. The execution API returns environment/generation, lease/TTL and SSH
   `host`, `port`, `user` and `host_key`. The plugin pins that public key in a
   private known-hosts file for the managed environment/generation. Personal mode
   rejects a missing host key. The SSH client private key stays in the existing
   Hermes credential mount and is never returned by the API or mounted in a
   terminal worker.
5. Hermes executes commands and transfers files directly over SSH. The manager
   handles authorization and lifecycle, not command bytes. Lease heartbeats,
   release and session close remain scoped to the instance/runtime/environment.

The SSH server identity is distinct from the client key, derived from retained
encrypted instance material and projected only to its workers. The current design
uses a shared server key for that instance's workers; SSH aliases still include
each environment and generation. Network policy
allows SSH from the owning Hermes Pod and the control-plane controller; workers
receive no Kubernetes token. Nonempty runtime service-egress policy still needs
Service/Endpoint resolution and remains blocked rather than broadly allowed.

## Cleanup and remaining limits

Native instance readiness and terminal readiness are separate. Periodic native
resync can complete while workers are pending or active; it neither waits for
their retirement nor drains a healthy worker. Worker readiness still gates SSH
grants, and native generation changes/removal retain their separate drain fences.
Lease acquisition can update usage time while a readiness observer holds its
claim, without changing that observer's owner, deadline or fence.

Session close, access/policy/native-generation changes and lease expiry fence new
work and enter a durable drain. Live child leases are respected until the
original drain deadline. The controller journals an exact UID/resource-version
stop, deletes only that Pod and records retirement after observed absence.
An uncertain unbound CREATE remains retained for observation rather than being
discarded on a single 404. PVC data and credential material are retained.

Ordinary idle cleanup excludes always-running mode. New requests can create a
new environment after retirement; old request/lease tombstones cannot revive an
expired environment. Ready workers are periodically re-observed; a missing,
replaced, terminating or exited Pod enters retirement so its old endpoint cannot
continue granting new leases. New acquisitions can provision a fresh environment
after observed retirement. Commands interrupted by worker loss are not replayed.
Always-running mode currently skips ordinary idle cleanup; provisioning and
replacement require acquisition work. Proactive prewarming/replenishment without
a new request is not implemented.
This does not add coordinated backups, routing or automatic upstream image
selection.

## Verification

The normal Rust suite covers scoped policy/lease behavior, retirement fencing
and historical request preservation. Plugin tests cover profile configuration,
runtime-separated transport caching, retry identity and required SSH identity.
The passing extended native fixture builds the reviewed images and checks
the real dashboard schema/configuration handlers, bundled plugin, manager HTTPS
API, constrained controller, SSH/file transfer, host-key rejection, owner PVC
subpaths and retirement across two owners and backend types. It verifies three
allowed workers, a denied backend request and all three retirement/absence
receipts.

```bash
uv run python -m tests.personal_hermes.native_runtime --build
uv run python -m tests.personal_hermes.kubernetes --native
```

Use the disposable PostgreSQL fixture and toolchain from the
[verification commands](implementation.md#verification-commands). These tests
do not substitute for real Keycloak browser login or release qualification.
