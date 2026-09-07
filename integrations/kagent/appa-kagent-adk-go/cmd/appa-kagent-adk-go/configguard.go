// The rendered-config guard.
//
// The stock loader drops a key it does not know and ignores a value
// the Go runtime does not implement. Either way a narrower agent than
// the operator declared runs. A config compiled for a wider kagent
// schema can carry in-process sub-agents or agent plugins that this
// image cannot represent. A config inside the rc4 schema can still
// turn on code execution or set a context config, and the Go runtime
// builds neither. The image cannot gate what it does not build. So the
// guard reads the raw file once, before anything runs it. It refuses
// the start on any top-level key outside the rc4 adk.AgentConfig
// schema. It decodes the bytes it checked through the stock decoder.
// It then refuses the start on a tool declared under a name OpenAPPA
// owns, and on any value the Go runtime would ignore. The config it
// returns is the config the runtime runs. Nothing reads the file a
// second time.
package main

import (
	"encoding/json"
	"fmt"
	"reflect"
	"sort"
	"strings"

	appakagentadk "github.com/archestra-ai/OpenAPPA/integrations/kagent/appa-kagent-adk-go"
	"github.com/kagent-dev/kagent/go/api/adk"
)

// configRefusalKind names why the guard refuses a config.
type configRefusalKind int

const (
	// notAnObject: the file does not decode as one JSON object.
	notAnObject configRefusalKind = iota
	// inProcessFeature: a named key that a wider kagent schema compiles
	// for a feature this image cannot represent.
	inProcessFeature
	// outsideSchema: top-level keys the rc4 schema does not carry.
	outsideSchema
	// codeExecution: execute_code is true, and the Go runtime builds no
	// code executor.
	codeExecution
	// contextCompaction: context_config is set, and the Go runtime runs
	// no context compaction.
	contextCompaction
	// reservedToolName: a declared tool carries an APPA-owned name.
	reservedToolName
)

// configRefusal is the guard's error: which refusal fired and the keys
// it names. The outsideSchema kind lists its keys sorted. The
// inProcessFeature kind names one key and the feature it compiles. A
// value kind names the one key whose value the Go runtime would ignore.
type configRefusal struct {
	kind configRefusalKind
	keys []string
	// feature names what the key compiles, for inProcessFeature.
	feature string
	// detail carries the decoder's own message for notAnObject.
	detail string
}

func (r *configRefusal) Error() string {
	switch r.kind {
	case notAnObject:
		return "config.json is not one JSON object: " + r.detail
	case inProcessFeature:
		return fmt.Sprintf("config.json carries %s: a config compiled for a kagent that ships in-process %s reached this image. "+
			"This image runs the rc4 schema and cannot represent them.", r.keys[0], r.feature)
	case codeExecution:
		return "config.json sets execute_code to true, and this image builds no code executor. " +
			"It cannot run the config as declared."
	case contextCompaction:
		return "config.json sets context_config, and this image runs no context compaction. " +
			"It cannot run the config as declared."
	case reservedToolName:
		return fmt.Sprintf("config.json declares a tool named %s at %s, and OpenAPPA owns that name. Rename the tool.",
			r.feature, r.keys[0])
	default:
		return "config.json carries top-level fields outside this image's rc4 schema, and the runtime does not run " +
			"what it cannot gate: " + strings.Join(r.keys, ", ")
	}
}

// namedRefusal is one key the guard refuses by name and the feature
// that key compiles.
type namedRefusal struct {
	key     string
	feature string
}

// namedRefusals lists the keys the guard refuses by name, in the order
// it checks them. They run before the schema check on purpose. A kagent
// module bump that adds one of them to adk.AgentConfig must still refuse
// here. It must not pass as a known key.
var namedRefusals = []namedRefusal{
	{key: "sub_agents", feature: "sub-agents"},
	{key: "agent_plugins", feature: "agent plugins"},
}

// knownTopLevelKeys is the set of top-level json tags of the rc4
// adk.AgentConfig. Reflection reads it, so the set follows the pinned
// kagent module. AgentConfig.UnmarshalJSON decodes through a temporary
// struct with the same tags, so this set is exactly what the stock
// decoder reads.
//
// Keys match exactly. encoding/json folds case when it decodes, so the
// stock decoder also accepts Instruction for instruction. This guard
// does not: kagent's compiler emits the lower-case tags, and the
// python image matches exactly too.
var knownTopLevelKeys = topLevelJSONKeys(reflect.TypeOf(adk.AgentConfig{}))

// topLevelJSONKeys collects the json names of a struct's fields. A
// field without a json name is not a config key the guard accepts.
func topLevelJSONKeys(t reflect.Type) map[string]struct{} {
	keys := make(map[string]struct{}, t.NumField())
	for i := 0; i < t.NumField(); i++ {
		name, _, _ := strings.Cut(t.Field(i).Tag.Get("json"), ",")
		if name == "" || name == "-" {
			continue
		}
		keys[name] = struct{}{}
	}
	return keys
}

