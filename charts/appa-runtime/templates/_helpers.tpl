{{- define "appa-runtime.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "appa-runtime.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "appa-runtime.labels" -}}
helm.sh/chart: {{ include "appa-runtime.chart" . }}
app.kubernetes.io/name: {{ include "appa-runtime.name" . }}
app.kubernetes.io/instance: {{ .Release.Name | quote }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service | quote }}
{{- end -}}

{{- define "appa-runtime.selectorLabels" -}}
app.kubernetes.io/name: {{ include "appa-runtime.name" . }}
app.kubernetes.io/instance: {{ .Release.Name | quote }}
app: appa-runtime
{{- end -}}

{{- define "appa-runtime.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "appa-runtime.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.image" -}}
{{- $tag := .Values.image.tag | default (printf "v%s" .Chart.AppVersion) -}}
{{- if .Values.image.digest -}}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.testImage" -}}
{{- if .Values.test.image.digest -}}
{{- printf "%s:%s@%s" .Values.test.image.repository .Values.test.image.tag .Values.test.image.digest -}}
{{- else -}}
{{- printf "%s:%s" .Values.test.image.repository .Values.test.image.tag -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.configMapName" -}}
{{- .Values.config.existingConfigMap | default (printf "%s-policy" (include "appa-runtime.fullname" .)) -}}
{{- end -}}

{{- define "appa-runtime.pvcName" -}}
{{- .Values.persistence.existingClaim | default (printf "%s-data" (include "appa-runtime.fullname" .)) -}}
{{- end -}}

{{- define "appa-runtime.policy" -}}
{{- if .Values.config.contents -}}
{{- .Values.config.contents -}}
{{- else -}}
{{- .Files.Get "files/appa.toml" -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.packagedPolicyHash" -}}
{{- include "appa-runtime.policy" . | trim | sha256sum -}}
{{- end -}}

{{- define "appa-runtime.preserveLive" -}}
{{- $preserve := "" -}}
{{- if and .Release.IsUpgrade (not .Values.config.existingConfigMap) (not .Values.config.contents) -}}
{{- $live := lookup "v1" "ConfigMap" .Release.Namespace (include "appa-runtime.configMapName" .) -}}
{{- if and $live $live.data (hasKey $live.data .Values.config.key) -}}
{{- $data := index $live.data .Values.config.key -}}
{{- $ann := "" -}}
{{- if $live.metadata.annotations -}}
{{- $ann = index $live.metadata.annotations "appa.dev/packaged-policy-sha256" | default "" -}}
{{- end -}}
{{- $unmodifiedBootstrap := and (contains "# Bootstrap policy for the shared kagent runtime." $data) (not (contains "include =" $data)) -}}
{{- if $ann -}}
{{- if ne ($data | trim | sha256sum) $ann -}}
{{- $preserve = "true" -}}
{{- end -}}
{{- else if not $unmodifiedBootstrap -}}
{{- $preserve = "true" -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- $preserve -}}
{{- end -}}

{{- define "appa-runtime.managedPolicy" -}}
{{- if eq (include "appa-runtime.preserveLive" . | trim) "true" -}}
{{- $live := lookup "v1" "ConfigMap" .Release.Namespace (include "appa-runtime.configMapName" .) -}}
{{- index $live.data .Values.config.key -}}
{{- else -}}
{{- include "appa-runtime.policy" . -}}
{{- end -}}
{{- end -}}

{{- define "appa-runtime.policyHashAnnotation" -}}
{{- if eq (include "appa-runtime.preserveLive" . | trim) "true" -}}
{{- $live := lookup "v1" "ConfigMap" .Release.Namespace (include "appa-runtime.configMapName" .) -}}
{{- if $live.metadata.annotations -}}
{{- index $live.metadata.annotations "appa.dev/packaged-policy-sha256" | default "" -}}
{{- end -}}
{{- else -}}
{{- include "appa-runtime.packagedPolicyHash" . -}}
{{- end -}}
{{- end -}}
