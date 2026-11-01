# Hindsight integration plan

Status: audited and ready to implement; nothing here has been implemented yet.
Audited on 2026-11-01 against commit `8441c76`, the pinned Hermes image and the
pinned Hindsight plugin. Baseline before any change: `uv run pytest -q` gives
1291 passed, 6 deselected, and `helm lint` is clean.

## Objective

Add a supported, optional Hindsight service to the application chart, backed by
external PostgreSQL. When it is enabled, a fresh default Hermes home uses Hindsight
with bank `default`. Named profiles use Hindsight only when configured in Hermes, and
banks are chosen there as usual. Multica task executions never use external memory.

## Scope

**In scope:** the chart's Hindsight workload, the Hermes connection, native seeding,
operator documentation and tests.

**Out of scope:**

- Honcho, a Hindsight web UI, per-user tenancy, bank content or research prompts.
- Personal profiles, a database operator, backup Jobs, cloud resources or a paid
  provider selection.
- An owned Hindsight image or an upstream Helm subchart. The upstream image is used
  by digest, and the upstream chart is reference material only.
- Changes to vendored Multica, to a deployment repository or to `shared-postgres`.
  That workload stays disabled, unsupported scaffolding.
- Commits, pushes, version bumps, tags or publication without a new user request.

## Verified facts

Every item below was checked during the audit and changes the implementation. Rows
marked *probe* ran the real provider code on a local build carrying the same plugin
revision. WP1 reruns those probes on the image built from this repository.

