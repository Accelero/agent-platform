{{/* Translate one dashboard authentication choice into native Hermes settings. */}}
{{- define "agent-platform.dashboard" -}}
{{- $auth := .root.Values.hermes.dashboardAuth -}}
{{- $hermes := .workload -}}
{{- $oidc := eq $auth.mode "oidc" -}}
{{- $env := list -}}
{{- $secrets := list -}}
{{- range $field := list "env" "secretEnv" -}}
  {{- $entries := list -}}
  {{- range (index $hermes $field) -}}
    {{- if or (hasPrefix "HERMES_DASHBOARD_OIDC_" .name) (eq .name "HERMES_DASHBOARD_PUBLIC_URL") -}}
      {{- fail "Configure Hermes OIDC through hermes.dashboardAuth, not workload environment overrides" -}}
    {{- end -}}
    {{- if and (ne .name "HERMES_DASHBOARD_BASIC_AUTH_USERNAME") (not (and $oidc (hasPrefix "HERMES_DASHBOARD_BASIC_AUTH_" .name))) -}}
      {{- $entries = append $entries . -}}
    {{- end -}}
  {{- end -}}
  {{- if eq $field "env" -}}{{- $env = $entries -}}{{- else -}}{{- $secrets = $entries -}}{{- end -}}
{{- end -}}
{{- if $oidc -}}
  {{- range $name, $value := dict "HERMES_DASHBOARD_OIDC_ISSUER" $auth.oidc.issuer "HERMES_DASHBOARD_OIDC_CLIENT_ID" $auth.oidc.clientId "HERMES_DASHBOARD_PUBLIC_URL" $auth.oidc.publicUrl "HERMES_DASHBOARD_OIDC_SCOPES" $auth.oidc.scopes -}}
    {{- $env = append $env (dict "name" $name "value" $value) -}}
  {{- end -}}
  {{- with $auth.oidc.clientSecretRef.name -}}
    {{- $secrets = append $secrets (dict "name" "HERMES_DASHBOARD_OIDC_CLIENT_SECRET" "secretName" . "key" $auth.oidc.clientSecretRef.key) -}}
  {{- end -}}
{{- else -}}
  {{- $env = append $env (dict "name" "HERMES_DASHBOARD_BASIC_AUTH_USERNAME" "value" .root.Values.hermes.dashboardUsername) -}}
{{- end -}}
{{- $_ := set $hermes "env" $env -}}
{{- $_ := set $hermes "secretEnv" $secrets -}}
{{- end -}}
