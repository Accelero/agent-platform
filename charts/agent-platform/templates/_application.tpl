{{/* Keep internal service wiring and native environment configuration in Helm. */}}
{{- define "agent-platform.workloads" -}}
{{- $workloads := deepCopy .Values.workloads -}}
{{- $multica := .Values.multica -}}
{{- $backend := index $workloads "multica-backend" -}}
{{- $recovery := .Files.Get "files/recovery.json" | fromJson -}}
{{- $parts := splitList "/" $recovery.fence -}}
{{- $maintenance := "" -}}
{{- range $backend.persistence -}}
  {{- if eq .name (first $parts) -}}
    {{- $maintenance = printf "%s/%s" (trimSuffix "/" .mountPath) (join "/" (rest $parts)) -}}
  {{- end -}}
{{- end -}}
{{- if not $maintenance -}}{{ fail "recovery fence requires its dataset mount" }}{{- end -}}
{{- if and $multica.maintenanceFile (ne $multica.maintenanceFile $maintenance) -}}
  {{- fail "multica.maintenanceFile must match the recovery descriptor fence" -}}
{{- end -}}
{{- $env := list -}}
{{- range $backend.env -}}
  {{- if not (has .name (list "DATABASE_URL" "FRONTEND_ORIGIN" "CORS_ALLOWED_ORIGINS" "MULTICA_APP_URL" "ALLOWED_EMAILS" "ALLOW_SIGNUP" "MULTICA_MAINTENANCE_FILE")) -}}{{- $env = append $env . -}}{{- end -}}
{{- end -}}
{{- range $name, $value := dict "DATABASE_URL" $multica.databaseUrl "FRONTEND_ORIGIN" $multica.publicUrl "CORS_ALLOWED_ORIGINS" $multica.publicUrl "MULTICA_APP_URL" $multica.publicUrl "ALLOWED_EMAILS" $multica.ownerEmail "ALLOW_SIGNUP" "false" "MULTICA_MAINTENANCE_FILE" $maintenance -}}
  {{- $env = append $env (dict "name" $name "value" $value) -}}
{{- end -}}
{{- $_ := set $backend "env" $env -}}
{{- range $key := $multica.mailKeys -}}
  {{- $_ := set $backend "secretEnv" (append $backend.secretEnv (dict "name" $key "key" $key "secretName" $multica.mailSecret)) -}}
{{- end -}}
{{- with $multica.databaseCASecret -}}
  {{- $_ := set $backend "volumes" (append (default list $backend.volumes) (dict "name" "postgres-ca" "mountPath" "/etc/postgres" "source" (dict "secret" (dict "secretName" .)))) -}}
{{- end -}}
{{- $hermes := index $workloads "hermes" -}}
{{- $_ := set $hermes "volumes" (append (default list $hermes.volumes) (dict "name" "hermes-defaults" "mountPath" "/etc/hermes-defaults" "readOnly" true "source" (dict "configMap" (dict "name" "hermes-defaults")))) -}}

{{- with .Values.sandbox.hostKey.secretName -}}
  {{- $_ := set $hermes "volumes" (append (default list $hermes.volumes) (dict "name" "worker-host-key" "mountPath" "/opt/ssh-host" "readOnly" true "source" (dict "secret" (dict "secretName" . "defaultMode" 292 "items" (list (dict "key" $.Values.sandbox.hostKey.publicKey "path" "host-key.pub")))))) -}}
{{- end -}}

