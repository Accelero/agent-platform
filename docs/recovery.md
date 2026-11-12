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

Run `python -m platform_recovery OPERATION --config PATH`. Supported operations
are `inventory`, `backup`, `final`, `restore-files`, `restore-database`, `verify`, `verify-final`
and `resume`. Configuration includes the descriptor as `recovery`, resolved
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

| Payload | Reader behavior |
| --- | --- |
| Legacy schema 1, PostgreSQL 17 | Accept the full declared dataset set; cold restore excludes manager bookkeeping |
| Legacy schema 2, known selection 1, PostgreSQL 17 | Accept the immutable selected dataset set and validate all hashes and source identity evidence |
| Schema 3, API 1, supported platform release | Additionally validate the recorded source inventory digest, required bindings and recovery API |
| Unknown schema, selection, required Secret or platform release | Refuse; never infer a fresh installation |

Legacy formats lack a source application version gate. Their support preserves
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
An interrupted deletion must be inspected explicitly; do not clear this marker
to resume an installation that infrastructure teardown may already be removing.

Database recovery temporarily holds both the custom-format dump and full
plaintext `restore.sql` on the protected staging PVC. Failed import or ambiguous
commit receipts retain both for inspection; never print/export their contents.
Successful retry removes them. Preserve this evidence until recovery is resolved.

A newer reader may add selection versions while retaining the exact definitions
of all recorded source versions. Other inventory shape changes require migration.
