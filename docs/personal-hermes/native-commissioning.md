# Database-driven native commissioning

The infrastructure stack owns `manager.infrastructure_templates` and
`manager.desired_users`. The controller owns assignments, instance IDs, versioned
template snapshots, operation journals, credentials and resource UID bindings.
Neither routing nor a terminal worker is required for a native instance to reach
`Ready`. No route is published by native readiness.

This guide describes implemented source behavior. See the
[implementation status](implementation.md) for evidence and remaining manager
work, and the [manager README](../../services/hermes-manager/README.md) for
process configuration. The existing chart does not install this manager.

The installation issuer is the Keycloak realm issuer. A desired user's `subject`
is its exact Keycloak `sub`, never its email or display name. There is one mapping
per installation and subject. An existing API-managed assignment is not silently
adopted by infrastructure synchronization.

## Installation

Run `hermes-manager initialize --config controller.json` explicitly with trusted
operator Kubernetes credentials and an empty database. The command verifies the
cluster UID and scans namespaces and persistent volumes for the installation's
names and ownership labels, refusing retained or unexplained resources. It then
applies migrations and initializes the registry. It never runs automatically on
controller startup or after a database connection failure. Existing installations
continue to require the maintenance/migration procedure.

`hermes-manager render-bootstrap --config controller.json` emits a Kubernetes List
of the release-owned RBAC, admission policies and parameter schema for the infra
stack to review and install. The controller cannot install or relax those policies.
Supply its ServiceAccount, separate encryption/ownership keyrings, public native
OIDC client/origin and network settings using the controller configuration
described in the manager README. Run the controller with its constrained
ServiceAccount, separately from the administrator API.

## Desired templates and user mappings

Apply desired-state edits transactionally with an infrastructure database role
whose manager-table DML is limited to these two tables. Infrastructure owns the
template `definition` and desired user mappings; the controller maintains the
template's `condition` and `rejected_definition` diagnostics. There is no template
upload API or file watcher. Use bound SQL parameters in the stack;
the following is a nonsecret illustration, not an executable image reference:

```sql
INSERT INTO manager.infrastructure_templates(installation_id,id,definition)
VALUES (:installation, 'personal-default', :definition)
ON CONFLICT(installation_id,id) DO UPDATE SET definition=excluded.definition;

INSERT INTO manager.desired_users(installation_id,subject,template_id)
VALUES (:installation, :keycloak_subject, 'personal-default')
ON CONFLICT(installation_id,subject) DO UPDATE SET template_id=excluded.template_id;
```

The reviewed definition is a JSON object:

```json
{
  "schemaVersion": 1,
  "image": "registry.example.org/platform/hermes@sha256:<64 lowercase hex digits>",
  "storageClass": "local-path",
  "homeGib": 5,
  "workspaceGib": 10,
  "cpuMillis": 1000,
  "memoryMib": 2048,
  "adminConfig": {"agent": {"max_turns": 30}}
}
```

Definitions are typed deployment inputs rather than arbitrary Kubernetes Pod
manifests. The manager owns the non-root security context, mount boundaries,
native initialization and exact owner binding. Images must be digest-pinned and
provide the platform's native runtime entry points. Changing a definition selects
or creates an immutable catalog version and advances every mapped user's desired
generation. Changing a user's `template_id` selects another infrastructure
definition without transferring the owner or replacing credentials. Changes to PVC storage classes
or shrinking volumes require an explicit storage migration; they must not cause
automatic data replacement.

The controller polls bounded SQL batches; no notification or HTTP request is
necessary. Desired snapshots, generation fencing, audit and queued operations
commit atomically. Multiple replicas serialize ingestion. A restarted controller
claims durable work and recovers recorded Kubernetes actions by authenticated
creation provenance and exact UIDs, rather than adopting resources by name.
Invalid definitions record `invalid_definition` and are skipped until changed;
they do not dispatch a partial workload.

## Updates and shutdown