{{- $env = list -}}
{{- range $hermes.env -}}
  {{- if not (has .name (list "MULTICA_APP_URL" "HERMES_DASHBOARD_BASIC_AUTH_USERNAME" "MULTICA_DAEMON_MAX_CONCURRENT_TASKS" "FIRECRAWL_API_URL" "SEARXNG_URL" "CAMOFOX_URL")) -}}{{- $env = append $env . -}}{{- end -}}
{{- end -}}
{{- range $name, $value := dict "MULTICA_APP_URL" $multica.publicUrl "MULTICA_DAEMON_MAX_CONCURRENT_TASKS" (toString .Values.hermes.maxConcurrentTasks) -}}
  {{- $env = append $env (dict "name" $name "value" $value) -}}
{{- end -}}
{{- range $name, $url := dict "firecrawl" "http://firecrawl:3002" "searxng" "http://searxng:8080" "camofox" "http://camofox:9377" -}}
  {{- if (index $workloads $name).enabled -}}
    {{- $key := index (dict "firecrawl" "FIRECRAWL_API_URL" "searxng" "SEARXNG_URL" "camofox" "CAMOFOX_URL") $name -}}
    {{- $env = append $env (dict "name" $key "value" $url) -}}
  {{- end -}}
{{- end -}}
{{- $_ := set $hermes "env" $env -}}
{{- $secrets := list -}}
{{- range $hermes.secretEnv -}}
  {{- if or (ne .name "CAMOFOX_API_KEY") (index $workloads "camofox").enabled -}}{{- $secrets = append $secrets . -}}{{- end -}}
{{- end -}}
{{- $_ := set $hermes "secretEnv" $secrets -}}
{{- $_ := include "agent-platform.dashboard" (dict "root" . "workload" $hermes) -}}
{{- $manager := index $workloads "runtime-manager" -}}
{{- $_ := set $manager "serviceAccountName" .Values.sandbox.managerServiceAccount -}}
{{- $items := list -}}
{{- range $name, $client := .Values.runtimeManager.clients -}}
  {{- $items = append $items (dict "key" $client.tokenKey "path" $name) -}}
{{- end -}}
{{- range $volume := $manager.volumes -}}
  {{- if eq $volume.name "manager-token" -}}
    {{- $_ := set $volume.source.secret "secretName" $.Values.runtimeManager.authSecret -}}
    {{- $_ := set $volume.source.secret "items" $items -}}
  {{- else if eq $volume.name "manager-tls" -}}
    {{- $_ := set $volume.source.secret "secretName" $.Values.runtimeManager.tlsSecret -}}
  {{- end -}}
{{- end -}}
{{- range $name := list "hermes" "codex-launcher" -}}
  {{- range $volume := (index $workloads $name).volumes -}}
    {{- if eq $volume.name "manager-token" -}}
      {{- $_ := set $volume.source.secret "secretName" $.Values.runtimeManager.authSecret -}}
    {{- else if eq $volume.name "manager-ca" -}}
      {{- $_ := set $volume.source.secret "secretName" $.Values.runtimeManager.tlsSecret -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- $_ := include "agent-platform.hindsight" (dict "root" . "workloads" $workloads) -}}
{{- $workloads | toJson -}}
{{- end -}}

{{/* One canonical payload for both the ConfigMap and its checksum. */}}
{{- define "agent-platform.hermesDefaults" -}}
{{- $config := deepCopy .Values.hermes.config -}}
{{- $profiles := deepCopy .Values.hermes.profiles -}}
{{- $defaults := dict "config" $config "profiles" $profiles -}}
{{- if .Values.workloads.hindsight.enabled -}}
  {{- if not (hasKey (default dict $config.memory) "provider") -}}
    {{- $memory := default dict $config.memory -}}
    {{- $_ := set $memory "provider" "hindsight" -}}
    {{- $_ := set $config "memory" $memory -}}
    {{- $_ := set $defaults "hindsight" (dict "mode" "local_external" "api_url" "http://hindsight:8888" "bank_id" "default") -}}
  {{- end -}}
  {{- range $profiles -}}
    {{- $memory := default dict .memory -}}
    {{- if not (hasKey $memory "provider") -}}
      {{- $_ := set $memory "provider" "" -}}
      {{- $_ := set . "memory" $memory -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- $defaults | toJson -}}
{{- end -}}

