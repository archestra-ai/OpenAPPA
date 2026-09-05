{{/* An image reference; an empty tag means the chart's appVersion. */}}
{{- define "appa-demo.image" -}}
{{- printf "%s:%s" .image.repository (.image.tag | default .root.Chart.AppVersion) -}}
{{- end -}}

{{/* The existing shared runtime Service used by every demo Agent. */}}
{{- define "appa-demo.runtimeUrl" -}}
{{- required "runtime.url is required: the existing appa-runtime Service URL" .Values.runtime.url -}}
{{- end -}}

{{- define "appa-demo.controllerUrl" -}}
{{- .Values.seed.controllerUrl | default (printf "http://kagent-controller.%s.svc.cluster.local:8083" .Release.Namespace) -}}
{{- end -}}

{{- define "appa-demo.labels" -}}
app.kubernetes.io/part-of: appa-kagent-demo
app.kubernetes.io/managed-by: {{ .Release.Service | quote }}
app.kubernetes.io/instance: {{ .Release.Name | quote }}
{{- end -}}

{{/*
The agent names one release consumes, checked present and distinct.
The set is cluster-ops, release-manager, agents.childName and
agents.go.childName (the policy declares both children even without the
go cell), plus agents.go.name and agents.go.undeclaredName when
agents.go.enabled. Two agents under one name would merge into one Agent
object, and the policy could then declare the agent it must never name.
Fails the render on a missing or repeated name. Renders nothing.
*/}}
{{- define "appa-demo.requireDistinctAgentNames" -}}
{{- $names := list "cluster-ops" "release-manager" -}}
{{- $names = append $names (required "agents.childName is required: the python child the policy declares" .Values.agents.childName) -}}
{{- $names = append $names (required "agents.go.childName is required: the go child the policy declares" .Values.agents.go.childName) -}}
{{- if .Values.agents.go.enabled -}}
{{- $names = append $names (required "agents.go.name is required: the go twin of cluster-ops" .Values.agents.go.name) -}}
{{- $names = append $names (required "agents.go.undeclaredName is required: the go agent the policy never names" .Values.agents.go.undeclaredName) -}}
{{- end -}}
{{- $seen := dict -}}
{{- range $names -}}
{{- if hasKey $seen . -}}
{{- fail (printf "agent names collide: %q names two agents of this release. cluster-ops, release-manager, agents.childName, agents.go.childName and, with agents.go.enabled, agents.go.name and agents.go.undeclaredName must be distinct." .) -}}
{{- end -}}
{{- $_ := set $seen . true -}}
{{- end -}}
{{- end -}}

{{/*
The inert demo policy template with the agent-tool names kagent dispatches:
<namespace>__NS__<agent>, hyphens as underscores. The names check
establishes both child values. Its local command adapters forward consults
to the demo mock Service in this release namespace. appa-guide copies the
approved template into the policy ConfigMap owned by appa-runtime.
*/}}
{{- define "appa-demo.policy" -}}
{{- include "appa-demo.requireDistinctAgentNames" . -}}
{{- $ns := .Release.Namespace | replace "-" "_" -}}
{{- $child := .Values.agents.childName | replace "-" "_" -}}
{{- $childGo := .Values.agents.go.childName | replace "-" "_" -}}
{{- $mock := printf "http://appa-demo-mocks.%s.svc.cluster.local:8081" .Release.Namespace -}}
{{- /* Substitute the longest agent name first so the plain one cannot
       match inside the go one. The file defaults stay loadable in CI. */ -}}
{{- .Files.Get "files/demo.appa.toml" | replace "kagent__NS__log_analyst_go" (printf "%s__NS__%s" $ns $childGo) | replace "kagent__NS__log_analyst" (printf "%s__NS__%s" $ns $child) | replace "http://appa-demo-mocks.kagent.svc.cluster.local:8081" $mock -}}
{{- end -}}
