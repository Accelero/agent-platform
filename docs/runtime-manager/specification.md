# Runtime manager specification

The manager gives Hermes terminal workers (SSH Pods in the execution namespace)
on lease. It is one process with one SQLite database. Hermes, and optionally the
Codex launcher, are its clients; each has its own credential and deployment
scope. [execution.openapi.yaml](../../contracts/runtime-manager/execution.openapi.yaml)
is the authority for requests, responses and error codes.

## Terms

- **Environment**: the record of one worker identity: deployment, profile and,
  for `per_session`, session (plus the invocation scope for task workers). It
  owns at most one Pod at a time.
- **Generation**: names the current Pod, `hermes-exec-<environment>-<generation>`.
  It changes only after the previous Pod is confirmed absent.
- **Lease**: a client's claim on an environment. Active leases keep the Pod.
- **Retire reason**: why the current Pod must be replaced (`closed`, `config`,
  `schedule`, `idle`, `lease_lost`, `pod_lost`).
- **Failure**: `image_unavailable`, `start_failed` or `start_timeout`.

## Requirements

### Worker lifecycle

| ID | Requirement | Tests |
| --- | --- | --- |
| X1 | A Pod that was requested and is not Ready is never retired by the idle timers (`session_idle_seconds`, `on_demand_idle_seconds`). Idle time counts from the moment the Pod became Ready or the last lease ended. | `test_a_cold_start_slower_than_the_idle_timers_is_not_torn_down`, `test_a_pod_that_is_starting_is_never_idle`, `test_a_pod_that_just_became_ready_restarts_the_idle_clock_instead_of_expiring`, `test_cold_start_through_the_plugin_survives_idle_timers` |
| X2 | A Pod that is not Ready `start_timeout_seconds` after it was requested is a failure `start_timeout`. | `test_a_pod_that_does_not_become_ready_in_time_fails_with_start_timeout`, `test_a_pod_that_never_becomes_ready_times_out_but_a_slow_one_does_not` |
| X3 | An image pull or image name error is `image_unavailable`; a failed Pod, a terminated container or a container creation error is `start_failed`. An unschedulable or merely slow Pod is neither. The failure and its time are recorded once per attempt. | `test_failures_are_recorded_with_their_reason`, `test_pods_are_classified_by_what_waiting_can_still_fix`, `test_pull_failure_reaches_the_plugin_then_recovers_after_backoff` |
| X4 | While a failure is recorded, acquiring that environment answers 422 `backend_failed` with the reason. After `failure_backoff_seconds` the failed record is removed and the next acquire starts a fresh attempt. A change of template, image, scope, mode or template configuration ends the backoff at once. Maintenance never starts a Pod for a failed record. | `test_an_image_that_cannot_be_pulled_is_reported_with_its_reason_not_retried_blindly`, `test_maintenance_never_starts_a_pod_for_a_failed_record`, `test_a_changed_image_ends_the_backoff_at_once`, `test_a_failed_attempt_is_removed_then_kept_for_the_backoff_then_forgotten`, `test_a_failed_attempt_is_reported_until_the_backoff_ends_or_the_config_changes`, `test_a_replacement_waits_for_confirmed_pod_absence` |
| X5 | A Pod that was Ready and is then failed, gone or terminating without a retire reason makes the environment `pod_lost`: its leases end at once (their heartbeats answer `lease_expired`) and a new generation replaces it. | `test_a_lost_worker_ends_its_leases_and_is_replaced`, `test_a_lost_worker_ends_its_leases_at_once_and_is_replaced`, `test_forced_lease_ends_reach_the_plugins_heartbeat` |
| X6 | A heartbeat never extends a lease beyond `max_lease_seconds` after its creation, and replaying the acquire request does not either. | `test_a_lease_cannot_be_renewed_past_its_maximum_age`, `test_forced_lease_ends_reach_the_plugins_heartbeat` |
| X7 | When a replacement is due while leases are live, new acquires without a live lease of that environment answer 409 `environment_draining`. After `drain_deadline_seconds` the remaining leases end and the Pod is replaced. | `test_a_replacement_waits_for_leases_then_ends_them_at_the_drain_deadline`, `test_a_replacement_waits_for_leases_until_the_drain_deadline`, `test_forced_lease_ends_reach_the_plugins_heartbeat` |
| X8 | A request ID whose lease is live returns that lease. A request ID whose lease ended is refused with 409 `lease_not_live` for `request_memory_seconds`; it never grants a new lease. A request ID used for another environment is refused as invalid. | `test_a_live_request_returns_its_lease_and_an_ended_one_is_refused`, `test_a_request_id_cannot_move_to_another_environment`, `test_request_ids_are_idempotent_and_never_revive`, `test_a_concurrent_release_cannot_revive_an_acquire_request`, `test_a_slow_cluster_observation_cannot_revive_an_expired_request` |
| X16 | Heartbeat, release and status never wait for a cluster call. A cluster call for one profile does not delay another profile's acquire. | `test_lease_operations_never_wait_for_a_cluster_call`, `test_tls_lease_traffic_and_other_profiles_do_not_wait_for_cluster_io` |

