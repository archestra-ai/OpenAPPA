package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/kagent-dev/kagent/go/adk/pkg/config"
	"github.com/kagent-dev/kagent/go/api/adk"

	"context"
	a2atype "github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv"
	"github.com/a2aproject/a2a-go/a2asrv/eventqueue"
	appakagentadk "github.com/archestra-ai/OpenAPPA/integrations/kagent/appa-kagent-adk-go"
	"github.com/go-logr/logr"
	adkplugin "google.golang.org/adk/v2/plugin"
	adkrunner "google.golang.org/adk/v2/runner"
	adksession "google.golang.org/adk/v2/session"
)

// setKnob puts the knob in the state a case describes. t.Setenv runs
// first, so the cleanup restores the operator's value either way.
func setKnob(t *testing.T, set bool, value string) {
	t.Helper()
	t.Setenv(appaEnabledEnv, value)
	if !set {
		if err := os.Unsetenv(appaEnabledEnv); err != nil {
			t.Fatal(err)
		}
	}
}

// The knob is a closed set. Unset, empty and "false" serve the stock
// runtime. "true" gates the agent. Every other value refuses the start,
// because a typo must never disable the gate in silence.
func TestTheKnobIsAClosedSet(t *testing.T) {
	cases := []struct {
		name    string
		set     bool
		value   string
		mode    appaMode
		refused bool
	}{
		{name: "unset", mode: appaOff},
		{name: "empty", set: true, mode: appaOff},
		{name: "blank space", set: true, value: "   ", mode: appaOff},
		{name: "false", set: true, value: "false", mode: appaOff},
		{name: "False", set: true, value: "False", mode: appaOff},
		{name: "true", set: true, value: "true", mode: appaOn},
		{name: "TRUE with a trailing space", set: true, value: "TRUE ", mode: appaOn},
		{name: "yes", set: true, value: "yes", refused: true},
		{name: "1", set: true, value: "1", refused: true},
		{name: "0", set: true, value: "0", refused: true},
		{name: "a typo of true", set: true, value: "ture", refused: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			setKnob(t, tc.set, tc.value)
			mode, err := appaModeFromEnv()
			if tc.refused {
				var refusal *knobRefusal
				if !errors.As(err, &refusal) {
					t.Fatalf("a value outside the set must refuse the start, got mode %d and %v", mode, err)
				}
				if !strings.Contains(err.Error(), tc.value) {
					t.Errorf("the diagnostic must name the value %q, got %q", tc.value, err.Error())
				}
				return
			}
			if err != nil {
				t.Fatalf("the value is inside the set: %v", err)
			}
			if mode != tc.mode {
				t.Errorf("the knob selected mode %d, want %d", mode, tc.mode)
			}
		})
	}
}

// The knob alone decides. An ungated agent ignores the runtime URL, and
// a gated agent that names no runtime fails closed.
func TestTheKnobAloneDecidesTheGating(t *testing.T) {
	const runtimeURL = "http://appa-runtime.appa-system:8787"

	t.Run("the knob off ignores a runtime URL", func(t *testing.T) {
		setKnob(t, false, "")
		t.Setenv(runtimeURLEnv, runtimeURL)
		gate, err := gatingFromEnv()
		if err != nil {
			t.Fatal(err)
		}
		if gate.enabled() {
			t.Fatal("a runtime URL must not turn the deltas on")
		}
		if gate.runtimeURL != "" {
			t.Errorf("no delta may see a runtime URL while the knob is off, got %q", gate.runtimeURL)
		}
		if gate.ignoredRuntimeURL != runtimeURL {
			t.Errorf("the startup line must name the ignored URL, got %q", gate.ignoredRuntimeURL)
		}
	})

	t.Run("the knob off names no ignored URL when none is set", func(t *testing.T) {
		setKnob(t, true, "false")
		t.Setenv(runtimeURLEnv, "   ")
		gate, err := gatingFromEnv()
		if err != nil {
			t.Fatal(err)
		}
		if gate.enabled() || gate.ignoredRuntimeURL != "" {
			t.Errorf("blank space is not a runtime URL, got %+v", gate)
		}
	})

	t.Run("the knob on takes the runtime URL", func(t *testing.T) {
		setKnob(t, true, "true")
		t.Setenv(runtimeURLEnv, " "+runtimeURL+" ")
		gate, err := gatingFromEnv()
		if err != nil {
			t.Fatal(err)
		}
		if !gate.enabled() {
			t.Fatal("the knob on must turn the deltas on")
		}
		if gate.runtimeURL != runtimeURL {
			t.Errorf("the runtime URL drifted: %q", gate.runtimeURL)
		}
		if gate.ignoredRuntimeURL != "" {
			t.Errorf("a gated agent ignores no URL, got %q", gate.ignoredRuntimeURL)
		}
	})

	t.Run("the knob on without a runtime URL fails closed", func(t *testing.T) {
		setKnob(t, true, "true")
		t.Setenv(runtimeURLEnv, "   ")
		if _, err := gatingFromEnv(); !errors.Is(err, errMissingRuntimeURL) {
			t.Fatalf("a gated start that names no runtime must refuse, got %v", err)
		}
	})

	t.Run("a value outside the set refuses before the URL", func(t *testing.T) {
		setKnob(t, true, "yes")
		t.Setenv(runtimeURLEnv, runtimeURL)
		var refusal *knobRefusal
		if _, err := gatingFromEnv(); !errors.As(err, &refusal) {
			t.Fatalf("the knob refusal must reach the caller, got %v", err)
		}
	})
}

// -- the knob decides every delta -----------------------------------

// testRuntimeURL is the runtime a gated case names.
const testRuntimeURL = "http://appa-runtime.appa-system:8787"

// gateOff and gateOn are the two gating values every delta asks. Off
// is what an agent that leaves the knob alone gets.
var (
	gateOff = gating{mode: appaOff}
	gateOn  = gating{mode: appaOn, runtimeURL: testRuntimeURL}
)

