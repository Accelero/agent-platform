# Manager implementation handover — 2026-10-21

## Dashboard scaffold follow-up

The manager serves a protected React/TypeScript/Vite overview and read-only
assignment, terminal-runtime and audit lists. It uses generated OpenAPI types and
the existing Rust Keycloak OIDC administrator-session boundary for both HTML and
assets. `admin.uiDirectory` enables it; omitting the field preserves API-only
mode. See the [dashboard guide](dashboard.md) for build/run and Keycloak setup.

The local/CI multi-stage image build uses pinned Node/Rust/Debian stages and a
finite, privacy-checked development context, and ships a non-root runtime without
Node. CI checks generated types, formatting, frontend/browser tests, the Rust
authentication boundary, compiled process and complete image build. The scaffold
does not add administration editors, a chart deployment or GHCR publication.

Verification: 170 Rust tests, 128 Python tests, 7 frontend client tests and 4
Chromium presentation tests; workspace Clippy/formatting, Ruff, Helm lint and
privacy preflight. The compiled manager serves real Vite assets with anonymous
navigation redirected and assets denied; HTTPS/PostgreSQL protocol tests verify
administrator-only access and logout. Protocol fixtures do not replace live
Keycloak acceptance. The previous native image/SSH evidence below remains valid;
the full cluster acceptance was not repeated for the dashboard scaffold.

## Admin terminal policy follow-up

The image adds protected personal-mode terminal policy. The manager pins
the plugin enable/disable policy and connection settings; the Hermes image
requires the bundled provider, rejects other terminal backends, applies managed
settings to profile scopes and refuses terminal creation if policy is invalid.
Changing profile environment variables does not remove personal mode because
the read-only owner mount remains authoritative. Terminal type selection by
`terminal.runtime_id` remains editable unless the admin pins it. See the
[operator guide](terminal-backends.md) for the configuration boundary.

Standard verification for this follow-up: 169 Rust tests, 127 Python tests,
Clippy/formatting/Ruff, Helm lint and privacy preflight. The native image smoke
also exercises conflicting profile YAML/environment settings, mandatory plugin
loading, shadow rejection, protected credentials and restart. The earlier full
Kubernetes/SSH acceptance below was not repeated for this configuration change.
No publication or deployment is part of this follow-up.

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
open. Routing, complete manager administration editors and coordinated backups
remain outside this implementation. Real Keycloak browser login and full release
qualification are separate acceptance requirements.

There is no pending SSH acceptance failure to resume; use the implementation
status and operator guide for the next scope decision.
