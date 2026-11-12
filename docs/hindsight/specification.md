# Hindsight specification

Optional Hindsight uses external PostgreSQL. The chart seeds only fresh root homes;
native Hermes configuration owns existing homes and named profiles.

## Requirements

| ID | Required result | Proving tests |
| --- | --- | --- |
| H1 | With `workloads.hindsight.enabled: false`, there are no Hindsight objects or `HINDSIGHT_*` variables on `hermes`. `defaults.json` and `checksum/hermes-defaults` are byte-identical to today's. | `test_h1_disabled_chart_renders_no_hindsight_and_unchanged_defaults` |
| H2 | When enabled, the API runs from a digest-pinned image with its database URL from an existing Secret. Missing inputs fail the render, and an empty or non-PostgreSQL URL stops the container. | `test_h2_enabled_chart_renders_pinned_api_without_placeholders`, `test_h2_missing_inputs_fail_the_render`, `test_h2_empty_database_url_stops_the_container` |
| H3 | A fresh root home selects `hindsight`, `local_external` and bank `default`, as seen by the real provider, unless `hermes.config.memory.provider` says otherwise. | `test_h3_fresh_root_seed_writes_provider_and_connection`, `test_h3_seeded_home_initializes_the_real_provider`, `test_h3_explicit_root_provider_is_not_seeded` |
| H4 | A named profile never inherits the root's provider from the chart. A profile configured natively keeps its own bank across restart and upgrade, and works on a routed turn with no credential. | `test_h4_chart_profile_does_not_inherit_the_root_provider`, `test_h4_explicit_profile_provider_is_preserved`, `test_h4_native_profile_settings_survive_restart`, `test_h4_routed_profile_uses_its_native_settings_without_a_key` |
| H5 | Existing configuration, Hindsight JSON, credentials and explicit opt-outs are preserved, including `memory.provider: ""`. No file is added to a home whose `config.yaml` already exists. Unsafe paths are refused. | `test_h5_existing_home_is_not_changed_by_seeding`, `test_h5_explicit_empty_provider_is_preserved`, `test_h5_seed_refuses_symlinked_hindsight_parent`, `test_h5_interrupted_seed_converges` |
| H6 | The first retain creates the bank and the memory survives an API restart. A missing bank or an unavailable or rejecting API does not stop ordinary chat. | `test_h6_retain_creates_bank_and_survives_api_restart`, `test_h6_unavailable_api_does_not_break_a_turn` |
| H7 | Multica task processes hold no Hindsight variables, and a derived home initializes no provider and makes zero calls to the API. Built-in file memory keeps working. | `test_h7_multica_run_removes_hindsight_environment`, `test_h7_derived_home_makes_no_provider_calls` |
| H8 | Database and LLM credentials appear only as Secret references. They are not in ConfigMaps, seed payloads, the seeded JSON, logs, chart defaults or receipts. LLM endpoint URLs contain no credential, query or fragment. | `test_h8_credentials_render_only_as_secret_references`, `test_h8_seed_rejects_credentials_in_hindsight_settings`, `test_h8_synthetic_credentials_are_absent_from_api_logs` |
| H9 | Hindsight runs as non-root with no service-account token or RBAC. Enabling it requires `networkPolicy.enabled: true`; otherwise rendering fails because the API has no authentication. Only `hermes` has ingress. Sandbox workers, the manager and other workloads gain no access, and no workload but `hindsight` receives its database or LLM credentials. | `test_h9_pod_is_non_root_without_token_or_rbac`, `test_h9_only_hermes_gets_ingress`, `test_h9_enabled_api_requires_network_policy`, `test_h9_only_hindsight_gets_its_credentials` |
| H10 | Startup, readiness, migration failure, database outage and queued-work recovery have bounded, documented behaviour. Probes carry no credential. | `test_h10_probes_are_unauthenticated_http`, `test_h10_failed_migration_exits_without_serving`, `test_h10_queued_retain_completes_after_restart`, `test_h10_database_outage_fails_readiness_and_recovers` |
| H11 | No data PVC is needed and the cache is optional. Without an enabled or existing cache claim, the chart sets `HF_HUB_OFFLINE=1` and the rendered API becomes healthy without outbound access, using preloaded models. With either kind of cache claim, the chart adds no offline setting; an empty claim needs Hugging Face for the first download. An operator's `HF_HUB_OFFLINE` entry, including a Secret reference, is preserved without duplication. Disabling deletes no bank, credential or retained claim. The guide covers deselecting the provider first. | `test_h11_default_has_no_persistent_volume`, `test_h11_cache_claim_mounts_only_when_selected`, `test_h11_offline_models_only_without_cache_claim`, `test_h11_operator_offline_setting_is_preserved`, `test_h11_rendered_api_becomes_healthy_without_egress`, `test_h11_retired_login_claim_is_not_rendered_or_deleted` |
| H12 | The packaged chart keeps the upstream digest and wiring, and the release allowlists stay complete. | `test_h12_packaged_chart_keeps_hindsight_digest_and_wiring` |
| H13 | Seeded homes never select the public service. A root home with missing, empty or mode-less JSON stays on the internal URL, and a routed profile without settings makes no calls. Saved dashboard defaults (F2) are covered in the guide. | `test_h13_hermes_gets_internal_endpoint_fail_safe`, `test_h13_missing_json_never_selects_cloud`, `test_h13_routed_profile_without_settings_makes_no_calls` |
| H14 | The dashboard routes and the documented commands each activate and deactivate an existing home. A dashboard save writes the panel's full field set to `hindsight/config.json`, including defaults (31 stored keys for the qualified version), while `config.yaml` changes only the memory provider. The documented commands change only the provider and, when absent, the connection settings. | `test_h14_dashboard_settings_reach_the_real_provider`, `test_h14_documented_activation_changes_only_memory_settings`, `test_h14_documented_commands_match_the_guide` |

