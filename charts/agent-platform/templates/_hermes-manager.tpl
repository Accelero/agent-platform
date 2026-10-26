{{- /*
Installation-wide names derived once from the hermesManager values, as JSON.
The controller renders instances from the same inputs, so every name here
matches hermes-render.
*/}}
{{- define "hermes-manager.model" -}}
{{- $m := .Values.hermesManager -}}
{{- $id := required "hermesManager.installation.id is required" $m.installation.id -}}
{{- if not (regexMatch "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$" $id) -}}
{{- fail "hermesManager.installation.id must be a lowercase UUID" -}}
{{- end -}}
{{- $issuer := required "hermesManager.installation.issuer is required" $m.installation.issuer -}}
{{- if not (hasPrefix "https://" $issuer) -}}{{- fail "hermesManager.installation.issuer must be an HTTPS URL" -}}{{- end -}}
{{- $short := substr 0 12 (replace "-" "" $id) -}}
{{- $base := printf "hermes-manager-%s" $short -}}
{{- $account := $m.controller.serviceAccountName -}}
{{- $role := default (printf "%s-instance" $base) $m.controller.instanceRole -}}
{{- $sources := list -}}
{{- $ids := dict -}}
{{- range $m.storageSources -}}
{{- if hasKey $ids .id -}}{{- fail (printf "hermesManager.storageSources: duplicate id %s" .id) -}}{{- end -}}
{{- $_ := set $ids .id true -}}
{{- $sources = append $sources . -}}
{{- end -}}
{{- $peer := dict "app.kubernetes.io/instance" .Release.Name -}}
{{- dict
  "installationId" $id
  "issuer" $issuer
  "base" $base
  "namespacePrefix" (printf "hi-%s-" $short)
  "namespace" .Release.Namespace
  "serviceAccount" $account
  "controllerUser" (printf "system:serviceaccount:%s:%s" .Release.Namespace $account)
  "instanceRole" $role
  "storageSources" $sources
  "dataIdentity" $m.dataIdentity
  "peers" (dict
    "api" (merge (dict "app.kubernetes.io/name" "hermes-manager-api") $peer)
    "controller" (merge (dict "app.kubernetes.io/name" "hermes-manager-controller") $peer)
    "gateway" (merge (dict "app.kubernetes.io/name" "hermes-gateway") $peer))
  | toJson -}}
{{- end -}}

{{- /* A session keyring naming one key file per ID in the role's Secret. */}}
{{- define "hermes-manager.keyring" -}}
{{- $keys := .keys -}}
{{- if or (not $keys.active) (not $keys.ids) (not (has $keys.active $keys.ids)) -}}
{{- fail (printf "%s needs an active key among its ids" .field) -}}
{{- end -}}
{{- $files := dict -}}
{{- range $keys.ids -}}{{- $_ := set $files . (printf "%s/session-key-%s" $.dir .) -}}{{- end -}}
{{- dict "activeKeyId" $keys.active "keyFiles" $files | toJson -}}
{{- end -}}
