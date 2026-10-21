# Terminal backend handover — 2026-10-21

The continuation completed the previously unfinished terminal integration
acceptance on `feat/personal-hermes-commissioning`. No release or deployment was
performed for this implementation.

## Implemented and verified

- Native profile/dashboard `terminal.runtime_id` and the bundled managed-execution
  plugin, with infrastructure-controlled connection settings.
- Database runtime/policy selection, instance credential authorization, owner
  PVC/subpath enforcement and fenced on-demand v2 worker creation/start/readiness.
- A separate retained instance SSH server identity, authenticated controller SSH
  probe, API public host key and strict plugin host-key pinning.
- Ready-worker loss observation and exact-UID retirement, retaining history and
  old request tombstones while allowing fresh requests after retirement.
- Migration `0018_worker_ssh_identity.sql`, contracts and the
  [operator guide](terminal-backends.md).

## Integration fixes

1. Policy activation verifies the authenticated current native rendering digest
   after template updates, while preserving creation proof and UID ownership.
2. V2 mounts the immutable authorized-key file below the image's root-owned
   directory; OpenSSH StrictModes remains enabled. Server private keys are copied
   from their read-only projection into a private file.
3. The Hermes image supplies UID/GID 1000 for personal Pods' OpenSSH client while
   preserving the upstream legacy `hermes` account at UID 10000.
4. Native readiness can complete while terminal workers are pending or active.
   Generation changes/removal retain their separate drain fences.
5. Acquisition can update usage time while a Ready observer holds its claim.
   The database still rejects unfenced changes to the observation lease; owner,
   deadline and fence remain intact during acquisition. A real PostgreSQL
   regression reproduced the failure before the fix and passes afterward.

The acceptance harness now checks Hermes's actual `returncode` result field and
requires a host-key verification error for the deliberately wrong-key connection.
Temporary production diagnostics were removed; fixture progress/failure logs are
bounded and use synthetic data only.

## Final verification

- Rust workspace: **168 passed**, 3 intentionally ignored live tests.
- Python suite: **116 passed**.
- Ruff, Rust formatting/Clippy, Helm lint, privacy preflight and diff whitespace.
- Rebuilt non-root native runtime startup/restart and SSH-client smoke test.
- Both explicit Kubernetes ownership/bootstrap tests.
- Complete native acceptance: two real Hermes instances; image/config updates,
  Pod restart, retained removal/re-add; profile-selected HTTPS requests; three
  allowed workers across two owners/two backend types; denied backend access;
  SSH execution/file transfer, wrong-host-key rejection, PVC subpath isolation,
  and all three worker retirement/absence receipts.

Local evidence logs (temporary, synthetic fixture data):

```text
/tmp/hermes-terminal-rust-verified.log
/tmp/hermes-terminal-clippy-verified.log
/tmp/hermes-terminal-python-complete.log
/tmp/hermes-terminal-native-resumed.log
/tmp/hermes-terminal-kubernetes-verified.log
```

No acceptance run is left running. The successful runner removed its Kubernetes
fixture; the exact disposable PostgreSQL fixture and its volume were removed
after verification. Recreate fixtures using the
[verification commands](implementation.md#verification-commands) when needed.
Run Cargo builds/tests sequentially against the shared target directory.

## Remaining work

Nonempty runtime Service/Endpoint egress, proactive always-running prewarming,
full API-managed lifecycle completion and deployment/release packaging remain
open. Routing, the manager administration frontend and coordinated backups remain
outside this implementation. Real Keycloak browser login and full release
qualification are separate acceptance requirements.

There is no pending SSH acceptance failure to resume; use the implementation
status and operator guide for the next scope decision.
