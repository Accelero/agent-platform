{{/* Keep internal service wiring and native environment configuration in Helm. */}}
{{- define "agent-platform.workloads" -}}
{{- $workloads := deepCopy .Values.workloads -}}
{{- $multica := .Values.multica -}}
{{- $backend := index $workloads "multica-backend" -}}
{{- $env := list -}}
{{- range $backend.env -}}
  {{- if not (has .name (list "DATABASE_URL" "FRONTEND_ORIGIN" "CORS_ALLOWED_ORIGINS" "MULTICA_APP_URL" "ALLOWED_EMAILS" "ALLOW_SIGNUP" "MULTICA_MAINTENANCE_FILE")) -}}{{- $env = append $env . -}}{{- end -}}
{{- end -}}
{{- range $name, $value := dict "DATABASE_URL" $multica.databaseUrl "FRONTEND_ORIGIN" $multica.publicUrl "CORS_ALLOWED_ORIGINS" $multica.publicUrl "MULTICA_APP_URL" $multica.publicUrl "ALLOWED_EMAILS" $multica.ownerEmail "ALLOW_SIGNUP" "false" "MULTICA_MAINTENANCE_FILE" $multica.maintenanceFile -}}
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
{{- $workloads | toJson -}}
{{- end -}}