{{/* Enabled-only checks and wiring for the optional, externally backed API. */}}
{{- define "agent-platform.hindsight" -}}
{{- $root := .root -}}
{{- $api := .workloads.hindsight -}}
{{- if $api.enabled -}}
  {{- if not $root.Values.networkPolicy.enabled -}}
    {{- fail "workloads.hindsight.enabled requires networkPolicy.enabled: true because the Hindsight API has no authentication" -}}
  {{- end -}}
  {{- $settings := $root.Values.hindsight -}}
  {{- if not (regexMatch "@sha256:[a-f0-9]{64}$" $api.image) -}}
    {{- fail "workloads.hindsight.image must be digest-pinned" -}}
  {{- end -}}
  {{- $_ := required "hindsight.database.urlSecretRef.name is required when enabled" (trim $settings.database.urlSecretRef.name) -}}
  {{- $_ := required "hindsight.database.urlSecretRef.key is required when enabled" (trim $settings.database.urlSecretRef.key) -}}
  {{- $_ := required "hindsight.llm.provider is required when enabled" (trim $settings.llm.provider) -}}
  {{- $_ := required "hindsight.llm.model is required when enabled" (trim $settings.llm.model) -}}
  {{- if and $settings.llm.apiKeySecretRef.name (not (trim $settings.llm.apiKeySecretRef.key)) -}}
    {{- fail "hindsight.llm.apiKeySecretRef.key is required with a Secret name" -}}
  {{- end -}}
  {{- if and $settings.llm.baseUrl (not (regexMatch "^https?://[^/@?#]+(/[^@?#]*)?$" $settings.llm.baseUrl)) -}}
    {{- fail "hindsight.llm.baseUrl must be an HTTP URL without credentials, query or fragment" -}}
  {{- end -}}
  {{- $reserved := list "HINDSIGHT_API_DATABASE_URL" "HINDSIGHT_API_LLM_PROVIDER" "HINDSIGHT_API_LLM_MODEL" "HINDSIGHT_API_LLM_BASE_URL" "HINDSIGHT_API_LLM_API_KEY" "HINDSIGHT_API_WORKER_ENABLED" "HINDSIGHT_API_WORKER_ID" -}}
  {{- $hermesReserved := list "HINDSIGHT_MODE" "HINDSIGHT_BANK_ID" "HINDSIGHT_API_URL" -}}
  {{- range $name, $workload := dict "hindsight" $api "hermes" .workloads.hermes -}}
    {{- $names := $reserved -}}
    {{- if eq $name "hermes" -}}{{- $names = $hermesReserved -}}{{- end -}}
    {{- range $field := list "env" "secretEnv" -}}
      {{- range (index $workload $field) -}}
        {{- if has .name $names -}}
          {{- fail "Configure Hindsight through hindsight settings, not chart-owned workload environment overrides" -}}
        {{- end -}}
      {{- end -}}
    {{- end -}}
  {{- end -}}
  {{- $env := default list $api.env -}}
  {{- $cache := index $root.Values.persistence "hindsight-cache" -}}
  {{- $offlineConfigured := false -}}
  {{- range concat $env (default list $api.secretEnv) -}}
    {{- if eq .name "HF_HUB_OFFLINE" -}}{{- $offlineConfigured = true -}}{{- end -}}
  {{- end -}}
  {{- if and (not (or $cache.enabled $cache.existingClaim)) (not $offlineConfigured) -}}
    {{- $env = append $env (dict "name" "HF_HUB_OFFLINE" "value" "1") -}}
  {{- end -}}
  {{- range $name, $value := dict "HINDSIGHT_API_LLM_PROVIDER" $settings.llm.provider "HINDSIGHT_API_LLM_MODEL" $settings.llm.model "HINDSIGHT_API_WORKER_ENABLED" "true" "HINDSIGHT_API_WORKER_ID" "hindsight" -}}
    {{- $env = append $env (dict "name" $name "value" $value) -}}
  {{- end -}}
  {{- with $settings.llm.baseUrl -}}
    {{- $env = append $env (dict "name" "HINDSIGHT_API_LLM_BASE_URL" "value" .) -}}
  {{- end -}}
  {{- $_ := set $api "env" $env -}}
  {{- $secrets := append (default list $api.secretEnv) (dict "name" "HINDSIGHT_API_DATABASE_URL" "secretName" $settings.database.urlSecretRef.name "key" $settings.database.urlSecretRef.key) -}}
  {{- with $settings.llm.apiKeySecretRef.name -}}
    {{- $secrets = append $secrets (dict "name" "HINDSIGHT_API_LLM_API_KEY" "secretName" . "key" $settings.llm.apiKeySecretRef.key) -}}
  {{- end -}}
  {{- $_ := set $api "secretEnv" $secrets -}}
  {{- with $settings.database.caSecret -}}
    {{- $_ := set $api "volumes" (append (default list $api.volumes) (dict "name" "postgres-ca" "mountPath" "/etc/postgres" "source" (dict "secret" (dict "secretName" .)))) -}}
  {{- end -}}
  {{- if or $cache.enabled $cache.existingClaim -}}
    {{- $_ := set $api "persistence" (append (default list $api.persistence) (dict "name" "hindsight-cache" "mountPath" "/home/hindsight/.cache")) -}}
  {{- end -}}
  {{- $hermes := .workloads.hermes -}}
  {{- $env = default list $hermes.env -}}
  {{- /* No HINDSIGHT_BANK_ID: since Hindsight 1.x it overrides every home's own bank. */ -}}
  {{- range $name, $value := dict "HINDSIGHT_MODE" "local_external" "HINDSIGHT_API_URL" "http://hindsight:8888" -}}
    {{- $env = append $env (dict "name" $name "value" $value) -}}
  {{- end -}}
  {{- $_ := set $hermes "env" $env -}}
{{- end -}}
{{- end -}}
