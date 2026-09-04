// The tool inventory: what a rendered config lets the wire name, and how.

package appakagentadk

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"strings"
	"testing"
)

const (
	sharedManifestPath = "../fixtures/kagent-builtins.json"
	pythonManifestPath = "../appa-kagent-adk/src/appa_kagent_adk/builtins.json"
)

var demoTools = MCPServerSpec{
	Path: "http_tools[0]", URL: "http://demo-tools.kagent.svc.cluster.local:3000/mcp", Tools: []string{"list_pods"},
}

func mustBuild(t *testing.T, spec InventorySpec) Inventory {
	t.Helper()
	inventory, err := BuildInventory(spec)
	if err != nil {
		t.Fatalf("the inventory must build: %v", err)
	}
	return inventory
}

func mustRefuse(t *testing.T, spec InventorySpec, kind InventoryRefusalKind, path string) {
	t.Helper()
	_, err := BuildInventory(spec)
	var refusal *InventoryRefusal
	if !errors.As(err, &refusal) {
		t.Fatalf("the inventory must refuse, got %v", err)
	}
	if refusal.Kind != kind || refusal.Path != path {
		t.Errorf("the refusal drifted: got kind %d at %q, want kind %d at %q (%v)", refusal.Kind, refusal.Path, kind, path, err)
	}
}

func TestEachClassSpellsItsTools(t *testing.T) {
	inventory := mustBuild(t, InventorySpec{
		MCPServers: []MCPServerSpec{
			demoTools,
			{Path: "sse_tools[0]", URL: "https://kagent-tool-server:8084/sse", Tools: []string{"k8s_get_resources"}},
		},
		RemoteAgents: []RemoteAgentSpec{{Path: "remote_agents[0].name", Name: "kagent__NS__log_analyst"}},
	})
	for name, want := range map[string]string{
		"list_pods":               "mcp:demo-tools/list_pods",
		"k8s_get_resources":       "mcp:kagent-tool-server/k8s_get_resources",
		"kagent__NS__log_analyst": "agent:kagent/log-analyst",
		"ask_user":                "builtin:ask_user",
		ReservedTool:              ControlTool,
	} {
		if got, known := inventory.Spelling(name); !known || got != want {
			t.Errorf("%s spells as %q (%v), want %q", name, got, known, want)
		}
	}
	if _, known := inventory.Spelling("k8s_delete_namespace"); known {
		t.Error("a name the config never declared is outside the inventory")
	}
}

func TestOnlyTheAgentClassIsASpawn(t *testing.T) {
	if !IsSpawn("agent:kagent/log-analyst") {
		t.Error("an agent: tool is a spawn")
	}
	for _, spelled := range []string{"mcp:demo-tools/list_pods", "builtin:ask_user", "gate:code_execution", ControlTool} {
		if IsSpawn(spelled) {
			t.Errorf("%s is no spawn", spelled)
		}
	}
}

func TestARemoteAgentNameUnmanglesBothLabels(t *testing.T) {
	inventory := mustBuild(t, InventorySpec{
		RemoteAgents: []RemoteAgentSpec{{Path: "remote_agents[0].name", Name: "team_a__NS__release_manager_go"}},
	})
	if got, _ := inventory.Spelling("team_a__NS__release_manager_go"); got != "agent:team-a/release-manager-go" {
		t.Errorf("the labels come back with their hyphens, got %q", got)
	}
}

func TestAnMCPEntryWithoutAToolFilterIsRefused(t *testing.T) {
	for _, tools := range [][]string{nil, {}} {
		mustRefuse(t, InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "http://demo-tools:3000/mcp", Tools: tools}}},
			UnfilteredToolset, "http_tools[0]")
	}
}

func TestANameTheWireCannotSpellIsRefused(t *testing.T) {
	mustRefuse(t, InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "", Tools: []string{"a"}}}},
		UnspellableName, "http_tools[0]")
	mustRefuse(t, InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "/mcp", Tools: []string{"a"}}}},
		UnspellableName, "http_tools[0]")
	mustRefuse(t, InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "http://demo-tools:3000/mcp", Tools: []string{"list pods"}}}},
		UnspellableName, "http_tools[0].tools[0]")
	mustRefuse(t, InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "http://demo__tools:3000/mcp", Tools: []string{"a"}}}},
		UnspellableName, "http_tools[0]")
	for _, name := range []string{"log-analyst", "__NS__log_analyst", "kagent__NS__", "a__NS__b__NS__c"} {
		mustRefuse(t, InventorySpec{RemoteAgents: []RemoteAgentSpec{{Path: "remote_agents[0].name", Name: name}}},
			UnspellableName, "remote_agents[0].name")
	}
}

func TestARawNameDeclaredTwiceIsRefused(t *testing.T) {
	other := MCPServerSpec{Path: "http_tools[1]", URL: "http://other:3000/mcp", Tools: []string{"list_pods"}}
	_, err := BuildInventory(InventorySpec{MCPServers: []MCPServerSpec{demoTools, other}})
	var refusal *InventoryRefusal
	if !errors.As(err, &refusal) || refusal.Kind != DuplicateName || refusal.Name != "list_pods" {
		t.Errorf("a tool listed by two servers is refused, got %v", err)
	}
	_, err = BuildInventory(InventorySpec{MCPServers: []MCPServerSpec{{Path: "http_tools[0]", URL: "http://x:1/mcp", Tools: []string{"ask_user"}}}})
	if !errors.As(err, &refusal) || refusal.Kind != DuplicateName || refusal.Name != "ask_user" {
		t.Errorf("a tool that shadows a builtin is refused, got %v", err)
	}
}

