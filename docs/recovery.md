# Application recovery contract, API 1

The platform owns application inventory, capture, validation, restoration and
writer/dispatch lifecycle. Deployments own storage/database provisioning,
credentials, backup transport, encryption, scheduling, retention and deletion
coordination. The operations tools image packages `platform_recovery` and the
same `files/recovery.json` descriptor shipped in the chart. No deployment source
or personal configuration enters the image. This contract initially supports
singleton Multica/Hermes; Hindsight is unsupported and must remain disabled.
Dynamic personal instances require the inventory extension in their own design.

## Consumer interface

Read `files/recovery.json` as ordinary chart data before deploying. Require
`apiVersion: 1`, resolve its dataset and namespace-scoped Secret bindings, and
preserve the platform writer list and selection versions. Add deployment Secrets
explicitly. Hash the stable logical inventory using sorted, compact JSON and SHA-256 as
`inventory_sha256`: sorted dataset IDs (without capacities), sorted writer names,
versioned selections with sorted IDs, sorted namespace/name/required Secret
bindings, namespace and execution namespace. Release versions, compatible-reader
lists and payload capability declarations are excluded. Schema 3 stores that
source inventory beside its digest; verification checks source integrity and
required binding compatibility, allowing optional deployment extensions to change. The tools package verifies these
bindings against its embedded descriptor. The descriptor and tools image must
come from the same digest-pinned release.

Both repositories pin the same fully specified recovery inventory golden file in
their unit suites; here it is
`services/tools/tests/fixtures/recovery-inventory-golden.json`. It includes
dataset capacities, writers, selections, application/execution namespace Secret
bindings and an optional deployment certificate. Its inventory SHA-256 is
`69c2d8c96ac21be4106450d5fcc1855950674a1d122da9ea14863ec5e2f142c5`.
Keep these fixtures identical when deliberately extending the shared contract.

Run `python -m platform_recovery OPERATION --config PATH`. Supported operations
are `inventory`, `backup`, `final`, `restore-files`, `restore-database`, `verify`,
`verify-final`, `invalidate-cutoff` and `resume`. Configuration includes the
descriptor as `recovery`, resolved
bindings, installation/current namespace/database UIDs, image/release references,
configuration/seed checksums, the Helm `release_name`, a `database_resource`
reference (`group`, `version`, `plural`, `name`) for its current UID check,
an `ownership_label` for restored objects, and a
trusted `transport_command` argv. Paths are `/datasets`, `/staging` and the
explicit configuration file. JSON status and errors never contain private state.
`inventory` validates without contacting Kubernetes. Preparation, capture and
completion are internal stages of the operation, retaining existing journal names.

The transport command accepts one JSON object on stdin:
`{apiVersion: 1, config: {...}, action: "...", values: {...}}`.
It returns one JSON result on stdout, a nonzero status with redacted diagnostics
on failure. Actions are `pointer`, `initialize_empty`, `retain` (`keep` snapshot),
`checkpoint` (`root`, `manifest`, `verify` directory), and `verify` (`record`,
`target`). Checkpoint must upload, restore/decrypt the entire payload, invoke
`python -m platform_recovery verify-payload` with `{config, record, checkpoint}`
on stdin, and only then publish and reread its completion pointer. Verify returns
`{checkpoint: "path", manifest: {...}}`; the platform independently validates it
before restoration. The transport RPC `verify` downloads without application
validation; `checkpoint` validates once before publication. The package never
imports the transport implementation. `invalidate_cutoff` adds
`cutoff_released: true` to a final completion record before writers restart;
its encrypted snapshot remains readable but cannot authorize deletion.

Keep the administrator Job lifetime at most 1200 seconds. The shared
`platform_recovery.lease.operation_lock` holds a 1500-second Kubernetes lease,
recognizes same-Pod subprocesses and never releases the parent holder's lease.
Bootstrap waits up to 180 seconds and retries 409 acquisition races; release uses
UID and resource-version preconditions. Other operations fail fast by default.

`POD_NAME` and `POD_UID` identify the owning Job. The runtime verifies both UIDs
and the actual Job start/deadline, subtracts 90 seconds for cleanup and caps the
operation at 1080 seconds. One monotonic deadline crosses every transport RPC;
all command timeouts are clipped to the remainder, and hashing checks it between
8 MiB reads. The old fixed 120-second validation timeout is removed. The RPC
supervisor allows 35 extra seconds for a killed/reaped child's safe unlock,
still inside the Job deadline. Oversized work fails closed before an unfinished
snapshot can become complete. Keep credentials through verification/deletion.

Only failure recovery may use `budget.cleanup()` to withdraw final proof after
the normal deadline. It adds at most 45 seconds to that original absolute
deadline, leaving the RPC supervisor's 35-second grace and a 10-second safety
margin inside the 90-second reserve. Nested contexts and retries cannot renew
the allowance. The adjusted deadline crosses the withdrawal RPC without changing
the normal configuration. Other transport actions cannot use this allowance;
ordinary backup, restore and resume work stays within the operation budget.
If withdrawal times out or fails, the fence and stopped writers remain in place.

Retention runs only after publication and is best effort. A timeout reports a
redacted warning and defers remaining work to a later backup, preserving final
writer state. Pruning's upper bound is 1800 seconds but the shared remaining
operation budget is always smaller. Transport runs default `restic unlock`
before repository operations and after a failed child; it never uses
`--remove-all`. Abrupt Pod/node loss can leave a lock on another hostname until
restic recognizes it as stale. Inspect/retry rather than remove active locks.

