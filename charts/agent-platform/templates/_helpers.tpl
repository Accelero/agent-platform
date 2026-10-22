{{- define "agent-platform.labels" -}}
app.kubernetes.io/part-of: agent-platform
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}