| # | Fact | Evidence | Consequence |
| --- | --- | --- | --- |
| F1 | With no `hindsight/config.json`, an empty one, or one without `mode`, the provider selects `mode=cloud`. It targets `https://api.hindsight.vectorize.io` unless `api_url` or `HINDSIGHT_API_URL` is set. | probe | Conversations could reach a public service, and the application namespace has no egress policy. Hermes needs the internal URL in its environment (H13). |
| F2 | With `HINDSIGHT_MODE=local_external`, `HINDSIGHT_API_URL` and no key in the environment, a root home without JSON stays internal, a routed profile with seeded JSON works, and a routed profile without JSON reports unavailable and makes no calls. A public `api_url` saved in the JSON still wins, and is reported available even without a key. | probe | The chart sets both variables on Hermes. Saved dashboard defaults remain the one route to the public service, so the guide must cover them (WP4). |
| F3 | In a turn routed to a profile other than the process's own, or under `gateway.multiplex_profiles`, `get_secret` does not fall back to `os.environ`, so a Pod-level key would not reach a named profile. | probe, `agent/secret_scope.py` | Running the server without a key removes the need for per-profile credentials (D3). |
| F4 | `templates/services.yaml`, `networkpolicy.yaml` and `configmaps.yaml` read the raw `.Values.workloads`, not the derived `agent-platform.workloads`. | source | Deriving `workloads.hindsight.enabled` from a second switch would render a Deployment with no Service and no NetworkPolicy (D1). |
| F5 | `workloads.yaml` mounts every `$workload.persistence` entry as a PVC even when that claim's `enabled` is false. Entries in `$workload.volumes` default to `readOnly: true`. | source | The current scaffolding Pod would stay Pending on the missing `hindsight-login` claim. Claims must be added conditionally. |
| F6 | The upstream image preloads its models under `/home/hindsight/.cache`, because it sets no `HF_HOME`. | upstream Dockerfile | The proposed default `emptyDir` cache would hide the preloaded models and force a download on every start (D4). |
| F7 | The image user is the non-numeric `hindsight`; the upstream chart uses UID/GID 1000. | image config, upstream chart | Kubernetes `runAsNonRoot` needs an explicit numeric `runAsUser`. |
| F8 | Application NetworkPolicies are ingress-only. | source | There is no egress rule to add. A database in another namespace must admit Hindsight through its own policy. |
| F9 | `workloads.yaml` renders readiness and startup probes only. | source | No liveness probe in this work (D6). `start-all.sh` already stops the container if the API misses `/health` within 300 s. |
| F10 | The API runs its worker in-process by default (`DEFAULT_WORKER_ENABLED = True`), and the worker ID defaults to the hostname. | upstream `config.py` | Set a stable worker ID. Chart Deployments use `Recreate`, so two writers never overlap. |
| F11 | `/health` is a readiness check that returns 503 when the database is down, `/health/live` is liveness, and neither handler takes a request context. | upstream `http.py` | Readiness uses `/health`. |
| F12 | `HINDSIGHT_API_DATABASE_URL` defaults to `pg0` (embedded), and `start-all.sh` treats an empty value as embedded. | upstream `config.py`, `start-all.sh` | A Secret holding an empty value could fall back silently. The chart adds a startup guard (H2). |
| F13 | The default tenant extension has no authentication. `HINDSIGHT_API_TENANT_EXTENSION=hindsight_api.extensions.builtin.tenant:ApiKeyTenantExtension` with `HINDSIGHT_API_TENANT_API_KEY` is the opt-in alternative. | upstream `tenant.py` | The chart uses the default (D3). The extension remains a later hardening option. |
| F14 | `recall_types` defaults to `observation`, which only exists after LLM consolidation. Upstream has `mock` and `none` LLM providers; `none` forces chunks-only mode. | plugin README, upstream `config.py` | A stubbed retain/recall test must not rely on the default recall type (WP5). |
| F15 | Multica's `buildEnv` passes the daemon's environment to task processes, which run in the Hermes Pod and share its network identity. Hermes's URL safety check blocks private addresses for its web tools by default, and terminal commands run in workers, which NetworkPolicy denies. | probe, `pkg/agent/claude.go`, `tools/url_safety.py` | `multica-run` unsets the Hindsight variables (H7). Direct access from a process inside the Hermes Pod is an accepted limit of D3. |
| F16 | `hermes profile create --clone` copies `config.yaml`, `.env`, `SOUL.md` and memory files, but not `hindsight/` or `plugins/`. `runtime-init.py` syncs the plugin only at boot. A selected provider with no plugin triggers `recover_at_startup`, which installs from the catalog when lazy installs are allowed. | `hermes_cli/profiles.py`, `plugins/memory/__init__.py`, `agent/agent_init.py` | Profiles created later are a documented manual path, and the guide must warn about clones (WP4). |
| F17 | `bootstrap.writers` reads each named Deployment, so a name that isn't deployed fails. | `platform_bootstrap/__main__.py` | Do not add `hindsight` to the default list. |
| F18 | `tests/unit/test_specification.py` enforces the requirement-to-test mapping for `docs/runtime-manager/specification.md`, and its pattern matches `R` and `X` IDs only. | source | Hindsight needs its own spec file and its own check. |
| F19 | The plugin pin is 39 commits after `v0.10.1`, dated 2026-09-23. The image installs `hindsight-client==0.10.1`, which equals the plugin's minimum, so its self-upgrade does nothing. Upstream is now at `v1.1.0`, 404 commits later, and is MIT licensed. | GitHub API, plugin source | Qualify a `0.10.x` server first (D7). |
| F20 | The upstream migration creates `vector` if it can and otherwise requires it to exist, and it also references `pg_trgm`. The local `postgres:17.9` image has no pgvector. | upstream `migrations.py` | Tests need a pgvector image. WP1 records the full extension list. |
| F21 | The Hermes dashboard configures memory providers natively (`/api/memory/provider`, `/api/memory/providers/{name}/config`). For Hindsight it offers Mode, API key, API URL, Bank ID and Recall budget, and non-secret fields go to `hindsight/config.json`. The panel's defaults are Mode `cloud` and the public API URL. | `hermes_cli/web_routers/memory_providers.py`, `hermes_cli/profile_memory_config.py`, plugin `config_schema.py` | The dashboard is the operator path for existing homes and named profiles, and saving its defaults could select the public service (WP4). |
| F22 | The plugin and the dashboard panel both fall back to bank `hermes` when no bank is set, and `HINDSIGHT_BANK_ID` overrides that fallback when there is no JSON. | plugin `__init__.py`, `config_schema.py` | The root bank is `default` (D9), so the chart sets `HINDSIGHT_BANK_ID`, and the guide warns about the panel default. WP1 confirms upstream accepts `default` as a bank ID, because its API paths also use `default` as a namespace. |

The current chart scaffolding (`hermes-stack-hindsight:dev`, `/opt/hindsight/start.sh`,
`openai-codex`, `CODEX_HOME`, the `hindsight-login` mount, `sharedMemory`) is replaced,
not repaired. `helm.sh/resource-policy: keep` already retains any PVC the chart created,
so removing the `hindsight-login` entry from defaults deletes nothing.

## Architecture