The infrastructure stack must update the database definition to request a new
container image. Images are digest-pinned: publishing an upstream release or
moving a registry tag does not change the desired state. The manager does not
watch registries, resolve tags or choose upstream upgrades.

For an ordinary image or template update, the manager updates the owned
Deployment using its recorded UID and resource-version preconditions. Kubernetes
performs its `Recreate` rollout, stopping the old Pod before starting the
replacement. This updates the same Deployment; it does not purge its PVCs or
regenerate the owner's credentials.

On SIGTERM, `personal-run.py` forwards SIGTERM to both gateway and dashboard
process groups, shares a 20-second exit budget between them and sends SIGKILL to
remaining children. The Pod termination grace period is 45 seconds. There is no
application-level handshake that waits for active agent runs to finish or
acknowledges a completed checkpoint. An update can interrupt active work.

The replacement's init container verifies the retained home. The manager waits
for current storage, exact workload/start identity, initialization receipt and
native HTTP/auth-gate observations before recording the generation as `Ready`.
Transient infrastructure failures are retried through durable work and delayed
resync; ownership/admission/recovery conflicts require intervention. There is no
automatic rollback to the previous image.

## Configuration and persistence

Each owner has separate `home` and `workspace` PVCs. Missing-only initialization
seeds the home once. Updates and restarts preserve native `config.yaml`, provider
onboarding, profiles, memory, uploads and other native state.

`adminConfig` becomes a read-only managed `config.yaml` in the native trust
ConfigMap. Hermes' supported managed scope overlays its leaves after the user's
configuration. Other user settings remain editable through the native dashboard
and remain on the home PVC. Managed values are not copied into the user's file.
A template/config generation change restarts the Recreate Deployment; the init
container verifies the retained home instead of reseeding it. Removing an
administrator override exposes the retained user value again, if present.
`adminConfig` is stored in the database template and a ConfigMap; it is not a
secret-delivery mechanism.

## Removal

Delete the desired mapping:

```sql
DELETE FROM manager.desired_users
WHERE installation_id=:installation AND subject=:keycloak_subject;
```

If an original resource creation is uncertain, the controller settles that
frozen creation before advancing to removal. It then revokes access, fences
older operations, closes native admission, stops registered children and retires
the exact Deployment UID. Retirement is complete only after observed absence.
The instance becomes `Retained`; its
namespace, PVCs, bootstrap credentials and audit history remain. Permanent data
purge is not implemented and would require a separate explicit workflow. Other
users' workloads are unaffected.

## Scope limits

PVC retention preserves data through Pod restarts, updates and mapping
removal/re-add. It does not create a second copy or protect against lost or
corrupted storage. The manager has no automated backups, snapshot schedule,
backup retention or implemented full-data restore/checkpoint workflow.

Each instance includes the native Hermes dashboard for its owner. The manager
has an administration API and browser-session authentication but no admin
frontend. Terminal catalog/policy/lease and worker primitives exist, but this
commissioning path does not create a working terminal backend. It also does not
publish a route. Those features remain separate integration work.

## Verification

The infrastructure PostgreSQL tests exercise concurrent ingestion, stable owner
mapping, immutable template updates, stale-claim rejection, removal and reopening.
The Kubernetes fixture exercises the constrained ServiceAccount against real
resource creation, image/config updates and workload retirement. Its `--native`
mode commissions two real Hermes instances, updates their images and managed
configuration, restarts a Pod, removes one mapping and re-adds it. It verifies
readiness, separate owner state, retained PVCs and user configuration throughout.
The standalone native runtime fixture builds the reviewed image and verifies
managed configuration precedence and forced-restart recovery.

Run the repository checks, the Rust workspace suite and these explicit fixtures:

```bash
uv run python -m tests.personal_hermes.native_runtime --build
uv run python -m tests.personal_hermes.kubernetes --native
```

The integrated fixture uses the image built by the native runtime fixture and
the disposable PostgreSQL container described in the
[verification commands](implementation.md#verification-commands). Both
fixtures create isolated resources and clean up their containers after the run.