The root seed accepts exactly `mode: local_external`,
`api_url: http://hindsight:8888` and `bank_id: default`; extra fields or different
values are refused without echoing them. Named-home settings remain native Hermes
state. When Hindsight is enabled, workload environment entries cannot shadow the
chart-owned connection, database, LLM and worker fields; logging and other
non-owned environment fields remain overridable.

## Qualified upstream

Qualified locally on 2026-11-01 with disposable Docker containers and synthetic
credentials. These are compatibility and failure-behaviour checks; the mock LLM
is not evidence of memory quality. Local k3s policy checks also passed; production
CNI enforcement remains untested.

| Check | Result |
| --- | --- |
| API image | `ghcr.io/vectorize-io/hindsight-api:0.10.3@sha256:f80f304db313cbc8775acd601fda7d094f5854ebf9bc252d8134eb9276d56b40`; amd64 manifest `sha256:9174480a41edae0ad27d22346e12bb20bb7af2aba2edace95f2182d6143df29b`. Package and `/version` report `0.10.3`. MIT license at upstream tag `v0.10.3` (commit `eb6df499d35300e5b2f3f029b2e6adda04ed90f8`). |
| Identity | `uid=1000(hindsight) gid=1000(hindsight) groups=1000(hindsight)` with all capabilities dropped and no new privileges. |
| PostgreSQL | `pgvector/pgvector:pg17@sha256:7d0a41464797aeb9862dfdc0bbf1526b82cfd324cdd8036df19e3545f4a9ccb8`; `vector` 0.8.7, `pg_trgm` 1.6 and `btree_gin` 1.3 in `public` (plus PostgreSQL's built-in `plpgsql`). A non-superuser database owner migrated a fresh database with all three extensions created first by the administrator; all 25 application tables belong to that owner. |
| Health and authentication | `/health`, `/health/live`, `/version` and bank routes respond without an API key. First retain created bank `default`. |
| Hermes probes | Repository Dockerfile built from `privacy.stage_contexts` inputs; plugin revision `176f8c2de1369f569c489b831d143b78128b5535`, client 0.10.1. Missing or empty JSON uses environment, mode-less JSON selects cloud mode but uses the internal environment URL, configured routed profiles work without a key, unconfigured routed profiles are unavailable, saved public JSON overrides the environment. A private web URL is refused by `tools.url_safety.is_safe_url`; daemon environment inheritance is present in pinned Multica source. |
| Retain and recall | Real plugin `sync_turn`, retain drain and recall succeeded against the server using `mock/mock-model` and `recall_types: observation,world,experience`. The retained synthetic telescope fact survived API restart. The image contains the models; the chart now supplies `HF_HUB_OFFLINE=1` without a cache claim to suppress model metadata checks against Hugging Face. |
| LLM outage | With `openai` pointing at an unreachable local endpoint, verification warned after three attempts and the API became healthy in 24.77 s. No `HINDSIGHT_API_SKIP_LLM_VERIFICATION` is needed. |
| Queued restart | An accepted asynchronous batch of 24 documents completed in 19.79 s after SIGKILL/start with worker ID `hindsight`, with only one API worker using that database. |
| TLS | `sslmode=verify-full&sslrootcert=/etc/postgres/ca.crt` succeeded for migrations and runtime; PostgreSQL reports TLSv1.3 for the owner's runtime connections. |
| Empty URL and guard | The guard exits 1 for empty and non-PostgreSQL URLs without printing the value. With an explicitly empty environment value, this image fails initialization with `Database URL is required for migrations`; an absent value defaults to embedded `pg0`. The wrapper still treats empty as embedded for its directory precheck. This does not change H2 or the guard. |
| Logs | Neither synthetic database password nor synthetic LLM key appeared in info logs during startup, retain and restart. |
| Resources and probes | Cold startup 20.42 s; fresh pre-created-extension database 22.46 s. Initial idle cgroup memory 788,815,872 bytes (752 MiB), initial peak 797,605,888 bytes (761 MiB); after the 24-document retain, cgroup peak 1,012,891,648 bytes (966 MiB), process high-water RSS 1,216,192 KiB (1,188 MiB). Default request: 500m CPU / 1536Mi memory; limit 4Gi. Startup budget stays 300 s (5 s × 60), readiness `/health`; no liveness probe. These are local synthetic workload measurements, not capacity guarantees. |
| Black-holed API | Real provider and memory manager: default turn prefetch 3.00 s; synchronous override capped by Hermes at 8.00 s despite plugin timeout 120 s. No seed timeout addition is needed. |

The vendored `TestHermesDisablesExternalMemoryProvider` passed in
`golang:1.26-alpine@sha256:a2012b4a1d1f9f7e9f44e0d39b805a901e856b1dd80b4e6ee4338db547050308`.
Only the derived-home boundary is covered; no full Multica task or real model-backed
conversation was run. NetworkPolicy is the only API access control; enforcement
on the production cluster's CNI remains to be checked.

## Deployment limits

Set `HF_HUB_OFFLINE=1` to use the models already in the pinned image; a cold
startup without that setting can contact Hugging Face and fail with blocked
egress. Local k3s checks allowed only the same-release Hermes workload to reach
the unauthenticated API and confirmed nonroot execution without API credentials.
Recheck enforcement on the production CNI before enabling Hindsight.

The real-image tests (the `e2e` suite, see [testing](../testing.md)) cover native seeding, routed profiles, dashboard settings,
persistence, queued-work recovery, verified PostgreSQL TLS and memory API failure
responses using a synthetic model. No real-model memory-quality guarantee or full
Multica task follows from those checks. Deployment chooses LLM/database Secret
references, TLS, cache and backup coverage; Hindsight must remain disabled for
consumers of the singleton recovery API until its database is explicitly covered.
