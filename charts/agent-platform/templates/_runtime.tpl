{{/* Derive the native manager config and admission allowlist from one input. */}}
{{- define "agent-platform.runtime" -}}
{{- $settings := .Values.runtimeManager -}}
{{- if eq (include "agent-platform.executionNamespace" .) .Release.Namespace -}}
  {{- fail "Workers require a dedicated execution namespace, separate from the application namespace" -}}
{{- end -}}
{{- if and (index .Values.workloads "runtime-manager").enabled (not .Values.sandbox.enabled) -}}
  {{- fail "The runtime manager requires sandbox admission and network policies" -}}
{{- end -}}
{{- $config := deepCopy $settings.config -}}
{{- if not (hasKey $config.templates (default "hermes" $config.default_template)) -}}
  {{- fail "runtimeManager.config.default_template must name a configured template" -}}
{{- end -}}
{{- if or (hasKey $config "docker") (and (hasKey $config "backend") (ne $config.backend "kubernetes")) -}}
  {{- fail "runtimeManager.config supports only the Kubernetes backend" -}}
{{- end -}}
{{- $_ := set $config "backend" "kubernetes" -}}
{{- $_ := set $config "kubernetes" (dict "namespace" (include "agent-platform.executionNamespace" .) "image_pull_secrets" (include "agent-platform.workerPullSecrets" . | fromJsonArray)) -}}
{{- $clients := dict -}}
{{- range $name, $client := $settings.clients -}}
  {{- $_ := set $clients $name (dict "deployment" $client.deployment "token_file" (printf "/run/runtime-manager-auth/%s" $name)) -}}
{{- end -}}
{{- $_ := set $config "clients" $clients -}}
{{- $roles := dict -}}
{{- $identifier := "[A-Za-z0-9][A-Za-z0-9_.-]{0,255}" -}}
{{- $task := printf "%s/%s" $identifier $identifier -}}
{{- range $name, $template := $config.templates -}}
  {{- if or (hasKey $template "docker") (hasKey $template "runtime_root") -}}
    {{- fail "Runtime templates cannot use legacy Docker or host mount wiring" -}}
  {{- end -}}
  {{- $spec := $template.kubernetes -}}
  {{- $role := required "Each runtime template needs a Kubernetes role" $spec.role -}}
  {{- if hasKey $roles $role -}}{{- fail "Each runtime role must have exactly one template" -}}{{- end -}}
  {{- $image := default (index $.Values.images "agent-runtime") $template.image -}}
  {{- $_ := set $template "image" $image -}}
  {{- $_ := set $spec "uid" $settings.uid -}}
  {{- $_ := set $spec "gid" $settings.gid -}}
  {{- $_ := set $spec "service_account" $.Values.sandbox.workerServiceAccount -}}
  {{- $volumes := dict -}}
  {{- range $volume := $spec.volumes -}}
    {{- if hasKey $volumes $volume.name -}}{{- fail "Duplicate runtime volume name" -}}{{- end -}}
    {{- if or (ne (len (omit $volume "name")) 1) (not (or $volume.persistentVolumeClaim $volume.secret $volume.emptyDir)) -}}
      {{- if not (and (hasKey $volume "emptyDir") (eq (len (omit $volume "name")) 1)) -}}
        {{- fail "Runtime volumes must use selected PVCs, Secrets or emptyDir" -}}
      {{- end -}}
    {{- end -}}
    {{- $_ := set $volumes $volume.name $volume -}}
  {{- end -}}
  {{- $mounts := list -}}
  {{- range $mount := $spec.volume_mounts -}}
    {{- $source := required "Runtime mount references an undefined volume" (get $volumes $mount.name) -}}
    {{- if or $mount.subPathExpr (and $mount.mountPropagation (ne $mount.mountPropagation "None")) -}}
      {{- fail "Runtime mounts cannot use dynamic subpaths or mount propagation" -}}
    {{- end -}}
    {{- with $mount.subPath -}}
      {{- range $part := splitList "/" . -}}
        {{- if has $part (list "." ".." "") -}}{{- fail "Runtime mount subpaths must stay within their selected claim" -}}{{- end -}}
      {{- end -}}
    {{- end -}}
    {{- $rule := dict "mountPathPattern" (printf "^%s$" (regexQuoteMeta $mount.mountPath)) "readOnly" (default false $mount.readOnly) -}}
    {{- if $source.persistentVolumeClaim -}}
      {{- $_ := set $rule "claim" $source.persistentVolumeClaim.claimName -}}
      {{- with $mount.subPath -}}{{- $_ := set $rule "subPathPattern" (printf "^%s$" (regexQuoteMeta .)) -}}{{- end -}}
      {{- $mounts = append $mounts $rule -}}
    {{- else if $source.secret -}}
      {{- if or (not $source.secret.items) (not $rule.readOnly) -}}{{- fail "Runtime Secrets require selected keys and read-only mounts" -}}{{- end -}}
      {{- $keys := list -}}
      {{- range $item := $source.secret.items -}}
        {{- if or (not $item.key) (not $item.path) (has $item.key $keys) (contains "/" $item.path) (has $item.path (list "." "..")) -}}
          {{- fail "Runtime Secret items require unique keys and safe file names" -}}
        {{- end -}}
        {{- $keys = append $keys $item.key -}}
      {{- end -}}
      {{- $_ := set $rule "secret" $source.secret.secretName -}}
      {{- $_ := set $rule "keys" $keys -}}
      {{- with $mount.subPath -}}{{- $_ := set $rule "subPath" . -}}{{- end -}}
      {{- $mounts = append $mounts $rule -}}
    {{- end -}}
  {{- end -}}
  {{- with $scope := $template.workdir_scope -}}
    {{- if or (hasKey $scope "host_root") (hasKey $scope "kubernetes_root") -}}{{- fail "Runtime workdirs require task-only PVCs" -}}{{- end -}}
    {{- $prefix := "" -}}
    {{- with $scope.kubernetes_prefix -}}{{- $prefix = regexQuoteMeta (printf "%s/" (trimSuffix "/" .)) -}}{{- end -}}
    {{- $root := regexQuoteMeta (trimSuffix "/" $scope.controller_root) -}}
    {{- $mounts = append $mounts (dict
      "claim" $scope.kubernetes_claim
      "subPathPattern" (printf "^%s%s/workdir$" $prefix $task)
      "mountPathPattern" (printf "^%s/%s/workdir$" $root $task)
      "readOnly" $scope.read_only) -}}
    {{- with $scope.identity_kubernetes_claim -}}
      {{- $identity := dict "claim" . "mountPathPattern" (printf "^%s/%s/hermes-home$" $root $task) "readOnly" false -}}
      {{- with $scope.identity_kubernetes_subpath -}}{{- $_ := set $identity "subPathPattern" (printf "^%s$" (regexQuoteMeta .)) -}}{{- end -}}
      {{- $mounts = append $mounts $identity -}}
    {{- end -}}
  {{- end -}}
  {{- with $scope := $template.profile_workspace -}}
    {{- $segment := "\\.*[A-Za-z0-9_-][A-Za-z0-9_.-]{0,255}" -}}
    {{- $subdir := printf "%s(?:/%s)*" $segment $segment -}}
    {{- range $root := list $scope.mount_path $scope.controller_root -}}
      {{- $mounts = append $mounts (dict
        "claim" $scope.kubernetes_claim
        "subPathPattern" (printf "^%s$" (regexQuoteMeta $scope.kubernetes_subpath))
        "mountPathPattern" (printf "^%s$" (regexQuoteMeta $root))
        "readOnly" false) -}}
      {{- $mounts = append $mounts (dict
        "claim" $scope.kubernetes_claim
        "subPathPattern" (printf "^%s/%s$" (regexQuoteMeta $scope.kubernetes_subpath) $subdir)
        "mountPathPattern" (printf "^%s/%s$" (regexQuoteMeta (trimSuffix "/" $root)) $subdir)
        "readOnly" false) -}}
    {{- end -}}
  {{- end -}}
  {{- with $root := $template.runtime_guest_root -}}
    {{- $guest := regexQuoteMeta (trimSuffix "/" $root) -}}
    {{- $supported := dict
      ".multica/workspaces" (dict
        "subpath" $task "target" (printf "%s/\\.multica/workspaces/%s" $guest $task))
      "environments" (dict
        "subpath" (printf "%s/home" $identifier) "target" "/home/worker")
      ".codex/multica-sessions" (dict
        "subpath" (printf "%s/%s" $task $identifier)
        "target" (printf "%s/\\.codex/multica-sessions/%s/%s" $guest $task $identifier)) -}}
    {{- $paths := list -}}{{- $claims := list -}}
    {{- range $source := $template.runtime_kubernetes_mounts -}}
      {{- if or (not (hasKey $supported $source.path)) (has $source.path $paths) (has $source.claim $claims) -}}{{- fail "Runtime task, home and history storage require three separate selected claims" -}}{{- end -}}
      {{- $paths = append $paths $source.path -}}{{- $claims = append $claims $source.claim -}}
      {{- $rule := get $supported $source.path -}}
      {{- $prefix := "" -}}
      {{- with $source.prefix -}}{{- $prefix = regexQuoteMeta (printf "%s/" (trimSuffix "/" .)) -}}{{- end -}}
      {{- $mounts = append $mounts (dict
        "claim" $source.claim
        "subPathPattern" (printf "^%s%s$" $prefix $rule.subpath)
        "mountPathPattern" (printf "^%s$" $rule.target)
        "readOnly" false) -}}
    {{- end -}}
    {{- if ne (len $paths) 3 -}}{{- fail "Runtime task, home and history storage require three separate selected claims" -}}{{- end -}}
  {{- end -}}
  {{- $_ := set $roles $role (dict "mounts" $mounts "environment" (default dict $spec.environment)) -}}
{{- end -}}
{{- dict "config" $config "roles" $roles | toJson -}}
{{- end -}}
