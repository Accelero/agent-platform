{{- define "agent-platform.labels" -}}
app.kubernetes.io/part-of: agent-platform
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Keep each application's workers and namespace permissions within its deployment. */}}
{{- define "agent-platform.executionNamespace" -}}
{{- if .Values.sandbox.namespace -}}
  {{- .Values.sandbox.namespace -}}
{{- else if le (len .Release.Namespace) 53 -}}
  {{- printf "%s-execution" .Release.Namespace -}}
{{- else -}}
  {{- printf "%s-%s-execution" (trimSuffix "-" (trunc 44 .Release.Namespace)) (trunc 8 (sha256sum .Release.Namespace)) -}}
{{- end -}}
{{- end -}}