```text
Ordinary Hermes conversation
  -> plugin at $HERMES_HOME/plugins/hindsight (mode local_external)
  -> http://hindsight:8888, reachable only from the Hermes Pod
  -> Hindsight API with its in-process worker
  -> dedicated database on external PostgreSQL with pgvector

Multica task
  -> derived Hermes home with memory.provider = ""
  -> daemon environment without the Hindsight variables
  -> no external memory calls
```

Banks are logical partitions inside the database, not Kubernetes objects. A bank is
created by its first write. Nothing deletes a bank when a profile or release is removed.

## Decisions

These are settled for implementation. D2 and D3 were decided by the user on 2026-11-01.
The rest follow directly from the facts above.

| ID | Decision | Reason |
| --- | --- | --- |
| D1 | `workloads.hindsight.enabled` is the only activation switch. There is no `hindsight.enabled`. A top-level `hindsight:` block holds settings only. | It matches every other optional service, needs no template changes for F4, and removes the conflict error the old plan needed. |
| D2 | The chart seeds only the fresh root home. Named profiles use Hindsight only when configured in Hermes, and banks are chosen there as usual. The chart has no profile or bank settings. | Hermes already owns this configuration (F21), so a second place to set it would only drift. |
| D3 | Hindsight runs without API authentication. Access is controlled by NetworkPolicy: only `hermes` has ingress. Decided by the user on 2026-11-01. | The chart already requires a CNI that enforces NetworkPolicy, and sandbox isolation depends on it too. This removes a Secret and all per-profile credentials. Any process in the Hermes Pod can reach every bank, and if policies aren't enforced, so can anything else in the cluster. |
| D4 | The default has no cache mount, so the preloaded models are used. `persistence.hindsight-cache` is reused as the optional retained claim. | F6, and it adds no new setting. An empty claim hides the preloaded models, which the guide must say. |
| D5 | Existing homes are activated in the Hermes dashboard. The native commands remain documented for scripted use. Both are proven by tests, and there is no helper program. | F21, and `hermes config set memory.provider hindsight` works on the pinned image. |
| D6 | No liveness probe. | F9. Adding one would change the shared workload template. |
| D7 | The image candidate is `ghcr.io/vectorize-io/hindsight-api:0.10.3`, index digest `sha256:f80f304db313cbc8775acd601fda7d094f5854ebf9bc252d8134eb9276d56b40` (about 0.8 GiB compressed for amd64). If it fails with the pinned client, try `0.10.2`, then `0.10.1`. | It is the newest server on the client's minor line. Moving to `1.x` means moving the client and plugin pins too, which is separate work. |
| D8 | No new chart template file. Helpers go in `_application.tpl`. | Both files already in use are on the release allowlist, so `release-policy.json` doesn't change. |
| D9 | The default profile's bank is `default`. Decided by the user on 2026-11-01. | It's fixed in the seed and `HINDSIGHT_BANK_ID`, not a chart setting, because banks are otherwise chosen in Hermes (D2). |

## Requirements

Move this table into `docs/hindsight/specification.md` in WP0. If a test would pin
behaviour the table doesn't state, change the specification in the same change rather
than only adding the test.