### Interface

| ID | Requirement | Tests |
| --- | --- | --- |
| X9 | Every non-2xx response carries `error` and `code` as in the contract; `environment_pending` and `environment_draining` carry `Retry-After`. Every response matches the contract. | `test_every_operation_of_the_contract_is_served_and_conforms`, `test_every_error_carries_a_stable_code_that_the_contract_declares`, `test_requests_are_validated_before_they_reach_the_manager` |
| X10 | A connection that does not complete its TLS handshake does not delay other connections and is closed after 10 s. | `test_a_connection_that_never_finishes_its_handshake_blocks_nobody` |
| X11 | A managed Pod older than `orphan_grace_seconds` that no environment record names is reported by `GET /v1/orphans` and logged once. It is never adopted. It is deleted only when `orphan_policy` is `delete`, and then only after the grace period. | `test_orphans_obey_grace_are_reported_once_and_clear_when_gone`, `test_a_named_worker_is_never_an_orphan`, `test_orphan_deletion_is_opt_in_and_waits_for_the_grace`, `test_orphaned_workers_are_visible_to_the_client`, `test_an_acquire_during_orphan_listing_is_never_deleted` |
| X12 | State survives a restart. A database written by the previous manager is upgraded at startup without losing environments or live leases, and the Pods it names are adopted. | `test_a_restarted_manager_keeps_leases_and_workers`, `test_a_legacy_database_is_converted_and_its_worker_adopted`, `test_the_upgrade_runs_once_and_a_newer_database_is_refused`, `test_state_constraints_reject_values_outside_the_enums`, `test_restarting_the_manager_keeps_the_clients_lease` |
| X15 | The Hermes plugin retries `environment_pending`, `environment_draining`, `conflict` and 503 until its acquire timeout, fails at once on `backend_failed` and `lease_not_live` with the server's message, includes the last server message in a timeout, and treats a definitive heartbeat 409 as a lost lease immediately. | `test_plugin_retries_temporary_refusals`, `test_plugin_fails_at_once_with_the_managers_message`, `test_plugin_timeout_keeps_the_last_server_message`, `test_heartbeat_refusals_end_the_lease_at_the_right_time`, `test_pull_failure_reaches_the_plugin_then_recovers_after_backoff` |

### Worker identity

| ID | Requirement | Tests |
| --- | --- | --- |
| X13 | The default worker image opens SSH sessions as the UID and GID of the Pod, for any non-zero values the chart declares. It retains authorized-key authentication, SFTP, explicit environment forwarding and clean shutdown on TERM or INT. | `test_worker_ssh_binds_the_account_to_the_pod_identity` |
| X14 | With `sandbox.hostKey.secretName`, workers present that host key and the plugin accepts only that key; without it each generation's first key is trusted and a changed key is refused. | `test_worker_host_key_reaches_workers_and_only_the_public_half_reaches_hermes`, `test_a_provisioned_host_key_replaces_trust_on_first_use`, `test_worker_presents_the_provisioned_host_key_and_plugin_refuses_another` |

### Retained behaviour