// stockCard is an agent card the stock loader reads. The loader
// unmarshals it and validates nothing, so the four fields kagent
// renders are enough.
const stockCard = `{"name": "demo-agent", "description": "a demo agent", "url": "http://demo-agent:8080", "version": "1.0.0"}`

// deltaConfig is a rendered config every config delta can change: an
// OpenAI model that names no reasoning effort, and one remote agent.
func deltaConfig() *adk.AgentConfig {
	return &adk.AgentConfig{
		Model:        &adk.OpenAI{},
		RemoteAgents: []adk.RemoteAgentConfig{{Name: "billing-agent", Url: "http://billing.agents:8080"}},
	}
}

// stubExecutor stands in for the stock executor. No method runs: the
// cases below read which executor serves, not what it does.
type stubExecutor struct {
	a2asrv.AgentExecutor
}

// The config deltas are the reserved toolset and the reasoning-effort
// fill. While the knob is off the stock builder reads the config the
// stock loader decoded, so neither applies — a runtime URL an operator
// set does not change that.
func TestTheKnobDecidesTheConfigDeltas(t *testing.T) {
	cases := []struct {
		name string
		gate gating
		// httpTools is the toolset count the stock builder reads.
		httpTools int
		// effort is the OpenAI reasoning effort after the deltas.
		effort string
	}{
		{name: "off", gate: gateOff},
		{name: "off with a runtime URL an operator set", gate: gating{mode: appaOff, ignoredRuntimeURL: testRuntimeURL}},
		{name: "on", gate: gateOn, httpTools: 1, effort: "none"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Setenv(reasoningEffortEnv, "none")
			agentConfig := deltaConfig()
			applyConfigDeltas(tc.gate, agentConfig, logr.Discard())
			if len(agentConfig.HttpTools) != tc.httpTools {
				t.Errorf("the stock builder reads %d toolsets, want %d", len(agentConfig.HttpTools), tc.httpTools)
			}
			effort := ""
			if filled := agentConfig.Model.(*adk.OpenAI).ReasoningEffort; filled != nil {
				effort = *filled
			}
			if effort != tc.effort {
				t.Errorf("the reasoning effort is %q, want %q", effort, tc.effort)
			}
		})
	}
}

// While the knob is off the runner keeps the stock plugin list, so no
// gated callback runs. The stock plugins stay ahead of the gate.
func TestTheKnobDecidesThePluginRegistration(t *testing.T) {
	stock, err := adkplugin.New(adkplugin.Config{Name: "stock_plugin"})
	if err != nil {
		t.Fatal(err)
	}
	cases := []struct {
		name string
		gate gating
		want int
	}{
		{name: "off", gate: gateOff, want: 1},
		{name: "on", gate: gateOn, want: 2},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			runnerConfig := adkrunner.Config{
				PluginConfig: adkrunner.PluginConfig{Plugins: []*adkplugin.Plugin{stock}},
			}
			if err := appendAppaPlugin(tc.gate, &runnerConfig, appakagentadk.Inventory{}, logr.Discard()); err != nil {
				t.Fatal(err)
			}
			plugins := runnerConfig.PluginConfig.Plugins
			if len(plugins) != tc.want {
				t.Fatalf("the runner holds %d plugins, want %d", len(plugins), tc.want)
			}
			if plugins[0] != stock {
				t.Error("the gate joins after the stock plugins, and never replaces one")
			}
		})
	}
}

// While the knob is off the runtime serves the stock session service
// and the stock executor, so a session carries no landed lineage and a
// task carries the stock history.
func TestTheKnobDecidesTheSessionAndExecutorDeltas(t *testing.T) {
	stockSession := adksession.InMemoryService()
	stockExecutor := &stubExecutor{}

	t.Run("off keeps both stock", func(t *testing.T) {
		if got := withLineageHeaders(gateOff, stockSession); got != stockSession {
			t.Errorf("the session service is decorated while the knob is off: %T", got)
		}
		if got := withReviewShape(gateOff, stockExecutor); got != stockExecutor {
			t.Errorf("the executor is wrapped while the knob is off: %T", got)
		}
	})

	t.Run("on decorates both", func(t *testing.T) {
		if _, ok := withLineageHeaders(gateOn, stockSession).(lineageSessionService); !ok {
			t.Error("a gated session must land the lineage headers a delegated call carries")
		}
		if _, ok := withReviewShape(gateOn, stockExecutor).(reviewShapedExecutor); !ok {
			t.Error("a gated task must stay python-shaped while a person rules on a remedy")
		}
	})

	t.Run("no session service stays none", func(t *testing.T) {
		// session.NewService returns nil when the deployment names no
		// store, and the runner then builds its own in-memory service.
		if got := withLineageHeaders(gateOn, nil); got != nil {
			t.Errorf("a nil session service must stay nil: %T", got)
		}
	})
}

// While the knob is off the config load is the stock load, so a config
// the guard refuses for a gated agent starts as the stock image starts
// it. This case reads the loader in-process; the subprocess cases
// below read the whole start.
func TestTheKnobOffLoadsThroughTheStockLoader(t *testing.T) {
	dir := t.TempDir()
	// sub_agents is a key the guard refuses and the stock loader drops.
	writeConfig(t, dir, withKey(t, stockConfig, "sub_agents", `[{"name": "child"}]`))
	if err := os.WriteFile(filepath.Join(dir, "agent-card.json"), []byte(stockCard), 0o600); err != nil {
		t.Fatal(err)
	}
	stockLoaded, stockCardLoaded, err := config.LoadAgentConfigs(dir)
	if err != nil {
		t.Fatalf("the stock loader accepts this config: %v", err)
	}

	loaded, inventory, card := loadAgentConfigs(gateOff, dir, logr.Discard())
	if !reflect.DeepEqual(loaded, stockLoaded) {
		t.Errorf("the ungated load must be the stock load: got %+v", loaded)
	}
	if inventory.Len() != 0 {
		t.Errorf("the ungated load builds no inventory, got %d spellings", inventory.Len())
	}
	if !reflect.DeepEqual(card, stockCardLoaded) {
		t.Errorf("the ungated card must be the stock card: got %+v", card)
	}
}