| ID | Required result | Proving tests |
| --- | --- | --- |
| H1 | With `workloads.hindsight.enabled: false`, there are no Hindsight objects or `HINDSIGHT_*` variables on `hermes`. `defaults.json` and `checksum/hermes-defaults` are byte-identical to today's. | `test_h1_disabled_chart_renders_no_hindsight_and_unchanged_defaults` |
| H2 | When enabled, the API runs from a digest-pinned image with its database URL from an existing Secret. Missing inputs fail the render, and an empty or non-PostgreSQL URL stops the container. | `test_h2_enabled_chart_renders_pinned_api_without_placeholders`, `test_h2_missing_inputs_fail_the_render`, `test_h2_empty_database_url_stops_the_container` |
| H3 | A fresh root home selects `hindsight`, `local_external` and bank `default`, as seen by the real provider, unless `hermes.config.memory.provider` says otherwise. | `test_h3_fresh_root_seed_writes_provider_and_connection`, `test_h3_seeded_home_initializes_the_real_provider`, `test_h3_explicit_root_provider_is_not_seeded` |
| H4 | A named profile never inherits the root's provider from the chart. A profile configured natively keeps its own bank across restart and upgrade, and works on a routed turn with no credential. | `test_h4_chart_profile_does_not_inherit_the_root_provider`, `test_h4_explicit_profile_provider_is_preserved`, `test_h4_native_profile_settings_survive_restart`, `test_h4_routed_profile_uses_its_native_settings_without_a_key` |
| H5 | Existing configuration, Hindsight JSON, credentials and explicit opt-outs are preserved, including `memory.provider: ""`. No file is added to a home whose `config.yaml` already exists. Unsafe paths are refused. | `test_h5_existing_home_is_not_changed_by_seeding`, `test_h5_explicit_empty_provider_is_preserved`, `test_h5_seed_refuses_symlinked_hindsight_parent`, `test_h5_interrupted_seed_converges` |
| H6 | The first retain creates the bank and the memory survives an API restart. A missing bank or an unavailable or rejecting API does not stop ordinary chat. | `test_h6_retain_creates_bank_and_survives_api_restart`, `test_h6_unavailable_api_does_not_break_a_turn` |
| H7 | Multica task processes hold no Hindsight variables, and a derived home initializes no provider and makes zero calls to the API. Built-in file memory keeps working. | `test_h7_multica_run_removes_hindsight_environment`, `test_h7_derived_home_makes_no_provider_calls` |
| H8 | Database and LLM credentials appear only as Secret references. They are not in ConfigMaps, seed payloads, the seeded JSON, logs, chart defaults or receipts. | `test_h8_credentials_render_only_as_secret_references`, `test_h8_seed_rejects_credentials_in_hindsight_settings`, `test_h8_synthetic_credentials_are_absent_from_api_logs` |
| H9 | Hindsight runs as non-root with no service-account token or RBAC. Only `hermes` has ingress. Sandbox workers, the manager and other workloads gain no access, and no workload but `hindsight` receives its database or LLM credentials. | `test_h9_pod_is_non_root_without_token_or_rbac`, `test_h9_only_hermes_gets_ingress`, `test_h9_only_hindsight_gets_its_credentials` |
| H10 | Startup, readiness, migration failure, database outage and queued-work recovery have bounded, documented behaviour. Probes carry no credential. | `test_h10_probes_are_unauthenticated_http`, `test_h10_failed_migration_exits_without_serving`, `test_h10_queued_retain_completes_after_restart` |
| H11 | No data PVC is needed and the cache is optional. Disabling deletes no bank, credential or retained claim. The guide covers deselecting the provider first. | `test_h11_default_has_no_persistent_volume`, `test_h11_cache_claim_mounts_only_when_selected`, `test_h11_retired_login_claim_is_not_rendered_or_deleted` |
| H12 | The packaged chart keeps the upstream digest and wiring, and the release allowlists stay complete. | `test_h12_packaged_chart_keeps_hindsight_digest_and_wiring` |
| H13 | Seeded homes never select the public service. A root home with missing, empty or mode-less JSON stays on the internal URL, and a routed profile without settings makes no calls. Saved dashboard defaults (F2) are covered in the guide. | `test_h13_hermes_gets_internal_endpoint_fail_safe`, `test_h13_missing_json_never_selects_cloud`, `test_h13_routed_profile_without_settings_makes_no_calls` |
| H14 | The dashboard routes and the documented commands each activate and deactivate an existing home, and change only the provider and connection settings. | `test_h14_dashboard_settings_reach_the_real_provider`, `test_h14_documented_activation_changes_only_memory_settings`, `test_h14_documented_commands_match_the_guide` |

## Configuration interface

```yaml
hindsight:
  database:
    urlSecretRef: {name: '', key: url}   # required when enabled
    caSecret: ''                         # optional; mounted at /etc/postgres
  llm:
    provider: ''                         # required when enabled
    model: ''                            # required when enabled
    baseUrl: ''
    apiKeySecretRef: {name: '', key: api-key}   # empty name means no credential
```

- `{name, key}` follows `hermes.dashboardAuth.oidc.clientSecretRef`, and `caSecret`
  follows `multica.databaseCASecret`.
- The image stays at `workloads.hindsight.image`, like other third-party images. Do
  not add it to `images`, which holds the five release targets.
- The cache stays at `persistence.hindsight-cache`, and resources and probes stay on
  `workloads.hindsight`.
- Add the block to `values.schema.json`.
- To deploy the API without selecting it for the root home, set
  `hermes.config.memory.provider: ""`, which is ordinary native configuration.
- `hindsight-cache` stays in `persistence`. Remove `hindsight-login` from defaults.

### Rendered `hindsight` workload

