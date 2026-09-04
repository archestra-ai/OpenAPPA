// The tool inventory: every name ADK can dispatch on this agent, and
// its spelling on the wire.
//
// The plugin gates a tool under a structured spelling, never under the
// bare name ADK dispatches it by: an MCP tool as mcp:<toolset>/<tool>,
// a remote agent as agent:<namespace>/<agent>, a tool the kagent
// runtime attaches itself as builtin:<name>, an out-of-band flow the
// runtime main gates as gate:<name>, and the runtime's own control
// tool as appa:execute_remedy_plan. The runtime derives the canonical
// tool and whether the call is a spawn from that spelling.
//
// The runtime main builds the inventory once at startup from the
// rendered config, so what the wire can name is fixed before the model
// runs. A call of a name outside it is refused at the gate, never
// forwarded.
//
// The inverse travels with it. The runtime names a tool back to the
// model by the spelling it received, which is not a name the model can
// call, so Despell spells it into the name ADK dispatches. The builder
// owns both directions and refuses a config whose two raw names spell
// alike, so every spelling the wire carries names one tool the model
// can call.
//
//   - An MCP entry names its tools in its tool filter, and a gated agent
//     must carry one: without it the server decides the tool list at
//     runtime, and the gate cannot name what it did not see. The toolset
//     is the first DNS label of the server host in the entry's URL, the
//     name the RemoteMCPServer resource carries in the cluster.
//   - kagent renders a remote agent's tool name as
//     <namespace>__NS__<agent> with hyphens as underscores. Both halves
//     are DNS-1123 labels, which carry no underscore, so the real names
//     come back exactly. The rendering is not injective over every name
//     a config can carry — team_a__NS__x and team-a__NS__x spell alike —
//     and the builder refuses the pair rather than lose one.
//   - The builtins come from builtins.json, the manifest pinned to the
//     kagent go module this image wraps, in groups the rendered config
//     and the runtime's environment switch on.

package appakagentadk

import (
	_ "embed"
	"encoding/json"
	"fmt"
	"net/url"
	"regexp"
	"strings"
)

//go:embed builtins.json
var builtinsManifest []byte

// ManifestLane is this image's key in the shared builtin manifest.
const ManifestLane = "go"

// ControlTool is the reserved tool's spelling on the wire.
const ControlTool = "appa:execute_remedy_plan"

const namespaceMark = "__NS__"

// segment is one segment of a canonical tool id, as the runtime admits it.
var segment = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)

// spelledCandidate matches a wire spelling as it stands in a runtime
// string: two or three segments, class:name or class:namespace/name.
// Each run of segment characters is maximal, so a spelling inside a
// longer run is no candidate here, and the inventory alone decides
// which candidate is a spelling it gave out.
var spelledCandidate = regexp.MustCompile(`[A-Za-z0-9_.-]+:[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)?`)

// spellingEdge holds the characters that continue a wire spelling. A
// candidate that follows one is the tail of a longer run, not a whole
// spelling, and it stands.
const spellingEdge = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.:/-"

func MCPSpelling(toolset, tool string) string      { return "mcp:" + toolset + "/" + tool }
func AgentSpelling(namespace, agent string) string { return "agent:" + namespace + "/" + agent }
func BuiltinSpelling(name string) string           { return "builtin:" + name }

// IsSpawn reports whether a spelled tool runs another agent: the agent: class.
func IsSpawn(spelling string) bool { return strings.HasPrefix(spelling, "agent:") }

// BuiltinManifest is the packaged builtin manifest, every lane included.
func BuiltinManifest() []byte { return builtinsManifest }

// Inventory maps the raw ADK tool names of one agent to their wire
// spellings. The zero value spells nothing, so every call is refused.
type Inventory struct {
	spellings map[string]string
	// names is the inverse: each wire spelling, back to the name ADK
	// dispatches. BuildInventory is the only builder, and it fills both
	// directions as one: a spelling is assigned to at most one raw name,
	// so the inverse loses nothing.
	names map[string]string
}

// Spelling is the wire spelling of a raw name; false for a name outside
// the inventory.
func (i Inventory) Spelling(name string) (string, bool) {
	spelled, known := i.spellings[name]
	return spelled, known
}