func TestTheReservedToolsetJoinsTheRenderedConfig(t *testing.T) {
	agentConfig := &adk.AgentConfig{}
	withReservedToolset(agentConfig, "http://appa-runtime.appa-system:8787/")
	if len(agentConfig.HttpTools) != 1 {
		t.Fatalf("exactly one reserved toolset must join, got %d", len(agentConfig.HttpTools))
	}
	reserved := agentConfig.HttpTools[0]
	if reserved.Params.Url != "http://appa-runtime.appa-system:8787/mcp" {
		t.Errorf("the reserved toolset must point at /mcp, got %q", reserved.Params.Url)
	}
	if !reflect.DeepEqual(reserved.Tools, []string{appakagentadk.ReservedTool}) {
		t.Errorf("the reserved toolset must serve only execute_remedy_plan, got %v", reserved.Tools)
	}
	if reserved.Params.Timeout == nil || *reserved.Params.Timeout != remedyCallTimeoutSeconds {
		t.Errorf("the remedy call must outlast a parked consult; the ADK default fails it at the client: %v", reserved.Params.Timeout)
	}
}

func TestTheInventorySpecFollowsTheStockBuilder(t *testing.T) {
	shared := true
	agentConfig := &adk.AgentConfig{
		HttpTools: []adk.HttpMcpServerConfig{{
			Params: adk.StreamableHTTPConnectionParams{Url: "http://demo-tools:8080/mcp"}, Tools: []string{"list_pods"},
		}},
		SseTools: []adk.SseMcpServerConfig{{
			Params: adk.SseConnectionParams{Url: "http://kagent-tool-server:8084/sse"}, Tools: []string{"k8s_get_resources"},
		}},
		RemoteAgents: []adk.RemoteAgentConfig{
			{Name: "kagent__NS__billing_agent", Url: "http://billing.agents:8080"},
			{Name: "skipped-agent"}, // no URL: the stock builder skips it
		},
		Memory:     &adk.MemoryConfig{},
		ShareTools: &shared,
	}
	want := appakagentadk.InventorySpec{
		MCPServers: []appakagentadk.MCPServerSpec{
			{Path: "http_tools[0]", URL: "http://demo-tools:8080/mcp", Tools: []string{"list_pods"}},
			{Path: "sse_tools[0]", URL: "http://kagent-tool-server:8084/sse", Tools: []string{"k8s_get_resources"}},
		},
		RemoteAgents: []appakagentadk.RemoteAgentSpec{{Path: "remote_agents[0].name", Name: "kagent__NS__billing_agent"}},
		Builtins:     appakagentadk.BuiltinGroups{Memory: true, Skills: true, ShareTools: true},
	}
	if got := inventorySpec(agentConfig, "/skills"); !reflect.DeepEqual(got, want) {
		t.Errorf("the spec drifted from what the stock builder wires:\n got %+v\n want %+v", got, want)
	}
	plain := inventorySpec(&adk.AgentConfig{}, " ")
	if plain.Builtins != (appakagentadk.BuiltinGroups{}) {
		t.Errorf("no memory, no skills folder and no share tools switch nothing on, got %+v", plain.Builtins)
	}
}

func TestTheGuardHandsBackTheInventoryOfTheStockConfig(t *testing.T) {
	_, inventory, err := decodeGuarded([]byte(stockConfig), "")
	if err != nil {
		t.Fatalf("the stock config must be accepted: %v", err)
	}
	for name, want := range map[string]string{
		"list_pods":                "mcp:demo-tools/list_pods",
		"kagent__NS__log_analyst":  "agent:kagent/log-analyst",
		"ask_user":                 "builtin:ask_user",
		appakagentadk.ReservedTool: appakagentadk.ControlTool,
	} {
		if got, known := inventory.Spelling(name); !known || got != want {
			t.Errorf("%s spells as %q (%v), want %q", name, got, known, want)
		}
	}
	if _, known := inventory.Spelling("load_memory"); known {
		t.Error("the stock config declares no memory, so the memory builtins stay out")
	}
}

func TestTheImageEnvFillsAnUnsetOpenAIReasoningEffort(t *testing.T) {
	agentConfig := &adk.AgentConfig{Model: &adk.OpenAI{}}
	withReasoningEffort(agentConfig, " none ")
	model := agentConfig.Model.(*adk.OpenAI)
	if model.ReasoningEffort == nil || *model.ReasoningEffort != "none" {
		t.Fatalf("the env must fill an unset reasoning effort, got %v", model.ReasoningEffort)
	}
	untouched := &adk.AgentConfig{Model: &adk.OpenAI{}}
	withReasoningEffort(untouched, "")
	if untouched.Model.(*adk.OpenAI).ReasoningEffort != nil {
		t.Errorf("an empty env must leave the rendered config untouched")
	}
}

func TestAReasoningEffortTheCRDSetWinsOverTheEnv(t *testing.T) {
	low := "low"
	agentConfig := &adk.AgentConfig{Model: &adk.OpenAI{ReasoningEffort: &low}}
	withReasoningEffort(agentConfig, "none")
	if got := *agentConfig.Model.(*adk.OpenAI).ReasoningEffort; got != "low" {
		t.Errorf("the CRD value must win over the env, got %q", got)
	}
	// A model of another type is untouched, and the fill must not panic on it.
	other := &adk.AgentConfig{Model: &adk.AzureOpenAI{}}
	withReasoningEffort(other, "none")
}