Values defaults keep `enabled`, `image`, `ports` (`http`, 8888), `resources`, probes
and security contexts. `_application.tpl` appends the chart-owned environment, so an
operator's `workloads.hindsight.env` list can't remove it. An operator value that
collides with a chart-owned name fails the render, following the existing OIDC check.

| Setting | Value |
| --- | --- |
| `HINDSIGHT_API_DATABASE_URL` | `secretKeyRef` from `database.urlSecretRef` |
| `HINDSIGHT_API_LLM_PROVIDER`, `HINDSIGHT_API_LLM_MODEL` | from `llm` |
| `HINDSIGHT_API_LLM_BASE_URL` | only when set |
| `HINDSIGHT_API_LLM_API_KEY` | `secretKeyRef`, only when a name is set |
| `HINDSIGHT_API_WORKER_ENABLED`, `HINDSIGHT_API_WORKER_ID` | `true`, `hindsight` |
| `HINDSIGHT_API_LOG_LEVEL` | `info`, in values and overridable |
| `podSecurityContext` | `runAsNonRoot: true`, `runAsUser: 1000`, `runAsGroup: 1000`, `fsGroup: 1000` |
| `securityContext` | `capabilities: {drop: [ALL]}` |
| `readinessProbe` | `httpGet /health` on port `http` |
| `startupProbe` | `httpGet /health/live` on port `http`, period 5 s, failure threshold 60 (matches the 300 s wrapper limit) |
| `resources` | start at requests 500m / 1Gi and limit 4Gi; WP1 replaces these with measured values |
| `command` | the guard below |
| `postgres-ca` volume | at `/etc/postgres`, only when `caSecret` is set |
| `hindsight-cache` claim | at `/home/hindsight/.cache`, only when `persistence.hindsight-cache` has `enabled` or `existingClaim` set |

Remove `args`, `sharedMemory` and the default `persistence` list. The guard prints no
value and adds nothing else:

```yaml
command:
- /bin/sh
- -c
- |
  case "$HINDSIGHT_API_DATABASE_URL" in
    postgres://*|postgresql://*) exec /app/start-all.sh ;;
    *) echo 'HINDSIGHT_API_DATABASE_URL is not a PostgreSQL URL' >&2; exit 1 ;;
  esac
```

### Additions to the `hermes` workload

Add these only when `workloads.hindsight.enabled` is true:

| Variable | Value |
| --- | --- |
| `HINDSIGHT_MODE` | `local_external` |
| `HINDSIGHT_BANK_ID` | `default` |
| `HINDSIGHT_API_URL` | `http://hindsight:8888` |

`networkPolicy.ingress.hindsight: [hermes]` already exists and stays as it is. Do not
add a `sandboxIngress` entry. Leave the `shared-postgres` entries alone.

## Native seeding contract

WP2 produces this payload and WP3 consumes it, so they can run in parallel. Add one
helper, `agent-platform.hermesDefaults`, and use it for both the `hermes-defaults`
ConfigMap and `checksum/hermes-defaults`. Today those two places build the value
separately.

When `workloads.hindsight.enabled` is false, the helper emits exactly
`{"config": ..., "profiles": ...}`. When it is true:

```json
{
  "config": {"...": "...", "memory": {"provider": "hindsight"}},
  "profiles": {"other": {"memory": {"provider": ""}}},
  "hindsight": {"mode": "local_external", "api_url": "http://hindsight:8888", "bank_id": "default"}
}
```

**Chart rules:**

1. If `hermes.config.memory.provider` is set to anything, including `""`, keep it and
   omit the `hindsight` key. Otherwise set `hindsight` and include the `hindsight` key.
2. For each profile in `hermes.profiles`: keep an explicit `memory.provider`, whatever
   its value. Otherwise set `""`, so it doesn't inherit the root value on merge.
3. The chart seeds no Hindsight settings for a named profile, and an explicit
   `hindsight` provider there is not an error. That profile reports unavailable or uses
   the internal URL until it's configured in Hermes (F2).

**`seed_defaults` rules (`runtimes/hermes/runtime-init.py`):**

1. Validate `defaults.get('hindsight')` when present. It has exactly `mode`, `api_url`
   and `bank_id`, with `mode == 'local_external'`. Anything else raises `ValueError`,
   without echoing values.
2. Check whether the root `config.yaml` already exists.
3. If it's missing and settings are present, write `<root>/hindsight/config.json` first
   with `atomic_file(..., root, 'missing')`. If the file is still absent after that,
   raise.
4. Then write the configs as today. Named profiles get no Hindsight file.
5. If the root `config.yaml` exists, write no Hindsight file.

