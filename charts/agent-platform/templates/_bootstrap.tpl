{{- define "agent-platform.bootstrapPod" -}}
{{- $b := .Values.bootstrap }}
metadata:
  labels:
    app.kubernetes.io/name: agent-state
    app.kubernetes.io/part-of: agent-platform-bootstrap
spec:
  serviceAccountName: {{ default (printf "%s-bootstrap" .Release.Name) $b.existingServiceAccount }}
  automountServiceAccountToken: true
  enableServiceLinks: false
  restartPolicy: Never
  securityContext:
    runAsNonRoot: true
    runAsUser: 1000
    runAsGroup: 1000
    fsGroup: 1000
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: bootstrap
      image: {{ required "bootstrap requires images.tools" .Values.images.tools | quote }}
      imagePullPolicy: {{ .Values.imagePullPolicy }}
      command: [python, -m, platform_bootstrap]
      args: []
      env:
        - name: POD_UID
          valueFrom:
            fieldRef: {fieldPath: metadata.uid}
        - name: MULTICA_POSTGRES_PASSWORD
          valueFrom:
            secretKeyRef:
              name: {{ .Values.existingSecret }}
              key: MULTICA_POSTGRES_PASSWORD
        {{- with $b.env }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
      {{- with $b.envFrom }}
      envFrom: {{ toYaml . | nindent 8 }}
      {{- end }}
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities: {drop: [ALL]}
      resources:
        requests: {cpu: 100m, memory: 256Mi}
        limits: {cpu: '1', memory: 1Gi}
      volumeMounts:
        - {name: bootstrap-config, mountPath: /etc/platform-bootstrap, readOnly: true}
        - {name: bootstrap-home, mountPath: /opt/data}
        {{- if .Values.multica.databaseCASecret }}
        - {name: bootstrap-postgres-ca, mountPath: /etc/postgres, readOnly: true}
        {{- end }}
        {{- with $b.volumeMounts }}
        {{- toYaml . | nindent 8 }}
        {{- end }}
  volumes:
    - name: bootstrap-config
      configMap: {name: {{ .Release.Name }}-bootstrap}
    - name: bootstrap-home
      persistentVolumeClaim:
        claimName: {{ default "hermes-home" (index .Values.persistence "hermes-home").existingClaim }}
    {{- if .Values.multica.databaseCASecret }}
    - name: bootstrap-postgres-ca
      secret: {secretName: {{ .Values.multica.databaseCASecret }}}
    {{- end }}
    {{- with $b.volumes }}
    {{- toYaml . | nindent 4 }}
    {{- end }}
{{- end -}}