func (i Inventory) Len() int { return len(i.spellings) }

// Despell is text with every wire spelling of this inventory replaced
// by the name ADK dispatches.
//
// The runtime names a tool by the spelling it was given, and the model
// dispatches another name, so runtime text that reaches the model
// passes through here first. The substitution is closed: a whole
// spelling this inventory carries is replaced, and every other byte
// stands — a spelling it never gave out included.
func (i Inventory) Despell(text string) string {
	var spelled strings.Builder
	written := 0
	for _, span := range spelledCandidate.FindAllStringIndex(text, -1) {
		start, end := span[0], span[1]
		if start > 0 && strings.IndexByte(spellingEdge, text[start-1]) >= 0 {
			continue
		}
		name, gaveOut := i.names[text[start:end]]
		if !gaveOut {
			continue
		}
		spelled.WriteString(text[written:start])
		spelled.WriteString(name)
		written = end
	}
	if written == 0 {
		return text
	}
	spelled.WriteString(text[written:])
	return spelled.String()
}

// InventoryRefusalKind names why a rendered config yields no inventory.
type InventoryRefusalKind int

const (
	// UnfilteredToolset: an MCP entry declares no tool filter.
	UnfilteredToolset InventoryRefusalKind = iota
	// UnspellableName: a declared name the wire cannot spell.
	UnspellableName
	// DuplicateName: one raw name declared twice.
	DuplicateName
	// CollidingSpelling: two raw names that spell alike on the wire.
	CollidingSpelling
)

// InventoryRefusal is BuildInventory's error: which refusal fired, the
// config path it names, and the name at fault.
type InventoryRefusal struct {
	Kind   InventoryRefusalKind
	Path   string
	Name   string
	Detail string
}

func (r *InventoryRefusal) Error() string {
	switch r.Kind {
	case UnfilteredToolset:
		return fmt.Sprintf("%s declares no tool filter, and the gate names only what the config declares: "+
			"list under tools every tool of this server the agent may call", r.Path)
	case DuplicateName:
		return fmt.Sprintf("the config declares the tool name %q twice (%s), and the gate cannot tell the two apart: rename one of them",
			r.Name, r.Detail)
	case CollidingSpelling:
		// The detail already names both declarations and the spelling.
		return r.Detail
	default:
		return fmt.Sprintf("%s: %s", r.Path, r.Detail)
	}
}

// MCPServerSpec is one MCP entry of the rendered config.
type MCPServerSpec struct {
	Path  string
	URL   string
	Tools []string
}

// RemoteAgentSpec is one remote agent of the rendered config, under the
// name the stock builder dispatches it by.
type RemoteAgentSpec struct {
	Path string
	Name string
}

// BuiltinGroups switches the manifest groups the rendered config and
// the environment turn on. The always group needs no switch.
type BuiltinGroups struct {
	Memory     bool
	Skills     bool
	ShareTools bool
}

// InventorySpec is what the runtime main reads off the rendered config.
type InventorySpec struct {
	MCPServers   []MCPServerSpec
	RemoteAgents []RemoteAgentSpec
	Builtins     BuiltinGroups
}

// BuildInventory spells every tool the spec declares. A refusal is an
// *InventoryRefusal; a manifest this image cannot read is a plain error.
func BuildInventory(spec InventorySpec) (Inventory, error) {
	b := builder{spellings: map[string]string{}, names: map[string]string{}, sources: map[string]string{}}
	if err := b.add(ReservedTool, ControlTool, "the reserved tool"); err != nil {
		return Inventory{}, err
	}
	for _, server := range spec.MCPServers {
		if err := b.mcpServer(server); err != nil {
			return Inventory{}, err
		}
	}
	for _, remote := range spec.RemoteAgents {
		if err := b.remoteAgent(remote); err != nil {
			return Inventory{}, err
		}
	}
	groups, err := laneGroups()
	if err != nil {
		return Inventory{}, err
	}
	enabled := map[string]bool{
		"always":      true,
		"memory":      spec.Builtins.Memory,
		"skills":      spec.Builtins.Skills,
		"share_tools": spec.Builtins.ShareTools,
	}
	for group, names := range groups {
		on, known := enabled[group]
		if !known {
			return Inventory{}, fmt.Errorf("the builtin manifest names a group this image does not switch on: %s", group)
		}
		if !on {
			continue
		}
		for _, name := range names {
			if err := b.add(name, BuiltinSpelling(name), "the builtin group "+group); err != nil {
				return Inventory{}, err
			}
		}
	}
	return Inventory{spellings: b.spellings, names: b.names}, nil
}