| ID | Requirement | Tests |
| --- | --- | --- |
| R1 | A client selects a configured template by name and may select an image; an invalid image, an unknown template or deployment, or client-supplied mount or claim specifications never reach the cluster. | `test_requests_the_manager_does_not_approve_never_reach_the_cluster`, `test_any_profile_can_select_any_configured_template_and_image`, `test_a_client_cannot_introduce_namespaces_claims_or_mount_specifications` |
| R2 | A profile mode shares one Pod among all sessions of the profile; `per_session` gives each session its own Pod and the same session reuses it. | `test_a_profile_worker_serves_all_sessions_and_each_session_gets_its_own`, `test_plugin_defaults_to_profile_scoped_on_demand_workers` |
| R3 | A changed template, image, invocation scope or mode waits for active work (409) and then replaces the Pod under a new generation. A changed template configuration in the chart replaces idle Pods the same way. | `test_a_changed_image_waits_for_active_work_then_replaces_the_pod`, `test_a_changed_template_waits_for_active_work`, `test_a_changed_template_configuration_replaces_an_idle_worker`, `test_configuration_drift_replaces_an_idle_worker`, `test_a_changed_profile_mode_waits_for_live_work_even_on_replay` |
| R4 | `serialize_profile` allows one profile worker at a time and `exclusive` allows one run per environment. | `test_serialized_exclusive_templates_allow_one_run_per_profile`, `test_serialization_retires_an_idle_sibling_before_starting_another`, `test_the_runtime_manager_release_smoke_payload_uses_the_current_package` |
| R5 | A daily recycle (`recycle.at`, `recycle.timezone`) replaces a profile Pod once per calendar day; a Pod created after the boundary is not recycled for it. | `test_the_daily_recycle_replaces_a_profile_worker_once_per_day`, `test_the_daily_recycle_runs_once_per_day_and_not_for_a_new_pod` |
| R6 | `per_session` Pods are retired after `session_idle_seconds`, on-demand Pods after `on_demand_idle_seconds`, an always-running Pod is restarted when lost while its profile was used within `recycle.inactive_after_seconds`, and closing a session retires its Pod. When close includes `runtime`, only environments with that exact invocation scope retire. | `test_idle_workers_are_retired_and_closing_a_session_retires_its_pod`, `test_an_always_running_profile_is_restarted_when_its_pod_is_lost`, `test_idle_workers_retire_by_mode`, `test_closing_a_session_can_filter_by_invocation_scope` |
| R7 | Credentials are distinct, at least 32 characters and bound to one deployment; leases, sessions and request IDs of one client are invisible to another. | `test_each_client_sees_only_its_own_leases_environments_and_request_ids`, `test_credentials_must_be_distinct_long_and_bound_to_one_deployment`, `test_another_clients_lease_is_forbidden` |
| R8 | Hermes and Codex invocation scopes mount only the selected profile, task or session directories, validated against read-only views, and never follow symbolic links. | `test_a_codex_run_mounts_its_task_home_and_session_store_only`, `test_per_profile_workspace_mapping_mounts_only_the_selected_profile`, `test_profile_images_registry_credentials_and_distinct_pvc_layouts_work_together` |
| R9 | Worker Pods run non-root with the configured UID and GID, no service-account token, no privilege escalation, all capabilities dropped and the approved image entrypoint. | `test_worker_pods_run_non_root_without_tokens_privileges_or_overrides` |
| R10 | The API stays compatible with the deployed stack: an unknown template or deployment is 400, a granted lease carries `environment`, `generation`, `lease`, `ttl` and `ssh`, the Pod is named `hermes-exec-<environment>-<generation>`, a Pod deleted outside the manager is replaced under a new generation, and a changed image applies once the previous lease is released. | `test_the_deployed_stack_can_replace_a_worker_deleted_outside_the_manager`, `test_native_backend_accepts_and_recognizes_chart_configured_runtime` |
## Architecture and verification

`services/runtime-manager/runtime_manager/domain/` holds the pure lifecycle
planner and admission rules. `service.py` executes their decisions through
`store.py` (short SQLite transactions) and `cluster.py` (bounded Kubernetes
commands). Per-profile locks serialize reconciliation; lease operations and
status reads do not take them. `api.py` handles authenticated scope and field
validation, and `server.py` serves TLS with a separate connection thread for
each handshake. `scope/` validates the existing Hermes and Codex mount policies.
Dependencies point inward, enforced by
`test_domain_and_adapters_keep_dependencies_pointing_inward`.

Every requirement names its proving tests. Tests have one of the `unit`,
`integration` or `e2e` levels; the default run needs no cluster or container
runtime. `uv run pytest -m image -q` builds the real worker and verifies SSH
account binding, key pinning and graceful shutdown in Docker. Image tests are
excluded from the default run. The TLS end-to-end tests use the shipped plugin,
real server, SQLite, Kubernetes adapter and a simulated namespace with a fake
clock. They do not establish behaviour on a live Kubernetes cluster.

Manager state upgrades in place from the previous schema at startup, preserving
environment IDs, generations and live leases so surviving workers are adopted.
Only one manager process may use the database. Older workers and clients keep
their existing API and mount contract; provisioned host keys require the new
worker entrypoint and remain optional. See the
[chart settings](../../charts/agent-platform/README.md#runtime-manager-lifecycle)
for timers, orphan policy and host identity configuration.
