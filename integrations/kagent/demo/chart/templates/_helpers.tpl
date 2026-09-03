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
The demo policy with the agent-tool names kagent dispatches:
<namespace>__NS__<agent>, hyphens as underscores. The names check
establishes both child values. The model profile the sanitizers consult
comes from llm.model and llm.url. An empty llm.url drops the url line,
which leaves the runtime on the OpenAI default endpoint.
*/}}
{{- define "appa-demo.policy" -}}
{{- include "appa-demo.requireDistinctAgentNames" . -}}
{{- $ns := .Release.Namespace | replace "-" "_" -}}
{{- $child := .Values.agents.childName | replace "-" "_" -}}
{{- $childGo := .Values.agents.go.childName | replace "-" "_" -}}
{{- $llmModel := required "llm.model is required: the model the policy's sanitizers consult" .Values.llm.model -}}
{{- $llm := printf "model = %q\n" $llmModel -}}
{{- if .Values.llm.url -}}
{{- $llm = printf "%surl = %q\n" $llm .Values.llm.url -}}
{{- end -}}
{{- /* The file on disk is a loadable policy carrying these defaults, so
       CI opens it (appa-runtime/tests/examples_load.rs). Substitute the
       defaults, longest agent name first so the plain one cannot match
       inside the go one. */ -}}
{{- .Files.Get "files/demo.appa.toml" | replace "kagent__NS__log_analyst_go" (printf "%s__NS__%s" $ns $childGo) | replace "kagent__NS__log_analyst" (printf "%s__NS__%s" $ns $child) | replace "model = \"gpt-4.1-mini\"\n" $llm -}}
{{- end -}}