## Behavior and compatibility

Live backups keep application writers running, use SQLite online copies and a
transactional PostgreSQL dump, and provide crash consistency across files.
Final checkpoints force-stop writers after checking staging capacity; they do
not drain tasks. Successful final checkpoints retain the fence and replica
journal. Failed ones withdraw any final completion proof before restoring the previous
fence/replicas; failed proof withdrawal keeps writers stopped. Failed live
backups preserve pending cutoffs. The next live backup
releases a pending final cutoff after an update. Database recovery recreates the
claim fence even when restoring a live snapshot with no archived marker. Restore imports and task
sanitization run in the same PostgreSQL transaction. Restore stays paused until
explicit resume. Missing-only seeding and the existing bootstrap recovery gate
and lifecycle hooks remain consumer integration points.

Before returning control to Helm during an update, consumers must run `verify`
with the candidate tools/configuration, then `invalidate-cutoff`. Helm may
restart desired replicas immediately afterwards. The withdrawal operation holds
the shared lease, honors the deletion-intent gate and uses the existing transport
`invalidate_cutoff` action. It changes no fences or replicas. It needs the same
namespace, Job identity, lease and ConfigMap permissions as `verify`; no additional
RBAC permissions are required. Failed withdrawal must block the update. A later
teardown still requires a fresh final checkpoint from the late deletion guard.

| Payload | Reader behavior |
| --- | --- |
| Legacy schema 1 | Accept the full declared dataset set; cold restore excludes manager bookkeeping |
| Legacy schema 2, known selection 1 | Accept the immutable selected dataset set and validate all hashes and source identity evidence |
| Schema 3, API 1, supported platform release | Additionally validate the recorded source inventory digest, required bindings and recovery API |
| Unknown schema, selection, required Secret or platform release | Refuse; never infer a fresh installation |

Every payload must record the descriptor's `databaseMajor` (18 since 0.9.0), so
recovery points taken with the PostgreSQL 17 tools of 0.8.0 and earlier are
refused. Legacy formats lack a source application version gate. Their support preserves
the existing contract for the pinned singleton payload and is covered by
synthetic fixtures; it is not a promise to import arbitrary upstream databases.
Schema 3 includes `platform_version`, `recovery_api` and `inventory_sha256`.
Pre-API consumers reject schema 3, so retain the last compatible recovery point
until a separately reviewed migration/cleanup. Archive locations, identities and
UID changes still need explicit migration/restore binding. Existing completion
pointer schema 1, ConfigMap journals and bootstrap enrollment journal remain
readable without rewriting stored records. Database commits with an ambiguous
receipt still require operator inspection rather than destructive retry.

A completion pointer alone cannot authorize deleting infrastructure. The
consumer must independently compare final installation, configuration and current
UID evidence, and decrypt/validate the full payload again before durable deletion.
Preserve this protection even after uninstalling Helm. Publishing the new release,
live deployment and lost-VM qualification remain separate operational steps.

API 1 `verify-final` rechecks final/current identities and full payload while
holding the lease, then records `agent-state-deleting`. Other administrator
operations and bootstrap refuse that namespace after the marker, closing the
verification/deletion race. A retry of final verification remains allowed.
No operation clears the marker automatically.

### Abandoning a verified teardown

1. Stop the infrastructure teardown and any retry automation. Keep scheduled
   backups suspended, bootstrap disabled and every application writer stopped.
   Do not remove leases or alter completion metadata to bypass verification.
2. Inspect both application and execution namespaces, their UIDs and
   `metadata.deletionTimestamp`. Inventory remaining PVCs/PVs, their claim and
   installation ownership, the database and its UID. Compare those resources
   with the infrastructure tool's current state and deletion history. An absent
   namespace, a deletion timestamp, missing volumes/database, or a resource
   already removed from infrastructure state requires explicit recovery. Retained
   PVs alone do not prove an installation is safe to resume.
3. Confirm the matching final checkpoint before removing the marker. A pointer
   alone is insufficient: rerun `verify-final` in the usual bounded administrator
   Job with the retained, digest-pinned tools, configuration, credentials and
   storage mounts. It is permitted while the marker exists. Require successful
   full decrypt/readback validation, matching installation/current UIDs and
   configuration, `final: true`, and no `cutoff_released: true`. Keep the verified
   snapshot and repository credentials available.
4. Remove `agent-state-deleting` manually only when teardown is confirmed
   stopped, neither namespace is terminating, all required data and ownership
   bindings remain intact, infrastructure state has been reconciled, and the
   final checkpoint has passed verification. This must happen before any writer
   restarts. If any check is uncertain, keep the marker and recover explicitly.
5. With writers still stopped and the marker removed, run `invalidate-cutoff`
   through the platform API before Helm or another controller can restart them.
   A failed withdrawal keeps the installation stopped. Reconcile missing
   application resources under a reviewed recovery procedure, then explicitly
   `resume` when ready and take a new live backup before enabling its schedule.
   If continuing teardown instead, leave the marker in place and retry deletion.

Database recovery temporarily holds both the custom-format dump and full
plaintext `restore.sql` on the protected staging PVC. Failed import or ambiguous
commit receipts retain both for inspection; never print/export their contents.
Successful retry removes them. Preserve this evidence until recovery is resolved.

A newer reader may add selection versions while retaining the exact definitions
of all recorded source versions. Other inventory shape changes require migration.