Writing JSON first means a crash can never leave a selected provider without its
settings, and rerunning converges. `atomic_file` already gives mode `0600` and refuses
symlinked parents. Leave plugin syncing and every other part of `initialize` unchanged.

**`runtimes/hermes/multica-run`:** before the final `exec`, add
`unset HINDSIGHT_MODE HINDSIGHT_BANK_ID HINDSIGHT_API_URL`.

## Work packages

Run them in this order: WP0, then WP1, then WP2 and WP3 in parallel, then WP4 and WP5,
then WP6. Only WP4 edits `charts/agent-platform/README.md`.

**Stop and report rather than improvising if:**

- WP1 finds no `0.10.x` server that works with the pinned client.
- A fact in this plan proves false in a way that changes a requirement.
- A fix would need a change to `vendor/`, the shared workload template or the
  deployment repository.

### WP0: Specification

- Create `docs/hindsight/specification.md` with the requirements table and an empty
  "Qualified upstream" section. The plan remains the implementation guide; the
  specification becomes the lasting record.
- **Exit:** the file exists and later packages update it.

### WP1: Qualify upstream

- **Needs:** Docker and network access to GHCR and Docker Hub. Use synthetic secrets.
- Run everything against disposable containers, using a digest-pinned
  `pgvector/pgvector` image, a non-superuser database owner and extensions created
  beforehand by an administrator.
- Run the API the way the chart will: `--user 1000:1000 --cap-drop ALL
  --security-opt no-new-privileges`, with the Pod environment above.

Check and record each of these in "Qualified upstream":

1. Image index digest, version, MIT license and `id` output.
2. The full list of required extensions, and that migrations succeed as the owner.
3. `/health`, `/health/live` and `/version` respond, and bank routes work with no
   key configured on either side, including for a bank named `default`.
4. Rerun the F1, F2, F3 and F15 probes on the image built from
   `runtimes/hermes/Dockerfile`.
5. The pinned plugin and client complete retain and recall against the server.
6. Which LLM option works for offline tests: `mock` first, then `none`. Set
   `recall_types` to `observation,world,experience` in test settings (F14).
7. Behaviour when the LLM is unreachable at startup, and whether
   `HINDSIGHT_API_SKIP_LLM_VERIFICATION` is needed. Don't set it by default unless the
   service otherwise can't start during a provider outage.
8. A queued retain finishes after a restart with a stable worker ID.
9. `sslmode=verify-full&sslrootcert=/etc/postgres/ca.crt` works for both the runtime
   and migrations.
10. Whether an empty URL really reaches embedded mode, and that the guard stops it.
11. Synthetic credentials don't appear in logs at `info`.
12. Peak memory at idle and during retain, and the startup time. Use these for
    `resources` and the probe thresholds.
13. How long a turn takes when the API is black-holed. The plugin's default timeout is
    120 s. If a turn blocks for more than 10 s, add a `timeout` key to the seeded JSON,
    update the seed contract and specification, and record why.

**Exit:** the table is complete and `workloads.hindsight.image` has its final digest.

### WP2: Chart wiring

- **Files:** `charts/agent-platform/values.yaml`, `values.schema.json`,
  `templates/_application.tpl`, `templates/configmaps.yaml`, `templates/workloads.yaml`
  (the checksum line only). New tests go in `tests/integration/test_hindsight_chart.py`,
  marked `integration`.
- Implement the configuration interface, the rendered workload, the Hermes additions
  and the chart rules above. Follow the conditional pattern at
  `_application.tpl:41-45` for `secretEnv`. Keep the enabled-only checks together in
  one helper with clear `fail` messages.
- Use a new fixture extending `examples/lightweight/values.yaml`. Don't change that
  file, because `agent_platform/release.py` requires its exact workload set.
- **Tests:** H1, H2 (render cases), H3 (explicit root), H4 (render cases), H8 (render), H9 (render), H10
  (probes), H11, H13 (render).
- **Exit:** those tests and `helm lint` pass, and the existing suite is unchanged.

### WP3: Native seeding and the Multica boundary

- **Files:** `runtimes/hermes/runtime-init.py`, `runtimes/hermes/multica-run`,
  `tests/test_runtime_init.py` (already `unit`). A test that runs `multica-run` with
  stubs may override the level with `integration`.
- Implement the seeding rules and the `unset` line. Don't change `native_files.py`
  unless a test shows it's needed.