// decodeGuarded is the guard's one entry. It refuses the raw
// config.json on a top-level key this image cannot represent. It
// decodes the same bytes through the stock decoder. It then refuses
// the decoded config on a declared tool that carries an APPA-owned
// name, and then on a value the Go runtime would ignore. It returns
// the decoded config for a config it accepts. A refusal is a
// *configRefusal. A decode failure is the stock decoder's own error,
// wrapped as the stock loader wraps it.
func decodeGuarded(raw []byte) (*adk.AgentConfig, error) {
	if err := refuseUnsupported(raw); err != nil {
		return nil, err
	}
	var agentConfig adk.AgentConfig
	if err := json.Unmarshal(raw, &agentConfig); err != nil {
		return nil, fmt.Errorf("failed to parse config file: %w", err)
	}
	if err := refuseReservedToolNames(&agentConfig); err != nil {
		return nil, err
	}
	if err := refuseIgnoredValues(&agentConfig); err != nil {
		return nil, err
	}
	return &agentConfig, nil
}

// refuseUnsupported refuses a raw config.json whose top-level keys this
// image cannot represent. It returns nil for a JSON object whose
// top-level keys are all rc4 schema keys.
//
// The key check walks the top-level object only. A nested unknown key
// is out of scope: the stock decoder drops it, and this image accepts
// that. The features this image must refuse by key land as top-level
// keys.
func refuseUnsupported(raw []byte) error {
	var top map[string]json.RawMessage
	if err := json.Unmarshal(raw, &top); err != nil {
		return &configRefusal{kind: notAnObject, detail: err.Error()}
	}
	if top == nil {
		// A JSON null decodes into a nil map with no error.
		return &configRefusal{kind: notAnObject, detail: "the document is null"}
	}
	for _, named := range namedRefusals {
		if _, present := top[named.key]; present {
			return &configRefusal{kind: inProcessFeature, keys: []string{named.key}, feature: named.feature}
		}
	}
	var unknown []string
	for key := range top {
		if _, known := knownTopLevelKeys[key]; !known {
			unknown = append(unknown, key)
		}
	}
	if len(unknown) > 0 {
		sort.Strings(unknown)
		return &configRefusal{kind: outsideSchema, keys: unknown}
	}
	return nil
}

// refuseReservedToolNames refuses a decoded config that declares a
// tool under a name OpenAPPA owns. Today that is appa_return, the tool
// a child scope returns through: the plugin registers it on every model
// request of a child scope and replaces the child's final message with
// one call to it.
//
// The plugin holds that name at dispatch — it registers its own gate
// over the slot and recognizes the gate by identity, so a foreign tool
// of that name is gated like every other tool. This refusal catches the
// collision earlier, at the start, where an operator can read it and
// rename the tool. It reads the three places a rendered config names a
// tool: the tool filter of each MCP toolset, and the name of each
// remote agent.
//
// A toolset with an empty filter names no tool here, and the MCP server
// behind it can still advertise the name. That case is the plugin's,
// not the guard's.
func refuseReservedToolNames(agentConfig *adk.AgentConfig) error {
	for index, server := range agentConfig.HttpTools {
		if at, name := reservedToolAt("http_tools", index, server.Tools); at != "" {
			return &configRefusal{kind: reservedToolName, keys: []string{at}, feature: name}
		}
	}
	for index, server := range agentConfig.SseTools {
		if at, name := reservedToolAt("sse_tools", index, server.Tools); at != "" {
			return &configRefusal{kind: reservedToolName, keys: []string{at}, feature: name}
		}
	}
	for index, remoteAgent := range agentConfig.RemoteAgents {
		if isReservedToolName(remoteAgent.Name) {
			return &configRefusal{
				kind:    reservedToolName,
				keys:    []string{fmt.Sprintf("remote_agents[%d].name", index)},
				feature: remoteAgent.Name,
			}
		}
	}
	return nil
}

// isReservedToolName reports whether a declared tool takes a name APPA
// owns. The plugin registers its return gate under one of them in a
// child scope, and the runtime main appends the reserved toolset under
// the other after this guard runs. Each is recognized by identity, so a
// declared tool of either name is gated like any other tool — but the
// model would read two declarations of one name, and which one answers
// is the builder's order rather than the policy's.
func isReservedToolName(name string) bool {
	if name == appakagentadk.ReturnTool {
		return true
	}
	for _, runtimeTool := range appakagentadk.RuntimeTools {
		if name == runtimeTool {
			return true
		}
	}
	return false
}

// reservedToolAt names the position and the spelling of the first
// APPA-owned name in one toolset's tool filter, or "", "" when the
// filter names none.
func reservedToolAt(key string, index int, tools []string) (string, string) {
	for position, name := range tools {
		if isReservedToolName(name) {
			return fmt.Sprintf("%s[%d].tools[%d]", key, index, position), name
		}
	}
	return "", ""
}

// refuseIgnoredValues refuses a decoded config on a value the Go
// runtime would ignore. The rule is plain: it refuses execute_code
// true and every non-null context_config, the empty object included.
// The stock runtime builds no code executor and no context
// compaction, so it would run the agent without them. kagent's
// controller warns on the same two features and renders them anyway.
// Its context warning covers the compaction case only, so this guard
// refuses a wider set than the controller reports. The checks run in
// schema order and the first refusal wins. An absent, null or false
// execute_code and a null or absent context_config pass.
//
// The network key is not in the set. Neither runtime reads it from
// config.json at the pinned versions. The controller renders the same
// allowlist into srt-settings.json. The Go skills shell applies it
// from there, as the python image does. A network allowlist passes.
func refuseIgnoredValues(agentConfig *adk.AgentConfig) error {
	if agentConfig.GetExecuteCode() {
		return &configRefusal{kind: codeExecution, keys: []string{"execute_code"}}
	}
	if agentConfig.ContextConfig != nil {
		return &configRefusal{kind: contextCompaction, keys: []string{"context_config"}}
	}
	return nil
}