func laneGroups() (map[string][]string, error) {
	var manifest map[string]struct {
		Groups map[string][]string `json:"groups"`
	}
	if err := json.Unmarshal(builtinsManifest, &manifest); err != nil {
		return nil, fmt.Errorf("the builtin manifest does not parse: %w", err)
	}
	lane, present := manifest[ManifestLane]
	if !present {
		return nil, fmt.Errorf("the builtin manifest carries no %s lane", ManifestLane)
	}
	return lane.Groups, nil
}

// builder holds both directions of one inventory, each raw name and
// each spelling taken once.
type builder struct {
	spellings map[string]string
	names     map[string]string
	sources   map[string]string
}

func (b *builder) add(name, spelling, source string) error {
	if declared, twice := b.sources[name]; twice {
		return &InventoryRefusal{Kind: DuplicateName, Path: source, Name: name, Detail: declared + " and " + source}
	}
	if spelled, taken := b.names[spelling]; taken {
		return &InventoryRefusal{Kind: CollidingSpelling, Path: source, Name: name,
			Detail: fmt.Sprintf("the config declares %q (%s) and %q (%s), and both spell as %q on the wire: "+
				"the runtime could name only one of them back to the model, so rename one of them",
				spelled, b.sources[spelled], name, source, spelling)}
	}
	b.spellings[name] = spelling
	b.names[spelling] = name
	b.sources[name] = source
	return nil
}

func (b *builder) mcpServer(server MCPServerSpec) error {
	toolset, ok := toolsetOf(server.URL)
	// A doubled underscore is the mark kagent reserves, so the runtime
	// admits no canonical id whose namespace carries one.
	if !ok || strings.Contains(toolset, "__") {
		return &InventoryRefusal{Kind: UnspellableName, Path: server.Path, Name: server.URL,
			Detail: fmt.Sprintf("the toolset name is the first label of the server host in the URL, and %q carries none the wire can spell", server.URL)}
	}
	if len(server.Tools) == 0 {
		return &InventoryRefusal{Kind: UnfilteredToolset, Path: server.Path}
	}
	for position, name := range server.Tools {
		if !segment.MatchString(name) {
			return &InventoryRefusal{Kind: UnspellableName, Path: fmt.Sprintf("%s.tools[%d]", server.Path, position), Name: name,
				Detail: fmt.Sprintf("the tool name %q is outside what the wire can spell", name)}
		}
		if err := b.add(name, MCPSpelling(toolset, name), server.Path); err != nil {
			return err
		}
	}
	return nil
}

func (b *builder) remoteAgent(remote RemoteAgentSpec) error {
	namespace, agent, marked := strings.Cut(remote.Name, namespaceMark)
	if !marked || namespace == "" || agent == "" || strings.Contains(agent, namespaceMark) {
		return &InventoryRefusal{Kind: UnspellableName, Path: remote.Path, Name: remote.Name,
			Detail: fmt.Sprintf("kagent renders a remote agent as <namespace>__NS__<agent>, and %q is not that shape", remote.Name)}
	}
	namespace, agent = strings.ReplaceAll(namespace, "_", "-"), strings.ReplaceAll(agent, "_", "-")
	if !segment.MatchString(namespace) || !segment.MatchString(agent) {
		return &InventoryRefusal{Kind: UnspellableName, Path: remote.Path, Name: remote.Name,
			Detail: fmt.Sprintf("the remote agent name %q is outside what the wire can spell", remote.Name)}
	}
	return b.add(remote.Name, AgentSpelling(namespace, agent), remote.Path)
}

func toolsetOf(raw string) (string, bool) {
	parsed, err := url.Parse(raw)
	if err != nil {
		return "", false
	}
	host := parsed.Hostname()
	if host == "" {
		return "", false
	}
	label, _, _ := strings.Cut(host, ".")
	if !segment.MatchString(label) {
		return "", false
	}
	return label, true
}