- **Tests:** H3 (seed case), H4 (seed case), H5, H7 (`multica-run` case), H8 (seed case).
- **Exit:** those tests pass and `test_helm_profiles_seed_independent_images_without_overwriting_live_settings`
  still passes unchanged.

### WP4: Operator documentation

Add a `## Hindsight` section to `charts/agent-platform/README.md`, and list the new
block under "Installation inputs". It needs to let an operator do all of the following
from this repository alone.

**Prepare:**

- Create a dedicated database and login, separate from Multica's.
- Have an administrator create the extensions from WP1.
- Create the URL Secret, the optional CA and the LLM choice.
- Allow ingress from `app.kubernetes.io/name: hindsight` if the database has its own
  NetworkPolicy.

**Enable:** set `workloads.hindsight.enabled: true`. A fresh root home is configured
automatically. Existing homes and all named profiles are configured in Hermes.

**Configure a home in the dashboard.** Select the profile, choose Hindsight
as the memory provider, and save these fields in this order:

1. Set Mode to **Local External**, not the default Cloud.
2. Set API URL to `http://hindsight:8888`, not the default public URL.
3. Set Bank ID. The panel's default is `hermes`; the root home uses `default`.
4. Leave API key empty.

Saving the panel's defaults sends conversations to the public service (F2), so the
guide must say this plainly and the test must prove the settings above stay internal.

**Activate with commands** (scripted use; root shown):

```sh
kubectl -n "$NAMESPACE" exec deploy/hermes -c hermes -- \
  /command/s6-setuidgid hermes sh -ceu '
    export HOME=/opt/data HERMES_HOME=/opt/data
    umask 077
    mkdir -p "$HERMES_HOME/hindsight"
    [ -e "$HERMES_HOME/hindsight/config.json" ] ||
      printf "%s\n" "{\"mode\":\"local_external\",\"api_url\":\"http://hindsight:8888\",\"bank_id\":\"default\"}" \
        > "$HERMES_HOME/hindsight/config.json"
    /opt/hermes/.venv/bin/hermes config set memory.provider hindsight
  '
```

For a named profile, use `HERMES_HOME=/opt/data/profiles/<name>` and the bank you
chose. Set `HERMES_HOME` explicitly: `-p`
didn't update the profile file in the audit's probe. The test must state when a new
setting takes effect (new session or gateway restart), and the guide must repeat it.
If `config.json` already exists, check its `mode` and `api_url` first, because a `cloud`
entry can send conversations to the public service (F1).

**Other procedures to document:**

- **Profiles created later:** restart Hermes so the plugin syncs, then run the
  activation commands. A `--clone` copies the provider selection without `hindsight/`
  or the plugin, so until these steps run it can use the root's default bank or trigger
  a catalog install (F16).
- **Disable:** run `hermes config set memory.provider ""` in each active home first,
  then set `workloads.hindsight.enabled: false`. Banks, Secrets and claims stay.
- **Retired claim:** `hindsight-login` is no longer used. An existing PVC remains and
  can be deleted by hand once confirmed unneeded.
- **Access control:** there is no API key. Access depends on NetworkPolicy enforcement,
  any process in the Hermes Pod can read every bank, and banks are not per-person
  authorization (D3).
- **Defaults to know:** Hermes built-in memory stays on, and the plugin retains every
  turn and recalls observations by default. Cron and subagent contexts skip writes.
- **Backup:**
  - Durable state is the Hindsight database, the Hermes homes and the Secrets; model
    caches are replaceable.
  - Enabling Hindsight enrolls it in no backup schedule. Deployment tooling owns that
    and must honor the installation operation Lease.
  - Stop `hindsight` and `hermes` before restoring the database, and restore so that
    native settings still point at their original bank IDs.
  - A deployment that wants `hindsight` in `bootstrap.writers` must add it itself (F17).
- **Example:** add `examples/hindsight/values.yaml` as an overlay with synthetic Secret
  names and a test render.

**Tests:** H14, as an image test that runs commands extracted from the README, so the
guide and the proof can't drift apart. **Exit:** H14 passes in WP5's suite.

### WP5: Real-image suite

- **File:** `tests/e2e/test_hindsight_image.py`, marked `e2e` and `image`. Follow the
  skip rules in `tests/e2e/test_worker_image.py`. A default run must stay offline.
- Reuse the WP1 setup, with one Docker network holding pgvector, the upstream API and
  the Hermes image built from this repository. Exercise the real provider through
  `plugins.memory.load_memory_provider` inside the Hermes image, and build the Hermes
  home by running the real `seed_defaults`.
