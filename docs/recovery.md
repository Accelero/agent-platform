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
explicitly. Hash the resolved `recovery`, `datasets`, `writers`, `secrets`,
`backup_selections`, `namespace` and `execution_namespace` fields using sorted,
compact JSON and SHA-256 as `inventory_sha256`. The tools package verifies these
bindings against its embedded descriptor. The descriptor and tools image must
come from the same digest-pinned release.

Run `python -m platform_recovery OPERATION --config PATH`. Supported operations
are `inventory`, `backup`, `final`, `restore-files`, `restore-database`, `verify`
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
before restoration. The package never imports the transport implementation.

Keep the administrator Job lifetime at most 1200 seconds. The shared
`platform_recovery.lease.operation_lock` holds a 1500-second Kubernetes lease,
recognizes same-Pod subprocesses and never releases the parent holder's lease.
Bootstrap uses that exact implementation. All nested command budgets must fit
within the Job deadline; transports must bound individual pruning commands.
Supervisory hook/RPC timeouts are 1250 seconds, beyond the 1200-second Job
deadline and below lease expiry, so they cannot abandon a live upload and
release its lease. The stack entry point runs the installed platform module in the same process.
Job termination must precede lease expiry. Keep credentials available until all
verification and deletion guards have completed.

## Behavior and compatibility

Live backups keep application writers running, use SQLite online copies and a
transactional PostgreSQL dump, and provide crash consistency across files.
Final checkpoints force-stop writers after checking staging capacity; they do
not drain tasks. Successful final checkpoints retain the fence and replica
journal. Failed ones restore the previous fence/replicas. The next live backup
releases a pending final cutoff after an update. Restore imports and task
sanitization run in the same PostgreSQL transaction. Restore stays paused until
explicit resume. Missing-only seeding and the existing bootstrap recovery gate
and lifecycle hooks remain consumer integration points.

| Payload | Reader behavior |
| --- | --- |
| Legacy schema 1, PostgreSQL 17 | Accept the full declared dataset set; cold restore excludes manager bookkeeping |
| Legacy schema 2, known selection 1, PostgreSQL 17 | Accept the immutable selected dataset set and validate all hashes and source identity evidence |
| Schema 3, API 1, supported platform release | Additionally require matching resolved inventory digest and recovery API |
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