func TestTheLineageHeadersLandInSessionStateOnGetAndCreate(t *testing.T) {
	service := lineageSessionService{adksession.InMemoryService()}
	ctx, _ := a2asrv.WithCallContext(context.Background(), a2asrv.NewRequestMeta(map[string][]string{
		"x-kagent-root-context-id":   {"root-1"},
		"x-kagent-parent-context-id": {"parent-1"},
	}))
	created, err := service.Create(ctx, &adksession.CreateRequest{AppName: "app", UserID: "u1", SessionID: "child-1"})
	if err != nil {
		t.Fatal(err)
	}
	headers, err := created.Session.State().Get("headers")
	if err != nil {
		t.Fatalf("the lineage lands on Create: %v", err)
	}
	if got := headers.(map[string]any); got["x-kagent-root-context-id"] != "root-1" || got["x-kagent-parent-context-id"] != "parent-1" {
		t.Fatalf("the python-shaped headers dict: %v", got)
	}
	fetched, err := service.Get(ctx, &adksession.GetRequest{AppName: "app", UserID: "u1", SessionID: "child-1"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := fetched.Session.State().Get("headers"); err != nil {
		t.Fatalf("the lineage lands on Get too, for the runner's re-fetch: %v", err)
	}

	bare := lineageSessionService{adksession.InMemoryService()}
	plain, err := bare.Create(context.Background(), &adksession.CreateRequest{AppName: "app", UserID: "u1", SessionID: "root-2"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := plain.Session.State().Get("headers"); err == nil {
		t.Fatal("a request with no lineage leaves the key absent: the entry is a root")
	}
}

// recordingQueue keeps what the executor wrote; only Write is exercised.
type recordingQueue struct {
	eventqueue.Queue
	events []a2atype.Event
}

func (q *recordingQueue) Write(_ context.Context, event a2atype.Event) error {
	q.events = append(q.events, event)
	return nil
}

func TestThePendingReviewResponseNeverReachesTheTask(t *testing.T) {
	inner := &recordingQueue{}
	q := reviewShapedQueue{inner}
	ctx := context.Background()
	pending := a2atype.DataPart{
		Data:     map[string]any{"id": "c1", "name": "execute_remedy_plan", "response": map[string]any{"result": "[appa] the reviewer has been asked", "appa": "review"}},
		Metadata: map[string]any{"adk_type": "function_response"},
	}
	call := a2atype.DataPart{Data: map[string]any{"id": "c1", "name": "execute_remedy_plan", "args": map[string]any{"offer_id": "o1"}}}
	answered := a2atype.DataPart{Data: map[string]any{"id": "c1", "name": "execute_remedy_plan", "response": map[string]any{"result": "executed"}}}
	other := a2atype.DataPart{Data: map[string]any{"id": "c2", "name": "list_pods", "response": map[string]any{"appa": "review"}}}
	write := func(final bool, parts ...a2atype.Part) {
		t.Helper()
		ev := &a2atype.TaskStatusUpdateEvent{Final: final, Status: a2atype.TaskStatus{State: a2atype.TaskStateWorking, Message: &a2atype.Message{Parts: parts}}}
		if err := q.Write(ctx, ev); err != nil {
			t.Fatal(err)
		}
	}
	write(false, call)                                  // the call itself stays
	write(false, pending)                               // the pending response vanishes with its update
	write(false, a2atype.TextPart{Text: "hi"}, pending) // the text stays, the pending part goes
	write(false, answered)                              // a real response of the reserved tool stays
	write(false, other)                                 // another tool is never the plugin's marker
	write(true, pending)                                // a final update is written even when emptied
	if err := q.Write(ctx, &a2atype.Message{Parts: a2atype.ContentParts{pending}}); err != nil {
		t.Fatal(err) // other event kinds pass untouched
	}
	counts := []int{}
	for _, ev := range inner.events {
		if update, ok := ev.(*a2atype.TaskStatusUpdateEvent); ok {
			counts = append(counts, len(update.Status.Message.Parts))
		} else {
			counts = append(counts, -1)
		}
	}
	if want := []int{1, 1, 1, 1, 0, -1}; !reflect.DeepEqual(counts, want) {
		t.Fatalf("the parts that reached the task: got %v, want %v", counts, want)
	}
}

// -- the rendered-config guard --------------------------------------

// stockConfig carries every top-level key of the rc4 schema, shaped
// as kagent's compiler renders one for an openai agent with a sandbox
// that names no domain.
const stockConfig = `{
	"model": {"type": "openai", "model": "gpt-5.2"},
	"description": "a demo agent",
	"instruction": "help with the cluster",
	"http_tools": [{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["list_pods"]}],
	"sse_tools": [],
	"remote_agents": [{"name": "kagent__NS__log_analyst", "url": "http://log-analyst:8080"}],
	"execute_code": false,
	"stream": true,
	"memory": null,
	"network": {"allowed_domains": []},
	"context_config": null,
	"share_tools": false,
	"session_db_url": ""
}`

// topLevel decodes a fixture into its top-level keys.
func topLevel(t *testing.T, config string) map[string]json.RawMessage {
	t.Helper()
	var top map[string]json.RawMessage
	if err := json.Unmarshal([]byte(config), &top); err != nil {
		t.Fatalf("the fixture must be a JSON object: %v", err)
	}
	return top
}

// marshalTopLevel renders top-level keys back into a fixture.
func marshalTopLevel(t *testing.T, top map[string]json.RawMessage) string {
	t.Helper()
	out, err := json.Marshal(top)
	if err != nil {
		t.Fatal(err)
	}
	return string(out)
}

// withKey returns config with one top-level key set to a JSON value.
func withKey(t *testing.T, config, key, value string) string {
	t.Helper()
	top := topLevel(t, config)
	top[key] = json.RawMessage(value)
	return marshalTopLevel(t, top)
}

// withoutKey returns config with one top-level key removed.
func withoutKey(t *testing.T, config, key string) string {
	t.Helper()
	top := topLevel(t, config)
	delete(top, key)
	return marshalTopLevel(t, top)
}

// stockDecoded decodes a fixture through the stock decoder alone: the
// config the guard must hand back, unchanged, for a config it accepts.
func stockDecoded(t *testing.T, config string) *adk.AgentConfig {
	t.Helper()
	var agentConfig adk.AgentConfig
	if err := json.Unmarshal([]byte(config), &agentConfig); err != nil {
		t.Fatalf("the accepted config must decode through the stock decoder: %v", err)
	}
	return &agentConfig
}

func TestTheConfigGuardRefusesWhatThisImageCannotRunAsDeclared(t *testing.T) {
	compaction := `{"compaction": {"compaction_interval": 5, "token_threshold": 4000}}`
	cases := []struct {
		name    string
		config  string
		refused bool
		kind    configRefusalKind
		keys    []string
		// parse: the stock decoder cannot decode the bytes. That is
		// the decoder's own error, not a refusal.
		parse bool
	}{
		{name: "the stock config is accepted", config: stockConfig},

		// -- keys --
		{name: "sub_agents are refused by name", config: withKey(t, stockConfig, "sub_agents", `[{"name": "child"}]`),
			refused: true, kind: inProcessFeature, keys: []string{"sub_agents"}},
		{name: "agent_plugins are refused by name", config: withKey(t, stockConfig, "agent_plugins", `[{"name": "audit"}]`),
			refused: true, kind: inProcessFeature, keys: []string{"agent_plugins"}},
		{name: "a top-level key outside the schema is refused", config: withKey(t, stockConfig, "skills", `["k8s"]`),
			refused: true, kind: outsideSchema, keys: []string{"skills"}},
		{name: "every key outside the schema is named, sorted", config: withKey(t, withKey(t, stockConfig, "zeta", `1`), "alpha", `2`),
			refused: true, kind: outsideSchema, keys: []string{"alpha", "zeta"}},
		{name: "the named refusal wins over the generic one", config: withKey(t, withKey(t, stockConfig, "skills", `[]`), "sub_agents", `[]`),
			refused: true, kind: inProcessFeature, keys: []string{"sub_agents"}},
		// encoding/json folds Instruction onto instruction. The guard
		// matches exactly, so the variant is a key outside the schema.
		{name: "a case variant of a known key is refused", config: `{"model": {"type": "openai", "model": "gpt-5.2"}, "Instruction": "x"}`,
			refused: true, kind: outsideSchema, keys: []string{"Instruction"}},
		{name: "a JSON null is refused", config: `null`, refused: true, kind: notAnObject},
		{name: "a JSON array is refused", config: `[]`, refused: true, kind: notAnObject},
		{name: "a JSON string is refused", config: `"{}"`, refused: true, kind: notAnObject},
		{name: "text that is not JSON is refused", config: `{`, refused: true, kind: notAnObject},
		// Out of scope: the key check walks the top-level object only.
		// The stock decoder drops a nested unknown key, and this image
		// accepts that. The features the image must refuse by key land
		// as top-level keys.
		{name: "nested unknown keys are not refused", config: `{"model": {"type": "openai", "model": "gpt-5.2", "bogus": 1},
			"description": "", "instruction": "x",
			"http_tools": [{"params": {"url": "http://demo-tools:8080/mcp", "bogus": true}, "tools": ["list_pods"]}]}`},

		// -- execute_code --
		{name: "execute_code true is refused", config: withKey(t, stockConfig, "execute_code", `true`),
			refused: true, kind: codeExecution, keys: []string{"execute_code"}},
		{name: "execute_code false is accepted", config: withKey(t, stockConfig, "execute_code", `false`)},
		{name: "execute_code null is accepted", config: withKey(t, stockConfig, "execute_code", `null`)},
		{name: "an absent execute_code is accepted", config: withoutKey(t, stockConfig, "execute_code")},

		// -- an APPA-owned tool name --
		// appa_return is the tool a child scope returns through. A
		// declared tool of that name collides with the gate, so the
		// start refuses and names where the collision is.
		{name: "an http toolset that declares appa_return is refused",
			config: withKey(t, stockConfig, "http_tools",
				`[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["list_pods", "appa_return"]}]`),
			refused: true, kind: reservedToolName, keys: []string{"http_tools[0].tools[1]"}},
		{name: "an sse toolset that declares appa_return is refused",
			config: withKey(t, stockConfig, "sse_tools",
				`[{"params": {"url": "http://demo-tools:8080/sse", "headers": {}}, "tools": ["appa_return"]}]`),
			refused: true, kind: reservedToolName, keys: []string{"sse_tools[0].tools[0]"}},
		{name: "a remote agent named appa_return is refused",
			config: withKey(t, stockConfig, "remote_agents",
				`[{"name": "kagent__NS__log_analyst", "url": "http://log-analyst:8080"}, {"name": "appa_return", "url": "http://x:8080"}]`),
			refused: true, kind: reservedToolName, keys: []string{"remote_agents[1].name"}},
		{name: "an APPA-owned tool name wins over a value refusal",
			config: withKey(t, withKey(t, stockConfig, "execute_code", `true`), "http_tools",
				`[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["appa_return"]}]`),
			refused: true, kind: reservedToolName, keys: []string{"http_tools[0].tools[0]"}},
		// The reserved tool is APPA's name too. The runtime main appends
		// that toolset AFTER this guard, so a config that declares the
		// name reaches the model as two declarations of one name, and
		// which one answers is the builder's order. Refuse it here.
		{name: "a declared tool named execute_remedy_plan is refused",
			config: withKey(t, stockConfig, "http_tools",
				`[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["execute_remedy_plan"]}]`),
			refused: true, kind: reservedToolName, keys: []string{"http_tools[0].tools[0]"}},
		{name: "a remote agent named execute_remedy_plan is refused",
			config: withKey(t, stockConfig, "remote_agents",
				`[{"name": "execute_remedy_plan", "url": "http://x:8080"}]`),
			refused: true, kind: reservedToolName, keys: []string{"remote_agents[0].name"}},

		// -- network --
		// Neither runtime reads network from config.json at the pinned
		// versions. The allowlist reaches the Go skills shell through
		// srt-settings.json, as it reaches the python image. So the key
		// is not a value refusal, and an allowlist passes.
		{name: "a network allowlist is accepted", config: withKey(t, stockConfig, "network", `{"allowed_domains": ["example.com"]}`)},
		{name: "a network with an empty allowlist is accepted", config: withKey(t, stockConfig, "network", `{"allowed_domains": []}`)},
		{name: "a network with a null allowlist is accepted", config: withKey(t, stockConfig, "network", `{"allowed_domains": null}`)},
		{name: "an empty network object is accepted", config: withKey(t, stockConfig, "network", `{}`)},
		{name: "network null is accepted", config: withKey(t, stockConfig, "network", `null`)},
		{name: "an absent network is accepted", config: withoutKey(t, stockConfig, "network")},

		// -- context_config --
		{name: "a context_config with compaction is refused", config: withKey(t, stockConfig, "context_config", compaction),
			refused: true, kind: contextCompaction, keys: []string{"context_config"}},
		// The compiler renders {} for a CRD context block with no
		// compaction. The rule is non-null, so it is refused too.
		{name: "an empty context_config object is refused", config: withKey(t, stockConfig, "context_config", `{}`),
			refused: true, kind: contextCompaction, keys: []string{"context_config"}},
		{name: "context_config null is accepted", config: withKey(t, stockConfig, "context_config", `null`)},
		{name: "an absent context_config is accepted", config: withoutKey(t, stockConfig, "context_config")},

		// -- mixed --
		{name: "a named key refusal wins over a value refusal",
			config:  withKey(t, withKey(t, stockConfig, "execute_code", `true`), "sub_agents", `[]`),
			refused: true, kind: inProcessFeature, keys: []string{"sub_agents"}},
		{name: "a key outside the schema wins over a value refusal",
			config:  withKey(t, withKey(t, stockConfig, "context_config", compaction), "skills", `[]`),
			refused: true, kind: outsideSchema, keys: []string{"skills"}},
		{name: "execute_code is checked before context_config",
			config:  withKey(t, withKey(t, stockConfig, "execute_code", `true`), "context_config", compaction),
			refused: true, kind: codeExecution, keys: []string{"execute_code"}},
		// The value check runs on the decoded config, so a config the
		// stock decoder cannot decode fails as a decode error first.
		{name: "a config the stock decoder cannot decode is the decoder's error",
			config: withKey(t, withKey(t, stockConfig, "execute_code", `true`), "model", `{"type": "bogus"}`), parse: true},
		// The inventory runs last: a gated agent names every tool it
		// can call, and a name the wire cannot spell never reaches it.
		{name: "an MCP server without a tool filter is refused",
			config:  withKey(t, stockConfig, "http_tools", `[{"params": {"url": "http://demo-tools:8080/mcp"}}]`),
			refused: true, kind: unfilteredToolset, keys: []string{"http_tools[0]"}},
		{name: "an MCP server with an empty tool filter is refused",
			config:  withKey(t, stockConfig, "http_tools", `[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": []}]`),
			refused: true, kind: unfilteredToolset, keys: []string{"http_tools[0]"}},
		{name: "a remote agent outside the rendered shape is refused",
			config:  withKey(t, stockConfig, "remote_agents", `[{"name": "log-analyst", "url": "http://log-analyst:8080"}]`),
			refused: true, kind: unspellableTool, keys: []string{"remote_agents[0].name"}},
		{name: "a tool name declared by two servers is refused",
			config: withKey(t, stockConfig, "http_tools",
				`[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["list_pods"]}, {"params": {"url": "http://other:8080/mcp"}, "tools": ["list_pods"]}]`),
			refused: true, kind: unspellableTool, keys: []string{"http_tools[1]"}},
		{name: "the reserved-name refusal wins over the inventory",
			config: withKey(t, stockConfig, "http_tools",
				`[{"params": {"url": "http://demo-tools:8080/mcp"}, "tools": ["appa_return"]}, {"params": {"url": "http://other:8080/mcp"}}]`),
			refused: true, kind: reservedToolName, keys: []string{"http_tools[0].tools[0]"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, _, err := decodeGuarded([]byte(tc.config), "")
			var refusal *configRefusal
			isRefusal := errors.As(err, &refusal)
			if tc.parse {
				if err == nil || isRefusal {
					t.Fatalf("the stock decoder's error must surface as itself, got %v", err)
				}
				return
			}
			if !tc.refused {
				if err != nil {
					t.Fatalf("the config must be accepted, got %v", err)
				}
				// The guard hands back exactly what the stock decoder
				// decodes from the checked bytes.
				if want := stockDecoded(t, tc.config); !reflect.DeepEqual(got, want) {
					t.Fatalf("the decoded config drifted from the stock decoder: got %+v, want %+v", got, want)
				}
				return
			}
			if !isRefusal {
				t.Fatalf("the config must be refused by the guard, got %v", err)
			}
			if got != nil {
				t.Errorf("a refused config must not be handed back, got %+v", got)
			}
			if refusal.kind != tc.kind || !reflect.DeepEqual(refusal.keys, tc.keys) {
				t.Errorf("the refusal drifted: got kind %d keys %v, want kind %d keys %v", refusal.kind, refusal.keys, tc.kind, tc.keys)
			}
			for _, key := range tc.keys {
				if !strings.Contains(err.Error(), key) {
					t.Errorf("the diagnostic must name %s, got %q", key, err.Error())
				}
			}
		})
	}
}

func TestTheAcceptedTopLevelKeysAreExactlyTheRC4Schema(t *testing.T) {
	// A kagent module bump that widens adk.AgentConfig fails here. The
	// widening then lands with a decision on whether this image can
	// gate the new key.
	want := []string{
		"context_config", "description", "execute_code", "http_tools", "instruction", "memory", "model",
		"network", "remote_agents", "session_db_url", "share_tools", "sse_tools", "stream",
	}
	got := make([]string, 0, len(knownTopLevelKeys))
	for key := range knownTopLevelKeys {
		got = append(got, key)
	}
	sort.Strings(got)
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("the accepted top-level keys drifted from the rc4 schema: got %v, want %v", got, want)
	}
}

// buildTimeout bounds the nested go build of the runtime binary. The
// build resolves the module cache and the toolchain, so it is the slow
// step of the subprocess test.
const buildTimeout = 3 * time.Minute

// buildRuntime builds this package into a temporary directory and
// returns the binary. Under -short the test skips. Otherwise the go
// tool must be on PATH. A missing tool fails the test, so a full run
// never leaves the runtime refusal unverified.
func buildRuntime(t *testing.T) string {
	t.Helper()
	if testing.Short() {
		t.Skip("the runtime binary is not built under -short")
	}
	goTool, err := exec.LookPath("go")
	if err != nil {
		t.Fatalf("the go tool must be on PATH to build the runtime binary: %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), buildTimeout)
	defer cancel()
	binary := filepath.Join(t.TempDir(), "appa-kagent-adk-go")
	build := exec.CommandContext(ctx, goTool, "build", "-o", binary, ".")
	build.Env = os.Environ()
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("the runtime binary must build within %s: %v\n%s", buildTimeout, err, out)
	}
	return binary
}

func writeConfig(t *testing.T, dir, config string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "config.json"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
}

// deliverUnderConfigDir writes the config where CONFIG_DIR points.
func deliverUnderConfigDir(t *testing.T, config string) (args, env []string) {
	t.Helper()
	dir := t.TempDir()
	writeConfig(t, dir, config)
	return nil, []string{"CONFIG_DIR=" + dir}
}

func TestTheRuntimeRefusesToStartOnAnUnsupportedConfig(t *testing.T) {
	binary := buildRuntime(t)
	cases := []struct {
		name   string
		config string
		// want is what the diagnostic on stderr must name.
		want string
		// deliver places the config the way kagent does and returns
		// the child's args and env.
		deliver func(t *testing.T, config string) (args, env []string)
	}{
		{"a config.json under CONFIG_DIR", withKey(t, stockConfig, "sub_agents", `[]`), "sub_agents", deliverUnderConfigDir},
		{"a config delivered through KAGENT_CONFIG_JSON", withKey(t, stockConfig, "agent_plugins", `[]`), "agent_plugins",
			func(t *testing.T, config string) ([]string, []string) {
				return nil, []string{"CONFIG_DIR=" + t.TempDir(), "KAGENT_CONFIG_JSON=" + config}
			}},
		{"a config dir named by -filepath", withKey(t, stockConfig, "skills", `[]`), "skills",
			func(t *testing.T, config string) ([]string, []string) {
				dir := t.TempDir()
				writeConfig(t, dir, config)
				return []string{"-filepath", dir}, nil
			}},
		// The value refusal runs on the decoded config, inside the binary.
		{"a value the Go runtime would ignore", withKey(t, stockConfig, "execute_code", `true`), "execute_code", deliverUnderConfigDir},
		// So does the APPA-owned name refusal: the operator reads the
		// collision at the start, not at the first delegation.
		{"a tool that takes an APPA-owned name",
			withKey(t, stockConfig, "remote_agents", `[{"name": "appa_return", "url": "http://log-analyst:8080"}]`),
			"appa_return", deliverUnderConfigDir},
		// And the inventory: an MCP server the gate cannot name the
		// tools of refuses the start, not the first call.
		{"an MCP server without a tool filter",
			withKey(t, stockConfig, "http_tools", `[{"params": {"url": "http://demo-tools:8080/mcp"}}]`),
			"tool filter", deliverUnderConfigDir},
		// An accepted config passes the stock validation and reaches
		// the agent card, the one stock load left on disk. No card is
		// written, so that load is the failure the runtime reports.
		{"an accepted config reaches the agent card loader", stockConfig, "agent card", deliverUnderConfigDir},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			args, env := tc.deliver(t, tc.config)
			ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
			defer cancel()
			cmd := exec.CommandContext(ctx, binary, args...)
			// The child env is explicit: the knob and the runtime URL
			// so the start reaches the config, and no KAGENT_*
			// delivery beyond the one the case scripts.
			cmd.Env = append([]string{
				"PATH=" + os.Getenv("PATH"),
				"APPA_ENABLED=true",
				"APPA_RUNTIME_URL=http://127.0.0.1:1",
			}, env...)
			var stdout, stderr bytes.Buffer
			cmd.Stdout = &stdout
			cmd.Stderr = &stderr
			err := cmd.Run()
			var exit *exec.ExitError
			if !errors.As(err, &exit) {
				t.Fatalf("the runtime must exit before it serves, got %v\nstdout: %s\nstderr: %s", err, stdout.String(), stderr.String())
			}
			if exit.ExitCode() != 1 {
				t.Errorf("the refusal must exit 1, got %d\nstderr: %s", exit.ExitCode(), stderr.String())
			}
			if !strings.Contains(stderr.String(), tc.want) {
				t.Errorf("the diagnostic on stderr must name %s, got:\n%s", tc.want, stderr.String())
			}
			// The startup line names the mode every time.
			if !strings.Contains(stderr.String(), gatedStartupMessage) {
				t.Errorf("a gated start must log the gated mode, got:\n%s", stderr.String())
			}
			if strings.Contains(stderr.String(), "UNGATED") {
				t.Errorf("a gated start must never claim the ungated mode, got:\n%s", stderr.String())
			}
		})
	}
}

// TestTheKnobOffServesTheStockRuntime pins the drop-in contract. An
// operator sets this image as the cluster default agent image, and an
// agent that leaves the knob off must behave as it does on the stock
// image. So the guard never runs, and a config the guard refuses for a
// gated agent starts the stock loader instead.
//
// Each case delivers such a config and no agent-card.json. The stock
// loader reads the config, accepts it as the stock image accepts it,
// and reports the missing card. That failure is the proof the guard
// never fired: a refusal ends the process before the card, and names
// the key it refused.
func TestTheKnobOffServesTheStockRuntime(t *testing.T) {
	binary := buildRuntime(t)
	cases := []struct {
		name   string
		config string
		// refused is the text a gated start puts on stderr for this
		// same config. An ungated start must never print it.
		refused string
		// env is what the case adds to the child env. An empty env
		// leaves the knob unset and names no runtime.
		env []string
		// ignoredURL is true when the child must name the runtime URL
		// it ignores.
		ignoredURL bool
	}{
		{name: "sub_agents", config: withKey(t, stockConfig, "sub_agents", `[{"name": "child"}]`), refused: "sub_agents"},
		{name: "agent_plugins", config: withKey(t, stockConfig, "agent_plugins", `[{"name": "audit"}]`), refused: "agent_plugins"},
		{name: "a top-level key outside the rc4 schema", config: withKey(t, stockConfig, "skills", `["k8s"]`), refused: "skills"},
		{name: "a value the Go runtime would ignore", config: withKey(t, stockConfig, "execute_code", `true`), refused: "execute_code"},
		{name: "the knob set to false", config: withKey(t, stockConfig, "sub_agents", `[]`), refused: "sub_agents",
			env: []string{"APPA_ENABLED=false"}},
		{name: "the knob set to nothing", config: withKey(t, stockConfig, "sub_agents", `[]`), refused: "sub_agents",
			env: []string{"APPA_ENABLED="}},
		// A runtime URL on an agent the knob leaves off is an operator
		// mistake. The image runs stock and names the ignored URL.
		{name: "a runtime URL the knob leaves off", config: withKey(t, stockConfig, "sub_agents", `[]`), refused: "sub_agents",
			env: []string{"APPA_RUNTIME_URL=http://127.0.0.1:1"}, ignoredURL: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			dir := t.TempDir()
			writeConfig(t, dir, tc.config)
			ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
			defer cancel()
			cmd := exec.CommandContext(ctx, binary)
			// The child env leaves the knob off, so the image must
			// serve the stock runtime.
			cmd.Env = append([]string{"PATH=" + os.Getenv("PATH"), "CONFIG_DIR=" + dir}, tc.env...)
			var stdout, stderr bytes.Buffer
			cmd.Stdout = &stdout
			cmd.Stderr = &stderr
			err := cmd.Run()
			var exit *exec.ExitError
			if !errors.As(err, &exit) {
				t.Fatalf("the stock start must reach the agent card, got %v\nstdout: %s\nstderr: %s", err, stdout.String(), stderr.String())
			}
			// The config dir carries the subtest name into every log
			// line, so the refusal checks read the diagnostics with the
			// path masked out.
			diagnostics := strings.ReplaceAll(stderr.String(), dir, "<configDir>")
			if !strings.Contains(diagnostics, ungatedStartupMessage) {
				t.Errorf("an ungated start must say so at startup, got:\n%s", diagnostics)
			}
			if strings.Contains(diagnostics, gatedStartupMessage) {
				t.Errorf("an ungated start must never claim the gated mode, got:\n%s", diagnostics)
			}
			if got := strings.Contains(diagnostics, ignoredRuntimeURLMessage); got != tc.ignoredURL {
				t.Errorf("the ignored runtime URL line: got %v, want %v, in:\n%s", got, tc.ignoredURL, diagnostics)
			}
			if strings.Contains(diagnostics, "Refusing to start") {
				t.Errorf("the guard must not run for an ungated agent, got:\n%s", diagnostics)
			}
			if strings.Contains(diagnostics, tc.refused) {
				t.Errorf("no refusal must name %s for an ungated agent, got:\n%s", tc.refused, diagnostics)
			}
			// The stock loader ran and got as far as the missing card.
			if !strings.Contains(diagnostics, "Failed to load agent config (model configuration is required)") ||
				!strings.Contains(diagnostics, "agent card") {
				t.Errorf("the stock loader must report the missing agent card, got:\n%s", diagnostics)
			}
		})
	}
}

// TestTheKnobRefusesAStartItCannotServe pins the two refusals the knob
// owns. Both run before the config load, so no case delivers a config.
// A refused start names no mode: the process ends instead.
func TestTheKnobRefusesAStartItCannotServe(t *testing.T) {
	binary := buildRuntime(t)
	cases := []struct {
		name string
		env  []string
		// want is what the diagnostic on stderr must name.
		want []string
	}{
		{name: "the knob is on and names no runtime", env: []string{"APPA_ENABLED=true"},
			want: []string{"APPA_ENABLED", "APPA_RUNTIME_URL"}},
		{name: "the knob is on and the runtime URL is blank", env: []string{"APPA_ENABLED=true", "APPA_RUNTIME_URL=   "},
			want: []string{"APPA_ENABLED", "APPA_RUNTIME_URL"}},
		{name: "a value outside the closed set", env: []string{"APPA_ENABLED=yes", "APPA_RUNTIME_URL=http://127.0.0.1:1"},
			want: []string{"APPA_ENABLED", "yes"}},
		{name: "a typo of true", env: []string{"APPA_ENABLED=ture"}, want: []string{"ture"}},
		{name: "a typo of false", env: []string{"APPA_ENABLED=flase"}, want: []string{"flase"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
			defer cancel()
			cmd := exec.CommandContext(ctx, binary)
			cmd.Env = append([]string{"PATH=" + os.Getenv("PATH")}, tc.env...)
			var stdout, stderr bytes.Buffer
			cmd.Stdout = &stdout
			cmd.Stderr = &stderr
			err := cmd.Run()
			var exit *exec.ExitError
			if !errors.As(err, &exit) {
				t.Fatalf("the runtime must exit before it serves, got %v\nstdout: %s\nstderr: %s", err, stdout.String(), stderr.String())
			}
			if exit.ExitCode() != 1 {
				t.Errorf("the refusal must exit 1, got %d\nstderr: %s", exit.ExitCode(), stderr.String())
			}
			for _, want := range tc.want {
				if !strings.Contains(stderr.String(), want) {
					t.Errorf("the diagnostic on stderr must name %s, got:\n%s", want, stderr.String())
				}
			}
			if strings.Contains(stderr.String(), gatedStartupMessage) || strings.Contains(stderr.String(), "UNGATED") {
				t.Errorf("a refused start must claim no mode, got:\n%s", stderr.String())
			}
			// The refusal runs before anything reads a config.
			for _, absent := range []string{"Failed to materialize agent config", "Failed to load agent config", "config.json"} {
				if strings.Contains(stderr.String(), absent) {
					t.Errorf("the knob refusal must precede the config load, got:\n%s", stderr.String())
				}
			}
		})
	}
}