- **Tests:**
  - H2 (empty URL), H3 (real provider), H4 (routed turn), H6, H7, H8 (logs).
  - H10 (failure cases), H13 (missing JSON), H14.
- **H7 detail:** vendored Multica's `disableHermesMemoryProvider` writes the derived
  config, and the vendored `TestHermesDisablesExternalMemoryProvider`
  (`internal/daemon/execenv/hermes_home_test.go`) already covers it. There's no local
  Go toolchain, so run that test once through a pinned `golang` container and report
  the result. The Python test runs Hermes's real `_init_memory` on a home with
  `memory.provider: ""` against a counting HTTP stub and asserts zero requests and a
  working built-in memory.
- **NetworkPolicy check:** policy is the only access control (D3), so run one enforcement
  check on a cluster that enforces it, such as k3s. A connection from a sandbox worker
  and from another workload must fail, and one from `hermes` must succeed. Report it as
  not run if no cluster is available.
- **Optional real model:** when `HINDSIGHT_TEST_LLM_PROVIDER`, `_MODEL` and `_API_KEY`
  are set, also retain and recall with default settings. Skip otherwise, and never
  report a stub run as proof of memory quality.
- **Exit:** `uv run pytest -m image -q tests/e2e/test_hindsight_image.py` passes.

### WP6: Release checks and close-out

- Add H12 to `tests/test_release.py`, using `release.prepare_chart`.
- Add `test_hindsight_requirements_name_existing_tests` to
  `tests/unit/test_specification.py`. Use a separate `H` pattern and spec path, and
  leave the runtime-manager check as it is.
- Confirm `release-policy.json` needs no change. If a new owned file did appear, add it.
- Run all validation below and fill in the proving tests and the final report.

## Validation

```bash
uv sync --locked
uv run ruff check agent_platform tests services/tools/platform_bootstrap
uv run ruff format --check agent_platform tests services/tools/platform_bootstrap
uv run pytest -q
uv run pytest -m image -q tests/e2e/test_hindsight_image.py
helm lint charts/agent-platform
uv run python -m agent_platform.privacy
git diff --check
```

**Acceptance:**

- Every H row has named, passing tests, enforced by the WP6 check.
- A fresh default home writes to bank `default`, and the memory survives an API restart.
- No path reaches embedded PostgreSQL or the public service.
- A derived Multica home makes zero Hindsight calls.
- An existing home changes only through the documented activation.

**The final report must state separately:**

- Whether a real model-backed retain and recall ran, or only the stub.
- Whether live Kubernetes was tested. Render tests and Docker runs don't prove
  NetworkPolicy enforcement, which is the only access control here, so it must be
  checked before production use if no cluster ran.
- Whether a full Multica task ran, or only the derived-home boundary test.
- The final image digest and the measured resources.

## Deployment inputs

These belong to the deployment and are not chosen here: the LLM provider and model,
the PostgreSQL endpoint and TLS policy, Secret names, backup coverage, and the cache
choice. Version bumps, tags, publication and backup changes remain separate work.

## Sources

Upstream was checked on 2026-11-01. Source facts were read at tag `v0.10.3`, and the
plugin at `176f8c2de1369f569c489b831d143b78128b5535`.

- [Hindsight installation and external PostgreSQL](https://hindsight.vectorize.io/developer/installation)
- [Database, vector extension, API authentication and file storage settings](https://hindsight.vectorize.io/developer/configuration)
- [Hermes provider configuration and memory banks](https://hindsight.vectorize.io/sdks/integrations/hermes)
- [Bank creation semantics](https://hindsight.vectorize.io/developer/api/memory-banks)
- Upstream files: `docker/standalone/Dockerfile`, `docker/standalone/start-all.sh`,
  `hindsight-api-slim/hindsight_api/config.py`, `.../api/http.py`,
  `.../extensions/builtin/tenant.py`, `.../migrations.py`, `helm/hindsight/`.
- Pinned Hermes: `agent/secret_scope.py`, `agent/agent_init.py`,
  `plugins/memory/__init__.py`, `hermes_cli/profiles.py`.
- Repository: `AGENTS.md`, `release-policy.json`, the chart values and templates,
  `runtimes/hermes/`, `tests/`, and vendored Multica
  `internal/daemon/execenv/hermes_home.go` and `pkg/agent/claude.go`.
