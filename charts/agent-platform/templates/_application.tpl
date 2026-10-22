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
{{- $workloads | toJson -}}
{{- end -}}
