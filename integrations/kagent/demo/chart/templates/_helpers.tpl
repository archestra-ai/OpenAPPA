{{/* An image reference; an empty tag means the chart's appVersion. */}}
{{- define "appa-demo.image" -}}
{{- printf "%s:%s" .image.repository (.image.tag | default .root.Chart.AppVersion) -}}
{{- end -}}

{{/* The Secret carrying OPENAI_API_KEY: the operator's, or the one named after the ModelConfig. */}}
{{- define "appa-demo.secretName" -}}
{{- .Values.openai.existingSecret | default .Values.modelConfig.name -}}
{{- end -}}

{{/* Where every agent reaches the shared runtime: the relay's Service. */}}
{{- define "appa-demo.runtimeUrl" -}}
{{- printf "http://appa-runtime.%s.svc.cluster.local:18789" .Release.Namespace -}}
{{- end -}}

{{- define "appa-demo.controllerUrl" -}}
{{- .Values.seed.controllerUrl | default (printf "http://kagent-controller.%s.svc.cluster.local:8083" .Release.Namespace) -}}
{{- end -}}

{{- define "appa-demo.labels" -}}
app.kubernetes.io/part-of: appa-kagent-demo
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