func TestTwoRawNamesThatSpellAlikeAreRefused(t *testing.T) {
	// kagent renders a hyphen as an underscore, so these two distinct raw
	// names carry one spelling, and one of them would be lost.
	_, err := BuildInventory(InventorySpec{RemoteAgents: []RemoteAgentSpec{
		{Path: "remote_agents[0].name", Name: "team_a__NS__release_manager"},
		{Path: "remote_agents[1].name", Name: "team-a__NS__release_manager"},
	}})
	var refusal *InventoryRefusal
	if !errors.As(err, &refusal) || refusal.Kind != CollidingSpelling || refusal.Name != "team-a__NS__release_manager" {
		t.Fatalf("two names that spell alike are refused, got %v", err)
	}
	if !strings.Contains(refusal.Error(), "agent:team-a/release-manager") {
		t.Errorf("the refusal names the spelling they collide on, got %q", refusal.Error())
	}
}

func TestEverySpellingDespellsBackToTheNameADKDispatches(t *testing.T) {
	inventory := mustBuild(t, InventorySpec{
		MCPServers:   []MCPServerSpec{demoTools},
		RemoteAgents: []RemoteAgentSpec{{Path: "remote_agents[0].name", Name: "kagent__NS__log_analyst"}},
	})
	for name, spelling := range inventory.spellings {
		if got := inventory.Despell("call " + spelling + " now"); got != "call "+name+" now" {
			t.Errorf("%q despells to %q, want the raw name %q", spelling, got, name)
		}
	}
}

func TestDespellReplacesAWholeSpellingAndLeavesEveryLongerIdentifier(t *testing.T) {
	inventory := mustBuild(t, InventorySpec{
		MCPServers:   []MCPServerSpec{demoTools},
		RemoteAgents: []RemoteAgentSpec{{Path: "remote_agents[0].name", Name: "kagent__NS__log_analyst"}},
	})
	const listPods = "mcp:demo-tools/list_pods"
	const logAnalyst = "agent:kagent/log-analyst"
	for _, row := range []struct{ id, text, want string }{
		// The plain cases: a spelling the runtime named, whole.
		{"in-a-sentence", "call " + listPods + " now", "call list_pods now"},
		{"the-whole-text", listPods, "list_pods"},
		{"the-agent-class", "blocked " + logAnalyst, "blocked kagent__NS__log_analyst"},
		{"two-spellings", listPods + " then " + logAnalyst + ".", "list_pods then kagent__NS__log_analyst."},
		// Punctuation after a spelling is punctuation, and it stands.
		{"a-period", "Retry " + listPods + ".", "Retry list_pods."},
		{"a-comma", "Retry " + listPods + ", then stop", "Retry list_pods, then stop"},
		{"a-colon", "blocked " + listPods + ": no body", "blocked list_pods: no body"},
		{"a-closing-bracket", "[" + listPods + "]", "[list_pods]"},
		{"a-quote", `"` + listPods + `"`, `"list_pods"`},
		// A spelling that only opens a longer identifier names no tool
		// the runtime gave out, and the whole run stands.
		{"a-longer-path", "blocked " + listPods + "/response", "blocked " + listPods + "/response"},
		{"a-dotted-suffix", listPods + ".json", listPods + ".json"},
		{"a-longer-last-segment", listPods + "x", listPods + "x"},
		{"a-longer-first-segment", "x" + listPods, "x" + listPods},
		{"preceded-by-a-path", "notes/" + listPods, "notes/" + listPods},
		{"inside-a-longer-identifier", "a/" + listPods + "/b", "a/" + listPods + "/b"},
		// A spelling of the right shape this inventory never gave out.
		{"never-issued", "mcp:other/list_pods", "mcp:other/list_pods"},
	} {
		if got := inventory.Despell(row.text); got != row.want {
			t.Errorf("%s: %q despells to %q, want %q", row.id, row.text, got, row.want)
		}
	}
}

func TestTheBuiltinGroupsFollowTheSwitches(t *testing.T) {
	plain := mustBuild(t, InventorySpec{})
	for _, name := range []string{"load_memory", "read_file", "create_share_link"} {
		if _, known := plain.Spelling(name); known {
			t.Errorf("%s is off without its switch", name)
		}
	}
	all := mustBuild(t, InventorySpec{Builtins: BuiltinGroups{Memory: true, Skills: true, ShareTools: true}})
	for _, name := range []string{"preload_memory", "load_memory", "save_memory", "skills", "read_file", "bash", "create_share_link"} {
		if got, known := all.Spelling(name); !known || got != BuiltinSpelling(name) {
			t.Errorf("%s spells as %q (%v) with its switch on", name, got, known)
		}
	}
}

func TestTheEmbeddedManifestIsTheSharedOneAndThePythonCopy(t *testing.T) {
	shared, err := os.ReadFile(sharedManifestPath)
	if err != nil {
		t.Fatalf("the shared manifest must be readable: %v", err)
	}
	python, err := os.ReadFile(pythonManifestPath)
	if err != nil {
		t.Fatalf("the python copy must be readable: %v", err)
	}
	if !bytes.Equal(builtinsManifest, shared) {
		t.Error("the embedded manifest drifted from the shared fixture")
	}
	if !bytes.Equal(python, shared) {
		t.Error("the python copy drifted from the shared fixture")
	}
	var lanes map[string]json.RawMessage
	if err := json.Unmarshal(shared, &lanes); err != nil {
		t.Fatalf("the manifest must parse: %v", err)
	}
	if _, present := lanes[manifestLane]; !present || len(lanes) != 2 {
		t.Errorf("the manifest carries the python and go lanes, got %v", lanes)
	}
}
