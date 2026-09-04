// AppaPluginKagent against a scripted /hook server.
//
// Every test follows one row of the go callback-to-event mapping table
// in integrations/kagent/IMPLEMENTATION.md: the callback fires, the
// plugin emits exactly the mapped wire event, and the answered
// decision is enforced in the go ADK's own terms — a returned map, a
// returned error, or a pass.

package appakagentadk

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"iter"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/plugin"
	"google.golang.org/adk/v2/runner"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
	"google.golang.org/adk/v2/tool/toolconfirmation"
)

var (
	ack   = map[string]any{"protocol": 1, "decision": "ack"}
	allow = map[string]any{"protocol": 1, "decision": "allow_call"}
)

// hook is the scripted runtime: answers in order, records every event.
// An int answer plays back as that HTTP status; a map answer plays
// back as a 200 decision envelope; an exhausted script answers ack.
//
// An answer registered for one event kind answers every event of that
// kind, before the ordered script is read. A run drives the liveness
// points through the same channel, and their pings consume an ordered
// script, so a whole-run test names the kind it is answering.
type hook struct {
	mu      sync.Mutex
	answers []any
	byKind  map[string]any
	events  []map[string]any
	server  *httptest.Server
}

func newHook(t *testing.T, answers ...any) *hook {
	t.Helper()
	h := &hook{answers: answers}
	h.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("the hook post must carry a body: %v", err)
		}
		var event map[string]any
		if err := json.Unmarshal(body, &event); err != nil {
			t.Errorf("every hook post must be one JSON event: %v", err)
		}
		h.mu.Lock()
		h.events = append(h.events, event)
		var answer any = ack
		kind, _ := event["event"].(string)
		if byKind, registered := h.byKind[kind]; registered {
			answer = byKind
		} else if len(h.answers) > 0 {
			answer = h.answers[0]
			h.answers = h.answers[1:]
		}
		h.mu.Unlock()
		if status, ok := answer.(int); ok {
			w.WriteHeader(status)
			fmt.Fprint(w, `{"error": "scripted"}`)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if err := json.NewEncoder(w).Encode(answer); err != nil {
			t.Errorf("the scripted answer must encode: %v", err)
		}
	}))
	t.Cleanup(h.server.Close)
	return h
}

// answering registers one answer for every event of a kind.
func (h *hook) answering(kind string, answer any) *hook {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.byKind == nil {
		h.byKind = map[string]any{}
	}
	h.byKind[kind] = answer
	return h
}

func (h *hook) recorded() []map[string]any {
	h.mu.Lock()
	defer h.mu.Unlock()
	return append([]map[string]any{}, h.events...)
}

func (h *hook) kinds() []string {
	var kinds []string
	for _, event := range h.recorded() {
		kinds = append(kinds, event["event"].(string))
	}
	return kinds
}

// testInventory spells the tools the tests dispatch: one MCP server
// named by its host, and two remote agents as kagent renders them.
func testInventory(t *testing.T) Inventory {
	t.Helper()
	inventory, err := BuildInventory(InventorySpec{
		MCPServers: []MCPServerSpec{{
			Path:  "http_tools[0]",
			URL:   "http://demo-tools.kagent.svc.cluster.local:3000/mcp",
			Tools: []string{"k8s_scale", "k8s_get_pods", "list_pods", "read_ledger", "k8s_annotate", "restart_deployment"},
		}},
		RemoteAgents: []RemoteAgentSpec{
			{Path: "remote_agents[0].name", Name: "kagent__NS__billing_agent"},
			{Path: "remote_agents[1].name", Name: "kagent__NS__log_analyst"},
		},
	})
	if err != nil {
		t.Fatalf("the test inventory must build: %v", err)
	}
	return inventory
}

func pluginOver(t *testing.T, h *hook) *AppaPluginKagent {
	t.Helper()
	p, err := New(Config{RuntimeURL: h.server.URL, Inventory: testInventory(t)})
	if err != nil {
		t.Fatalf("the plugin must construct: %v", err)
	}
	return p
}

// downPlugin points at a closed listener: every post is a transport
// failure.
func downPlugin(t *testing.T) *AppaPluginKagent {
	t.Helper()
	dead := httptest.NewServer(http.NotFoundHandler())
	url := dead.URL
	dead.Close()
	p, err := New(Config{RuntimeURL: url, Inventory: testInventory(t)})
	if err != nil {
		t.Fatalf("the plugin must construct: %v", err)
	}
	return p
}

// -- ADK-typed fakes ----------------------------------------------

type fakeState struct {
	values map[string]any
}

func (s *fakeState) Get(key string) (any, error) {
	value, ok := s.values[key]
	if !ok {
		return nil, session.ErrStateKeyNotExist
	}
	return value, nil
}

func (s *fakeState) Set(key string, value any) error {
	s.values[key] = value
	return nil
}

func (s *fakeState) All() iter.Seq2[string, any] {
	return func(yield func(string, any) bool) {
		for key, value := range s.values {
			if !yield(key, value) {
				return
			}
		}
	}
}

type fakeEvents struct {
	events []*session.Event
}

func (e *fakeEvents) All() iter.Seq[*session.Event] {
	return func(yield func(*session.Event) bool) {
		for _, event := range e.events {
			if !yield(event) {
				return
			}
		}
	}
}

func (e *fakeEvents) Len() int                { return len(e.events) }
func (e *fakeEvents) At(i int) *session.Event { return e.events[i] }

type fakeSession struct {
	id     string
	state  *fakeState
	events *fakeEvents
}

func newFakeSession(id string) *fakeSession {
	return &fakeSession{id: id, state: &fakeState{values: map[string]any{}}, events: &fakeEvents{}}
}

func (s *fakeSession) withHeaders(headers map[string]any) *fakeSession {
	s.state.values[headersStateKey] = headers
	return s
}

func (s *fakeSession) withContentEvent(text string) *fakeSession {
	s.events.events = append(s.events.events, &session.Event{
		LLMResponse: model.LLMResponse{Content: textContent(text)},
	})
	return s
}

func (s *fakeSession) withStateOnlyEvent() *fakeSession {
	s.events.events = append(s.events.events, &session.Event{})
	return s
}

func (s *fakeSession) ID() string                { return s.id }
func (s *fakeSession) AppName() string           { return "app" }
func (s *fakeSession) UserID() string            { return "u1" }
func (s *fakeSession) State() session.State      { return s.state }
func (s *fakeSession) Events() session.Events    { return s.events }
func (s *fakeSession) LastUpdateTime() time.Time { return time.Time{} }

// fakeContext stands in for the agent contexts every callback
// receives. StrictContextMock supplies the full surface; the plugin
// touches only the session, the agent, and the invocation id.
type fakeContext struct {
	agent.StrictContextMock
	session      session.Session
	agentName    string
	invocationID string
	// functionCallID is what adk-go pins on the tool context of one
	// call: the same value at the before-tool point, the tool, and the
	// after-tool point (agent.NewToolContext, base_flow.go).
	functionCallID string
	// The confirmation on a resumed call, and the requests this context saw.
	confirmation *toolconfirmation.ToolConfirmation
	requested    []requestedConfirmation
}

type requestedConfirmation struct {
	hint    string
	payload any
}

func (c *fakeContext) ToolConfirmation() *toolconfirmation.ToolConfirmation { return c.confirmation }

func (c *fakeContext) RequestConfirmation(hint string, payload any) error {
	c.requested = append(c.requested, requestedConfirmation{hint: hint, payload: payload})
	return nil
}

func (c *fakeContext) resumed(confirmed bool) *fakeContext {
	c.confirmation = &toolconfirmation.ToolConfirmation{Confirmed: confirmed}
	return c
}

func newFakeContext(sess session.Session) *fakeContext {
	return &fakeContext{
		StrictContextMock: agent.NewStrictContextMock(context.Background()),
		session:           sess,
		agentName:         "root-agent",
		invocationID:      "i1",
		functionCallID:    "fc-1",
	}
}

func (c *fakeContext) forAgent(name string) *fakeContext {
	c.agentName = name
	return c
}

func (c *fakeContext) forInvocation(id string) *fakeContext {
	c.invocationID = id
	return c
}

// forCall is the tool context of another function call. Two contexts
// that carry one id are one call, as adk-go builds them.
func (c *fakeContext) forCall(id string) *fakeContext {
	c.functionCallID = id
	return c
}

func (c *fakeContext) Session() session.Session { return c.session }
func (c *fakeContext) InvocationID() string     { return c.invocationID }
func (c *fakeContext) AgentName() string        { return c.agentName }
func (c *fakeContext) FunctionCallID() string   { return c.functionCallID }

// strictContext is what adk-go hands the tool and agent callbacks: a
// context that refuses Session() and Agent() (it logs and returns nil)
// but answers InvocationID() and AgentName().
type strictContext struct {
	*fakeContext
}

func (c strictContext) Session() session.Session { return nil }
func (c strictContext) Agent() agent.Agent       { return nil }

func strict(c *fakeContext) strictContext { return strictContext{c} }

func (c *fakeContext) Agent() agent.Agent {
	built, err := agent.New(agent.Config{
		Name: c.agentName,
		Run: func(agent.InvocationContext) iter.Seq2[*session.Event, error] {
			return func(func(*session.Event, error) bool) {}
		},
	})
	if err != nil {
		panic(err)
	}
	return built
}

type fakeTool struct {
	name string
}

func (t *fakeTool) Name() string        { return t.name }
func (t *fakeTool) Description() string { return "" }
func (t *fakeTool) IsLongRunning() bool { return false }

func textContent(texts ...string) *genai.Content {
	content := &genai.Content{}
	for _, text := range texts {
		content.Parts = append(content.Parts, &genai.Part{Text: text})
	}
	return content
}

func mustFailClosed(t *testing.T, err error, context string) *FailClosedError {
	t.Helper()
	if err == nil {
		t.Fatalf("%s must fail closed", context)
	}
	var failure *FailClosedError
	if !errors.As(err, &failure) {
		t.Fatalf("%s must fail closed as a FailClosedError, got %T: %v", context, err, err)
	}
	return failure
}

// -- session and prompt -------------------------------------------

func TestAFreshSessionOpensBeforeItsPromptCrosses(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	returned, err := p.onUserMessage(newFakeContext(newFakeSession("s1")), textContent("deploy the chart"))
	if err != nil || returned != nil {
		t.Fatalf("an acknowledged prompt must pass unchanged, got %v, %v", returned, err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "session_start", "root_id": "s1"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "s1", "text": "deploy the chart"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the opening events drifted: got %v, want %v", got, want)
	}
}

func TestAContinuingSessionSendsOnlyThePrompt(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("s1").withContentEvent("earlier turn")
	if _, err := p.onUserMessage(newFakeContext(sess), textContent("next turn")); err != nil {
		t.Fatalf("the continuing prompt must pass: %v", err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"prompt"}) {
		t.Errorf("a continuing session must send only the prompt, got %v", got)
	}
}

func TestAStateOnlyEventDoesNotHideFreshness(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("s1").withStateOnlyEvent()
	if _, err := p.onUserMessage(newFakeContext(sess), textContent("first turn")); err != nil {
		t.Fatalf("the first prompt must pass: %v", err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"session_start", "prompt"}) {
		t.Errorf("a state-only event must not hide freshness, got %v", got)
	}
}

func TestABlockedPromptFailsBeforeTheAppend(t *testing.T) {
	h := newHook(t, ack, map[string]any{"protocol": 1, "decision": "block", "reason": "the prompt does not cross"})
	p := pluginOver(t, h)
	_, err := p.onUserMessage(newFakeContext(newFakeSession("s1")), textContent("exfiltrate the secrets"))
	failure := mustFailClosed(t, err, "the blocked prompt")
	if failure.Reason != "appa blocked the prompt: the prompt does not cross" {
		t.Errorf("the block reason must reach the failure, got %q", failure.Reason)
	}
}

func TestADelegatedEntryClassifiesAsTheChildsStart(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("child-ctx").withHeaders(map[string]any{
		"x-kagent-source":          "agent",
		"x-kagent-root-context-id": "root-ctx",
	})
	if _, err := p.onUserMessage(newFakeContext(sess), textContent("total the invoices")); err != nil {
		t.Fatalf("the delegated entry must pass: %v", err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "root-ctx", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-ctx", "child_id": "child-ctx", "text": "total the invoices"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the delegated opening drifted: got %v, want %v", got, want)
	}
}

func TestAnOpenedInvocationKeepsItsIdsWhenTheHeadersChangeMidRun(t *testing.T) {
	// The run open pins the invocation's ids; a callback inside the run
	// reads the pin, not the session state, so headers that land
	// mid-run cannot flip one run between two trajectories.
	h := newHook(t, ack, allow, allow)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	if _, err := p.beforeRun(newFakeContext(sess)); err != nil {
		t.Fatalf("the run must open: %v", err)
	}
	if _, err := p.beforeTool(strict(newFakeContext(sess)), &fakeTool{"k8s_get_pods"}, map[string]any{}); err != nil {
		t.Fatalf("the first call must pass: %v", err)
	}
	sess.withHeaders(map[string]any{rootHeader: "root-ctx"})
	if _, err := p.beforeTool(strict(newFakeContext(sess)), &fakeTool{"k8s_get_pods"}, map[string]any{}); err != nil {
		t.Fatalf("the second call must pass: %v", err)
	}
	// The turn end reads the same pin: it closes the turn the prompt and
	// the tool calls ran in, not the trajectory the session state names now.
	p.afterRun(newFakeContext(sess))
	events := h.recorded()
	for _, event := range events[1:] {
		if event["root_id"] != "s1" || event["child_id"] != nil {
			t.Errorf("headers that land mid-run must not flip the invocation, got %v", event)
		}
	}
	if last := events[len(events)-1]; last["event"] != "turn_end" || last["root_id"] != "s1" || last["child_id"] != nil {
		t.Errorf("the turn end must carry the pinned root and no child id, got %v", last)
	}
}

// -- one child session id, many parents ----------------------------
//
// kagent's go remote-agent tool mints one child context id per parent
// pod and sends every delegation into it, so the child pod's ADK
// session id is the same for every parent. The runtime main lands each
// request's lineage headers in session state before its run; the plugin
// opens the (root, child) pair it reads then.

func TestEachParentOpensTheSharedChildSessionUnderItsOwnRoot(t *testing.T) {
	h := newHook(t, ack, ack, allow, ack, ack, ack, allow, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("child-ctx").withHeaders(map[string]any{rootHeader: "root-1"})
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i1"), textContent("total the invoices")); err != nil {
		t.Fatalf("the first parent's delegation must pass: %v", err)
	}
	if _, err := p.beforeTool(strict(newFakeContext(sess).forInvocation("i1")), &fakeTool{"read_ledger"}, map[string]any{}); err != nil {
		t.Fatalf("the first parent's tool call must pass: %v", err)
	}
	p.afterRun(newFakeContext(sess).forInvocation("i1"))
	// The child session now carries content, and the next parent's
	// headers land before its run.
	sess.withContentEvent("total the invoices").withHeaders(map[string]any{rootHeader: "root-2"})
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i2"), textContent("list the pods")); err != nil {
		t.Fatalf("the second parent's delegation must pass: %v", err)
	}
	if _, err := p.beforeTool(strict(newFakeContext(sess).forInvocation("i2")), &fakeTool{"k8s_get_pods"}, map[string]any{}); err != nil {
		t.Fatalf("the second parent's tool call must pass: %v", err)
	}
	p.afterRun(newFakeContext(sess).forInvocation("i2"))
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "root-1", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-1", "child_id": "child-ctx", "text": "total the invoices"},
		{"protocol": float64(1), "adapter": "kagent", "event": "tool_call", "root_id": "root-1", "child_id": "child-ctx", "tool": "mcp:demo-tools/read_ledger", "arguments": map[string]any{}},
		{"protocol": float64(1), "adapter": "kagent", "event": "turn_end", "root_id": "root-1", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "root-2", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-2", "child_id": "child-ctx", "text": "list the pods"},
		{"protocol": float64(1), "adapter": "kagent", "event": "tool_call", "root_id": "root-2", "child_id": "child-ctx", "tool": "mcp:demo-tools/k8s_get_pods", "arguments": map[string]any{}},
		{"protocol": float64(1), "adapter": "kagent", "event": "turn_end", "root_id": "root-2", "child_id": "child-ctx"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("each parent must open and drive the shared child session under its own root: got %v, want %v", got, want)
	}
}

func TestTheSameParentSendsNoSecondChildStart(t *testing.T) {
	// The plugin's side of a re-entry: the pair is open, so the second
	// delegation from the same parent sends only its prompt. The
	// runtime, not the plugin, decides what that second delegation
	// gets back.
	h := newHook(t, ack, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("child-ctx").withHeaders(map[string]any{rootHeader: "root-1"})
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i1"), textContent("total the invoices")); err != nil {
		t.Fatalf("the first delegation must pass: %v", err)
	}
	sess.withContentEvent("total the invoices").withHeaders(map[string]any{rootHeader: "root-1"})
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i2"), textContent("now the refunds")); err != nil {
		t.Fatalf("the second delegation must pass: %v", err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "root-1", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-1", "child_id": "child-ctx", "text": "total the invoices"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-1", "child_id": "child-ctx", "text": "now the refunds"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("an opened pair sends no second child_start: got %v, want %v", got, want)
	}
}

func TestARefusedChildStartFailsClosedAndTheNextEntryOpensAgain(t *testing.T) {
	// The pair joins the opened set only after the runtime acked, so a
	// refused opening fails the entry closed and is sent again on the
	// next entry.
	h := newHook(t, map[string]any{"protocol": 1, "decision": "refuse", "detail": "storage failure"}, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("child-ctx").withHeaders(map[string]any{rootHeader: "root-1"})
	_, err := p.onUserMessage(newFakeContext(sess).forInvocation("i1"), textContent("total the invoices"))
	failure := mustFailClosed(t, err, "the refused child start")
	if failure.Reason != "appa refused the session: storage failure" {
		t.Errorf("the refusal detail must reach the failure, got %q", failure.Reason)
	}
	// Content another run landed in the shared child session between the
	// two entries must not hide the unopened pair: the opened set
	// decides for a delegated pair, not freshness.
	sess.withContentEvent("total the invoices")
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i2"), textContent("total the invoices")); err != nil {
		t.Fatalf("the next entry must open the pair: %v", err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"child_start", "child_start", "prompt"}) {
		t.Errorf("the next entry after a refusal must send its own child_start, got %v", got)
	}
}

func TestARootSessionStillOpensOnceAtItsFirstContent(t *testing.T) {
	// The opened set is for delegated pairs; a root session keeps the
	// freshness rule across its runs.
	h := newHook(t, ack, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i1"), textContent("first turn")); err != nil {
		t.Fatalf("the first turn must pass: %v", err)
	}
	sess.withContentEvent("first turn")
	if _, err := p.onUserMessage(newFakeContext(sess).forInvocation("i2"), textContent("second turn")); err != nil {
		t.Fatalf("the second turn must pass: %v", err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"session_start", "prompt", "prompt"}) {
		t.Errorf("a root session opens once and then sends prompts, got %v", got)
	}
}

// -- the tool gate ------------------------------------------------

func TestAnAllowedCallPassesAndADeniedCallAnswersTheModel(t *testing.T) {
	h := newHook(t, allow, map[string]any{"protocol": 1, "decision": "deny_call", "feedback": "blocked: quotes offer offer-1"})
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	allowed, err := p.beforeTool(ctx, &fakeTool{"k8s_scale"}, map[string]any{"replicas": 3})
	if err != nil || allowed != nil {
		t.Fatalf("an allowed call must pass untouched, got %v, %v", allowed, err)
	}
	deniedResult, err := p.beforeTool(ctx, &fakeTool{"k8s_scale"}, map[string]any{"replicas": 30})
	if err != nil {
		t.Fatalf("a denied call must answer the model, not fail: %v", err)
	}
	wantDeny := map[string]any{"result": "blocked: quotes offer offer-1", denyKey: denied}
	if !reflect.DeepEqual(deniedResult, wantDeny) {
		t.Errorf("the deny map drifted: got %v, want %v", deniedResult, wantDeny)
	}
	wantEvent := map[string]any{
		"protocol":  float64(1),
		"adapter":   "kagent",
		"event":     "tool_call",
		"root_id":   "s1",
		"tool":      "mcp:demo-tools/k8s_scale",
		"arguments": map[string]any{"replicas": float64(3)},
	}
	if got := h.recorded()[0]; !reflect.DeepEqual(got, wantEvent) {
		t.Errorf("the tool_call event drifted: got %v, want %v", got, wantEvent)
	}
}

func TestEveryToolCrossesUnderItsInventorySpellingAndAssertsNoSpawn(t *testing.T) {
	// The wire carries the structured spelling of the inventory and no
	// spawn flag: the runtime derives both the canonical tool and
	// whether the call is a spawn from the spelling.
	h := newHook(t, allow, allow, allow)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	for _, name := range []string{"kagent__NS__billing_agent", "k8s_scale", "ask_user"} {
		if _, err := p.beforeTool(ctx, &fakeTool{name}, map[string]any{}); err != nil {
			t.Fatalf("the %s call must pass: %v", name, err)
		}
	}
	var tools []string
	for _, event := range h.recorded() {
		tools = append(tools, event["tool"].(string))
		if _, present := event["spawn"]; present {
			t.Errorf("the wire asserts no spawn, got %v", event)
		}
		if event["protocol"] != float64(1) || event["adapter"] != "kagent" {
			t.Errorf("every event carries the envelope, got %v", event)
		}
	}
	want := []string{"agent:kagent/billing-agent", "mcp:demo-tools/k8s_scale", "builtin:ask_user"}
	if !reflect.DeepEqual(tools, want) {
		t.Errorf("the spellings drifted: got %v, want %v", tools, want)
	}
}

func TestAToolOutsideTheInventoryIsRefusedAtTheGateAndNeverForwarded(t *testing.T) {
	// A name the rendered config never declared has no spelling, so the
	// plugin answers the call itself with a deny and posts nothing. The
	// result gate then reads the answered call and reports nothing.
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	unknown := &fakeTool{"k8s_delete_namespace"}
	denied, err := p.beforeTool(ctx, unknown, map[string]any{"name": "prod"})
	if err != nil {
		t.Fatalf("the refusal answers the model, not the harness: %v", err)
	}
	if denied[denyKey] != "denied" || !strings.Contains(denied["result"].(string), "k8s_delete_namespace") {
		t.Errorf("the refusal names the tool under the deny marker, got %v", denied)
	}
	if reported, err := p.afterTool(ctx, unknown, map[string]any{"name": "prod"}, denied, nil); err != nil || reported != nil {
		t.Errorf("the answered call reports nothing, got %v, %v", reported, err)
	}
	if got := h.recorded(); len(got) != 0 {
		t.Errorf("nothing crosses for a name the inventory does not carry, got %v", got)
	}
}

func TestAResultOfAToolOutsideTheInventoryFailsClosed(t *testing.T) {
	p := pluginOver(t, newHook(t))
	unknown := &fakeTool{"k8s_delete_namespace"}
	_, err := p.afterTool(newFakeContext(newFakeSession("s1")), unknown, map[string]any{}, map[string]any{"deleted": true}, nil)
	mustFailClosed(t, err, "the result of an undeclared tool")
	_, err = p.onToolError(newFakeContext(newFakeSession("s1")), unknown, map[string]any{}, errors.New("boom"))
	mustFailClosed(t, err, "the failure of an undeclared tool")
}

func TestADecisionUnderAnotherProtocolFailsClosed(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 2, "decision": "allow_call"})
	p := pluginOver(t, h)
	_, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"}, map[string]any{})
	failure := mustFailClosed(t, err, "a decision under another protocol")
	if !strings.Contains(failure.Error(), "protocol") {
		t.Errorf("the failure names the protocol, got %v", failure)
	}
}

func TestTheReservedToolPassesControl(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 1, "decision": "pass_control"})
	p := pluginOver(t, h)
	returned, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{ReservedTool}, map[string]any{"offer_id": "offer-1"})
	if err != nil || returned != nil {
		t.Fatalf("pass_control must let the call through to /mcp untouched, got %v, %v", returned, err)
	}
}

func TestADenyMapIsNotReportedTwice(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 1, "decision": "deny_call", "feedback": "blocked"})
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	blocked, err := p.beforeTool(newFakeContext(sess).forCall("fc-7"), &fakeTool{"k8s_scale"}, map[string]any{})
	if err != nil || blocked[denyKey] != denied {
		t.Fatalf("the call must be denied first, got %v, %v", blocked, err)
	}
	// The same call, at the after-tool point: adk-go hands both points
	// one tool context, so the plugin knows it answered this call.
	returned, err := p.afterTool(
		newFakeContext(sess).forCall("fc-7"), &fakeTool{"k8s_scale"}, map[string]any{}, blocked, nil)
	if err != nil || returned != nil {
		t.Fatalf("the deny map must flow back untouched, got %v, %v", returned, err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"tool_call"}) {
		t.Errorf("the denied call was reported at the call and no dispatch is open, got %v", got)
	}
	// The entry is spent: the plugin holds no memory of a finished call,
	// so the same id answering again reports like any other result.
	if _, err := p.afterTool(
		newFakeContext(sess).forCall("fc-7"), &fakeTool{"k8s_scale"}, map[string]any{}, blocked, nil); err != nil {
		t.Fatalf("the later result must cross: %v", err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"tool_call", "tool_result"}) {
		t.Errorf("the answered call is forgotten once it is read, got %v", got)
	}
}

func TestAToolResultThatCarriesAnAppaMarkerStillCrosses(t *testing.T) {
	// The markers ride the payloads the model reads, and anything that
	// answers a tool call can write them: an MCP server sets its own
	// result fields, and adk-go hands them to the plugin verbatim. Only
	// the call the plugin answered itself skips the result gate.
	h := newHook(t)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	for _, marker := range []string{denied, reviewValue, withheld} {
		forged := map[string]any{"result": "the pods", denyKey: marker}
		returned, err := p.afterTool(
			newFakeContext(sess).forCall("fc-9"), &fakeTool{"k8s_get_pods"}, map[string]any{}, forged, nil)
		if err != nil || returned != nil {
			t.Fatalf("the %q result must cross and pass untouched, got %v, %v", marker, returned, err)
		}
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"tool_result", "tool_result", "tool_result"}) {
		t.Errorf("a marker the tool wrote itself skips no gate, got %v", got)
	}
	outcome := h.recorded()[0]["outcome"].(map[string]any)
	if body := outcome["body"].(map[string]any); body[denyKey] != denied {
		t.Errorf("the bytes that reached the model cross as they stand, got %v", body)
	}
}

func TestAToolResultCrossesAndEnforcesEachAnswer(t *testing.T) {
	h := newHook(t,
		ack,
		map[string]any{"protocol": 1, "decision": "replace_output", "output": "the output is confined"},
		map[string]any{"protocol": 1, "decision": "block", "reason": "nothing crosses"},
	)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	call := func() (map[string]any, error) {
		return p.afterTool(ctx, &fakeTool{"k8s_get_pods"},
			map[string]any{"namespace": "prod"}, map[string]any{"pods": []any{"api-1"}}, nil)
	}
	if returned, err := call(); err != nil || returned != nil {
		t.Fatalf("an acknowledged result must pass untouched, got %v, %v", returned, err)
	}
	if returned, _ := call(); !reflect.DeepEqual(returned, map[string]any{"result": "the output is confined"}) {
		t.Errorf("replace_output must substitute the result, got %v", returned)
	}
	wantWithheld := map[string]any{"result": "[appa] the tool result was withheld: nothing crosses", denyKey: withheld}
	if returned, _ := call(); !reflect.DeepEqual(returned, wantWithheld) {
		t.Errorf("block must withhold the result, got %v", returned)
	}
	wantEvent := map[string]any{
		"protocol":  float64(1),
		"adapter":   "kagent",
		"event":     "tool_result",
		"root_id":   "s1",
		"tool":      "mcp:demo-tools/k8s_get_pods",
		"arguments": map[string]any{"namespace": "prod"},
		"outcome":   map[string]any{"status": "success", "body": map[string]any{"pods": []any{"api-1"}}},
	}
	if got := h.recorded()[0]; !reflect.DeepEqual(got, wantEvent) {
		t.Errorf("the tool_result event drifted: got %v, want %v", got, wantEvent)
	}
}

func TestASpawnReturnCrossesAsTheSpawnResultInBothReplyShapes(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	if _, err := p.afterTool(ctx, &fakeTool{"kagent__NS__billing_agent"},
		map[string]any{"request": "total the invoices"},
		map[string]any{"result": "the total is 42", "subagent_session_id": "child-ctx"}, nil); err != nil {
		t.Fatalf("the task reply must cross: %v", err)
	}
	if _, err := p.afterTool(ctx, &fakeTool{"kagent__NS__billing_agent"},
		map[string]any{"request": "go"},
		map[string]any{"error": "Remote agent 'billing-agent' failed."}, nil); err != nil {
		t.Fatalf("the failure reply must cross: %v", err)
	}
	events := h.recorded()
	taskReply, failureReply := events[0], events[1]
	if taskReply["event"] != "spawn_result" || taskReply["spawned_id"] != "child-ctx" || taskReply["value"] != "the total is 42" {
		t.Errorf("the task reply drifted: got %v", taskReply)
	}
	if failureReply["event"] != "spawn_result" {
		t.Errorf("the failure reply must still cross as a spawn_result, got %v", failureReply)
	}
	if _, present := failureReply["spawned_id"]; present {
		t.Errorf("a reply without a child id must carry no spawned_id, got %v", failureReply)
	}
	if _, present := failureReply["value"]; present {
		t.Errorf("a reply without a result must carry no value, got %v", failureReply)
	}
}

func TestAChildReturnSubstitutesWhatTheParentReceives(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 1, "decision": "child_return", "value": "the redacted summary"})
	p := pluginOver(t, h)
	returned, err := p.afterTool(
		newFakeContext(newFakeSession("s1")), &fakeTool{"kagent__NS__billing_agent"},
		map[string]any{},
		map[string]any{"result": "the raw child answer", "subagent_session_id": "child-ctx"}, nil)
	if err != nil {
		t.Fatalf("the child return must substitute, not fail: %v", err)
	}
	if !reflect.DeepEqual(returned, map[string]any{"result": "the redacted summary"}) {
		t.Errorf("child_return must substitute what the parent receives, got %v", returned)
	}
}

func TestAToolFailureCrossesAsAFailureOutcome(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	returned, err := p.onToolError(
		newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"},
		map[string]any{"replicas": 3}, errors.New("connection refused"))
	if err != nil || returned != nil {
		t.Fatalf("an acknowledged failure propagates the original error, got %v, %v", returned, err)
	}
	wantOutcome := map[string]any{"status": "failure", "message": "connection refused"}
	if got := h.recorded()[0]["outcome"]; !reflect.DeepEqual(got, wantOutcome) {
		t.Errorf("the failure outcome drifted: got %v, want %v", got, wantOutcome)
	}
}

func TestThePluginsOwnGateErrorIsNotReportedAsAToolFailure(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	returned, err := p.onToolError(
		newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"},
		map[string]any{}, failClosed("appa answered the tool call with refuse"))
	if err != nil || returned != nil {
		t.Fatalf("the plugin's own gate error must stay terminal untouched, got %v, %v", returned, err)
	}
	if len(h.recorded()) != 0 {
		t.Errorf("the gate already reported at the call; no failure event may follow, got %v", h.recorded())
	}
}

func TestTheErrorPathDoesNotDoubleReportAtTheAfterToolPoint(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	returned, err := p.afterTool(
		newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"},
		map[string]any{}, nil, errors.New("boom"))
	if err != nil || returned != nil {
		t.Fatalf("the after-tool point on the error path must pass through, got %v, %v", returned, err)
	}
	if len(h.recorded()) != 0 {
		t.Errorf("the failure already crossed at the error callback, got %v", h.recorded())
	}
}

func TestADeferredResultCrossesAsIndeterminate(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	if _, err := p.afterTool(
		newFakeContext(newFakeSession("s1")), &fakeTool{"ask_user"},
		map[string]any{}, nil, nil); err != nil {
		t.Fatalf("a deferred result must cross: %v", err)
	}
	wantOutcome := map[string]any{"status": "indeterminate"}
	if got := h.recorded()[0]["outcome"]; !reflect.DeepEqual(got, wantOutcome) {
		t.Errorf("a nil result with no error is an unresolved dispatch: got %v", got)
	}
}

// -- agent scopes -------------------------------------------------

func TestAChildScopeOpensAndEndsThroughTheAgentCallbacks(t *testing.T) {
	h := newHook(t, ack, ack, ack, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	if _, err := p.beforeAgent(newFakeContext(sess).forAgent("root-agent")); err != nil {
		t.Fatalf("the invocation's own scope must ping: %v", err)
	}
	if _, err := p.beforeAgent(newFakeContext(sess).forAgent("billing-agent")); err != nil {
		t.Fatalf("the child scope must open: %v", err)
	}
	if _, err := p.afterAgent(newFakeContext(sess).forAgent("billing-agent")); err != nil {
		t.Fatalf("the child scope must end quietly: %v", err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "ping"},
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "s1", "child_id": "i1:billing-agent"},
		{"protocol": float64(1), "adapter": "kagent", "event": "turn_end", "root_id": "s1", "child_id": "i1:billing-agent"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the agent-scope events drifted: got %v, want %v", got, want)
	}
}

func TestARefusedChildScopeFailsClosed(t *testing.T) {
	h := newHook(t, ack, map[string]any{"protocol": 1, "decision": "refuse", "detail": "storage failure"})
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	if _, err := p.beforeAgent(newFakeContext(sess).forAgent("root-agent")); err != nil {
		t.Fatalf("the own scope must ping: %v", err)
	}
	_, err := p.beforeAgent(newFakeContext(sess).forAgent("billing-agent"))
	failure := mustFailClosed(t, err, "the refused child scope")
	if failure.Reason != "appa refused the child scope: storage failure" {
		t.Errorf("the refusal detail must reach the failure, got %q", failure.Reason)
	}
}

// -- liveness gates -----------------------------------------------

func TestEveryLivenessGateHoldsWhenTheChannelIsDown(t *testing.T) {
	p := downPlugin(t)
	sess := newFakeSession("s1")
	ctx := newFakeContext(sess)
	gates := map[string]func() error{
		"before_run": func() error { _, err := p.beforeRun(ctx); return err },
		"on_event":   func() error { _, err := p.onEvent(ctx, &session.Event{}); return err },
		"before_model": func() error {
			_, err := p.beforeModel(ctx, &model.LLMRequest{})
			return err
		},
		"after_model": func() error {
			_, err := p.afterModel(ctx, &model.LLMResponse{}, nil)
			return err
		},
		"on_model_error": func() error {
			_, err := p.onModelError(ctx, &model.LLMRequest{}, errors.New("model died"))
			return err
		},
		"before_agent_own": func() error { _, err := p.beforeAgent(newFakeContext(sess)); return err },
	}
	for name, gate := range gates {
		mustFailClosed(t, gate(), "the "+name+" liveness gate")
	}
}

func TestEveryLivenessGatePassesWhenTheChannelAnswers(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	if _, err := p.beforeRun(ctx); err != nil {
		t.Fatalf("before_run must pass on a live channel: %v", err)
	}
	if _, err := p.beforeModel(ctx, &model.LLMRequest{}); err != nil {
		t.Fatalf("before_model must pass on a live channel: %v", err)
	}
	if _, err := p.onEvent(ctx, &session.Event{}); err != nil {
		t.Fatalf("on_event must pass on a live channel: %v", err)
	}
	for _, event := range h.recorded() {
		if !reflect.DeepEqual(event, map[string]any{"protocol": float64(1), "adapter": "kagent", "event": "ping"}) {
			t.Errorf("a liveness gate must send only pings, got %v", event)
		}
	}
}

// -- fail closed --------------------------------------------------

func TestAGatedCallbackFailsClosedOnTransportStatusAndContract(t *testing.T) {
	call := func(p *AppaPluginKagent) error {
		_, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"}, map[string]any{})
		return err
	}
	mustFailClosed(t, call(downPlugin(t)), "the downed channel")
	for _, answer := range []any{
		409,
		500,
		map[string]any{"protocol": 1, "decision": "approve"},
		map[string]any{"protocol": 1, "decision": "deny_call"},
	} {
		p := pluginOver(t, newHook(t, answer))
		mustFailClosed(t, call(p), fmt.Sprintf("the %v answer", answer))
	}
}

// -- turn ends ----------------------------------------------------

func TestATurnEndReportsAndNeverBlocks(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	p.afterRun(newFakeContext(newFakeSession("s1")))
	want := []map[string]any{{"protocol": float64(1), "adapter": "kagent", "event": "turn_end", "root_id": "s1"}}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the turn end drifted: got %v, want %v", got, want)
	}
	// A downed channel must not panic or fail a finished turn.
	downPlugin(t).afterRun(newFakeContext(newFakeSession("s1")))
}

func TestADelegatedChildsTurnEndCarriesItsChildID(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("child-ctx").withHeaders(map[string]any{rootHeader: "root-ctx"})
	p.afterRun(newFakeContext(sess))
	want := []map[string]any{{"protocol": float64(1), "adapter": "kagent", "event": "turn_end", "root_id": "root-ctx", "child_id": "child-ctx"}}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the delegated turn end drifted: got %v, want %v", got, want)
	}
}

// -- the installed ADK --------------------------------------------

func TestTheADKPluginSurfaceCarriesEveryCallback(t *testing.T) {
	// The per-version equivalence check: plugin.New accepts every
	// callback under the adk/v2 signatures, and the accessor-wired
	// callbacks reach the same gates.
	h := newHook(t, allow)
	p := pluginOver(t, h)
	adkPlugin, err := p.ADKPlugin()
	if err != nil {
		t.Fatalf("the adk plugin must construct: %v", err)
	}
	if adkPlugin.Name() != "appa_plugin_kagent" {
		t.Errorf("the plugin name drifted: %q", adkPlugin.Name())
	}
	returned, err := adkPlugin.BeforeToolCallback()(
		newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"}, map[string]any{"replicas": 3})
	if err != nil || returned != nil {
		t.Fatalf("the wired before-tool callback must pass an allowed call, got %v, %v", returned, err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"tool_call"}) {
		t.Errorf("the wired callback must reach the tool gate, got %v", got)
	}
	for name, callback := range map[string]any{
		"OnUserMessageCallback": adkPlugin.OnUserMessageCallback(),
		"OnEventCallback":       adkPlugin.OnEventCallback(),
		"BeforeRunCallback":     adkPlugin.BeforeRunCallback(),
		"AfterRunCallback":      adkPlugin.AfterRunCallback(),
		"BeforeAgentCallback":   adkPlugin.BeforeAgentCallback(),
		"AfterAgentCallback":    adkPlugin.AfterAgentCallback(),
		"BeforeModelCallback":   adkPlugin.BeforeModelCallback(),
		"AfterModelCallback":    adkPlugin.AfterModelCallback(),
		"OnModelErrorCallback":  adkPlugin.OnModelErrorCallback(),
		"BeforeToolCallback":    adkPlugin.BeforeToolCallback(),
		"AfterToolCallback":     adkPlugin.AfterToolCallback(),
		"OnToolErrorCallback":   adkPlugin.OnToolErrorCallback(),
	} {
		if reflect.ValueOf(callback).IsNil() {
			t.Errorf("the %s must be wired", name)
		}
	}
}

// -- human review through kagent's own confirmation -------------------

const reviewText = "APPA asks you to rule as the authority \"oncall\".\n\nTool: restart_deployment"

func denyWithReview() map[string]any {
	return map[string]any{
		"protocol": 1,
		"decision": "deny_call",
		"feedback": "[appa] Blocked",
		"review":   []any{map[string]any{"offer_id": "offer-1", "text": reviewText}},
	}
}

func TestAReviewedOfferAsksThePersonBeforeTheControlCallCrosses(t *testing.T) {
	h := newHook(t, denyWithReview())
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	blocked, err := p.beforeTool(newFakeContext(sess), &fakeTool{name: "restart_deployment"}, map[string]any{"name": "checkout-api"})
	if err != nil || blocked[denyKey] != denied {
		t.Fatalf("the call is denied first: %v %v", blocked, err)
	}
	ctx := newFakeContext(sess)
	pending, err := p.beforeTool(ctx, &fakeTool{name: ReservedTool}, map[string]any{"offer_id": "offer-1"})
	if err != nil || pending[denyKey] != reviewValue {
		t.Fatalf("the reviewed control call waits for the person: %v %v", pending, err)
	}
	if len(ctx.requested) != 1 || ctx.requested[0].hint != reviewText {
		t.Fatalf("the person sees the consult artifact the runtime rendered: %+v", ctx.requested)
	}
	if kinds := h.kinds(); len(kinds) != 1 || kinds[0] != "tool_call" {
		t.Fatalf("the control call did not cross yet: %v", kinds)
	}
}

func TestTheResumedControlCallCarriesThePersonsRuling(t *testing.T) {
	for _, tc := range []struct {
		confirmed bool
		ruling    string
	}{{true, "approve"}, {false, "deny"}} {
		h := newHook(t, denyWithReview(), map[string]any{"protocol": 1, "decision": "pass_control"}, map[string]any{"protocol": 1, "decision": "pass_control"})
		p := pluginOver(t, h)
		sess := newFakeSession("s1")
		if _, err := p.beforeTool(newFakeContext(sess), &fakeTool{name: "restart_deployment"}, map[string]any{"name": "checkout-api"}); err != nil {
			t.Fatal(err)
		}
		ctx := newFakeContext(sess).resumed(tc.confirmed)
		returned, err := p.beforeTool(ctx, &fakeTool{name: ReservedTool}, map[string]any{"offer_id": "offer-1"})
		if err != nil || returned != nil {
			t.Fatalf("the ruled call passes to /mcp: %v %v", returned, err)
		}
		if len(ctx.requested) != 0 {
			t.Fatalf("a resumed call asks nobody again: %+v", ctx.requested)
		}
		events := h.recorded()
		last := events[len(events)-1]
		if last["tool"] != ControlTool || last["ruling"] != tc.ruling {
			t.Fatalf("the answer rides the control call, never through the model: %v", last)
		}
		// The ruling is spent: quoted again, the offer asks nobody and carries nothing.
		again := newFakeContext(sess)
		if _, err := p.beforeTool(again, &fakeTool{name: ReservedTool}, map[string]any{"offer_id": "offer-1"}); err != nil {
			t.Fatal(err)
		}
		events = h.recorded()
		if _, has := events[len(events)-1]["ruling"]; has || len(again.requested) != 0 {
			t.Fatalf("a spent ruling neither asks nor rides again: %v %+v", events[len(events)-1], again.requested)
		}
	}
}

func TestAControlCallForAnOfferNeedingNoPersonNeverAsks(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 1, "decision": "pass_control"})
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	returned, err := p.beforeTool(ctx, &fakeTool{name: ReservedTool}, map[string]any{"offer_id": "offer-9"})
	if err != nil || returned != nil || len(ctx.requested) != 0 {
		t.Fatalf("an ordinary remedy is the agent's to take: %v %v %+v", returned, err, ctx.requested)
	}
	if _, has := h.recorded()[0]["ruling"]; has {
		t.Fatalf("no ruling on an unreviewed control call: %v", h.recorded()[0])
	}
}

func TestTheReviewMapIsNotReportedAsAResult(t *testing.T) {
	h := newHook(t, denyWithReview())
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	if _, err := p.beforeTool(newFakeContext(sess), &fakeTool{name: "restart_deployment"}, map[string]any{}); err != nil {
		t.Fatal(err)
	}
	arguments := map[string]any{"offer_id": "offer-1"}
	pending, err := p.beforeTool(newFakeContext(sess).forCall("fc-3"), &fakeTool{name: ReservedTool}, arguments)
	if err != nil || pending[denyKey] != reviewValue {
		t.Fatalf("the reviewed control call waits for the person: %v %v", pending, err)
	}
	returned, err := p.afterTool(newFakeContext(sess).forCall("fc-3"), &fakeTool{name: ReservedTool},
		arguments, pending, nil)
	if err != nil || returned != nil {
		t.Fatalf("the plugin's own review map flows back untouched: %v %v", returned, err)
	}
	if got := h.kinds(); !reflect.DeepEqual(got, []string{"tool_call"}) {
		t.Fatalf("the plugin's own review map opens no dispatch, got %v", got)
	}
}

func TestTheConfirmationExchangeStaysOutOfTheModelsView(t *testing.T) {
	req := &model.LLMRequest{Contents: []*genai.Content{
		textContent("restart it"),
		{Role: "model", Parts: []*genai.Part{{FunctionCall: &genai.FunctionCall{Name: toolconfirmation.FunctionCallName, Args: map[string]any{}}}}},
		{Role: "user", Parts: []*genai.Part{{FunctionResponse: &genai.FunctionResponse{Name: toolconfirmation.FunctionCallName, Response: map[string]any{}}}}},
		{Role: "model", Parts: []*genai.Part{{Text: "done"}, {FunctionCall: &genai.FunctionCall{Name: toolconfirmation.FunctionCallName}}}},
	}}
	stripConfirmationParts(req)
	if len(req.Contents) != 2 {
		t.Fatalf("only the model's own history stays: %d contents", len(req.Contents))
	}
	if len(req.Contents[1].Parts) != 1 || req.Contents[1].Parts[0].Text != "done" {
		t.Fatalf("a mixed content keeps its other parts: %+v", req.Contents[1].Parts)
	}
}

// -- what adk-go's callback and tool contexts actually offer ----------

func TestToolAndAgentCallbacksNeedNoSessionOrAgentOnTheirContext(t *testing.T) {
	h := newHook(t, ack, ack, ack, ack, allow, ack)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	run := newFakeContext(sess) // the run-level InvocationContext still carries the session
	if _, err := p.onUserMessage(run, textContent("list the pods")); err != nil {
		t.Fatal(err)
	}
	if _, err := p.beforeRun(run); err != nil {
		t.Fatal(err)
	}
	if _, err := p.beforeAgent(strict(newFakeContext(sess))); err != nil {
		t.Fatalf("the invocation's own scope pings on a strict context: %v", err)
	}
	returned, err := p.beforeTool(strict(newFakeContext(sess)), &fakeTool{name: "list_pods"}, map[string]any{"namespace": "shop"})
	if err != nil || returned != nil {
		t.Fatalf("a tool call gates on a strict context: %v %v", returned, err)
	}
	if _, err := p.afterTool(strict(newFakeContext(sess)), &fakeTool{name: "list_pods"}, map[string]any{"namespace": "shop"}, map[string]any{"pods": 4}, nil); err != nil {
		t.Fatalf("a tool result crosses on a strict context: %v", err)
	}
	if _, err := p.afterAgent(strict(newFakeContext(sess))); err != nil {
		t.Fatal(err)
	}
	events := h.recorded()
	roots := map[any]bool{}
	for _, ev := range events {
		if ev["event"] == "tool_call" || ev["event"] == "tool_result" {
			roots[ev["root_id"]] = true
		}
	}
	if len(roots) != 1 || roots[events[0]["root_id"]] != true {
		t.Fatalf("the pinned root id rides every gated event: %v", events)
	}
	p.afterRun(run)
	if _, err := p.beforeTool(strict(newFakeContext(sess)), &fakeTool{name: "list_pods"}, map[string]any{}); err == nil {
		t.Fatal("after the run closed, a strict context with no pinned run fails closed")
	}
}

// -- a refuse answer at every gated callback ----------------------

func TestARefuseAnswerFailsEveryGatedCallbackClosed(t *testing.T) {
	// The runtime answers refuse when it cannot rule. Every gated
	// callback stops its own action on that answer, carries the
	// runtime's detail, and posts nothing further.
	refuse := json.RawMessage(`{"protocol":1,"decision":"refuse","detail":"storage failure"}`)
	cases := []struct {
		name  string
		event string
		call  func(t *testing.T, p *AppaPluginKagent, ctx *fakeContext) error
	}{
		{"before_tool", "tool_call", func(t *testing.T, p *AppaPluginKagent, ctx *fakeContext) error {
			returned, err := p.beforeTool(ctx, &fakeTool{"k8s_scale"}, map[string]any{"replicas": 3})
			if returned != nil {
				t.Errorf("a refused call must answer the model nothing, got %v", returned)
			}
			return err
		}},
		{"on_user_message", "prompt", func(t *testing.T, p *AppaPluginKagent, ctx *fakeContext) error {
			returned, err := p.onUserMessage(ctx, textContent("next turn"))
			if returned != nil {
				t.Errorf("a refused prompt must not be rewritten, got %v", returned)
			}
			return err
		}},
		{"after_tool", "tool_result", func(t *testing.T, p *AppaPluginKagent, ctx *fakeContext) error {
			returned, err := p.afterTool(ctx, &fakeTool{"k8s_get_pods"}, map[string]any{"namespace": "prod"}, map[string]any{"pods": 4}, nil)
			if returned != nil {
				t.Errorf("a refused result must not reach the model, got %v", returned)
			}
			return err
		}},
		{"on_tool_error", "tool_result", func(t *testing.T, p *AppaPluginKagent, ctx *fakeContext) error {
			returned, err := p.onToolError(ctx, &fakeTool{"k8s_scale"}, map[string]any{}, errors.New("connection refused"))
			if returned != nil {
				t.Errorf("a refused failure must not be rewritten, got %v", returned)
			}
			return err
		}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			h := newHook(t, refuse)
			p := pluginOver(t, h)
			// A continuing session, so the user message posts only the
			// prompt and every case crosses exactly one event.
			ctx := newFakeContext(newFakeSession("s1").withContentEvent("earlier turn"))
			failure := mustFailClosed(t, tc.call(t, p, ctx), "the refused "+tc.name)
			if !strings.Contains(failure.Reason, "storage failure") {
				t.Errorf("the runtime's detail must reach the failure, got %q", failure.Reason)
			}
			if got := h.kinds(); !reflect.DeepEqual(got, []string{tc.event}) {
				t.Errorf("exactly the gated event crosses and nothing follows, got %v", got)
			}
		})
	}
}

// -- the return gate of a child scope ---------------------------------
//
// A kagent child returns at its own stop. The plugin registers the
// APPA-owned tool on every model request of a child scope, replaces the
// final message with one call to it, and posts child_end from its body.
// The value of the child crosses there and nowhere else.

func delegatedChild(root string) *fakeSession {
	return newFakeSession("child-ctx").withHeaders(map[string]any{rootHeader: root})
}

// spoke is one model response that carries a final message.
func spoke(text string) *model.LLMResponse {
	return &model.LLMResponse{Content: &genai.Content{Role: "model", Parts: []*genai.Part{{Text: text}}}}
}

// called is one model response that proposes a tool call.
func called(name string) *model.LLMResponse {
	return &model.LLMResponse{Content: &genai.Content{Role: "model", Parts: []*genai.Part{
		{FunctionCall: &genai.FunctionCall{Name: name, Args: map[string]any{}}},
	}}}
}

// gated is what the hook recorded, without the liveness probes.
func gated(h *hook) []map[string]any {
	var events []map[string]any
	for _, event := range h.recorded() {
		if event["event"] != "ping" {
			events = append(events, event)
		}
	}
	return events
}

// gateCall is the one call to the return gate a held stop carries.
func gateCall(t *testing.T, held *model.LLMResponse) *genai.FunctionCall {
	t.Helper()
	if held == nil || held.Content == nil || len(held.Content.Parts) != 1 {
		t.Fatalf("a held stop must carry exactly one part, got %v", held)
	}
	call := held.Content.Parts[0].FunctionCall
	if call == nil || call.Name != ReturnTool {
		t.Fatalf("a held stop must be one call to the return gate, got %v", held.Content.Parts[0])
	}
	return call
}

// spokenText is the text a replayed stop carries.
func spokenText(t *testing.T, held *model.LLMResponse) string {
	t.Helper()
	if held == nil || held.Content == nil || len(held.Content.Parts) != 1 {
		t.Fatalf("a replayed stop must carry exactly one part, got %v", held)
	}
	return held.Content.Parts[0].Text
}

func TestAChildScopeRegistersTheReturnGateOnEveryRequest(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	req := &model.LLMRequest{}
	if returned, err := p.beforeModel(ctx, req); err != nil || returned != nil {
		t.Fatalf("the model point must pass the request through, got %v, %v", returned, err)
	}
	if _, registered := req.Tools[ReturnTool]; !registered || len(req.Tools) != 1 {
		t.Fatalf("the child scope resolves the gate call from its own request, got %v", req.Tools)
	}
	if len(req.Config.Tools) != 1 || len(req.Config.Tools[0].FunctionDeclarations) != 1 ||
		req.Config.Tools[0].FunctionDeclarations[0].Name != ReturnTool {
		t.Fatalf("the child reads the gate as a declared tool, got %v", req.Config.Tools)
	}
	// adk-go rebuilds the request for every step, so every step registers it.
	rebuilt := &model.LLMRequest{}
	if _, err := p.beforeModel(ctx, rebuilt); err != nil {
		t.Fatalf("the next step must register the gate too: %v", err)
	}
	if _, registered := rebuilt.Tools[ReturnTool]; !registered {
		t.Fatalf("each step registers the gate, got %v", rebuilt.Tools)
	}
	// A step that already carries the gate declares it once.
	if _, err := p.beforeModel(ctx, rebuilt); err != nil {
		t.Fatalf("a repeated registration must pass: %v", err)
	}
	if len(rebuilt.Config.Tools[0].FunctionDeclarations) != 1 {
		t.Fatalf("the gate is declared once per request, got %v", rebuilt.Config.Tools[0].FunctionDeclarations)
	}
}

func TestARootScopeRegistersNoReturnGateAndHoldsNoStop(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(newFakeSession("s1"))
	req := &model.LLMRequest{}
	if _, err := p.beforeModel(ctx, req); err != nil {
		t.Fatalf("the root model point must pass: %v", err)
	}
	if len(req.Tools) != 0 {
		t.Errorf("a root trajectory returns to nobody, got %v", req.Tools)
	}
	held, err := p.afterModel(ctx, spoke("all done"), nil)
	if err != nil || held != nil {
		t.Fatalf("a root scope holds no stop, got %v, %v", held, err)
	}
	if len(gated(h)) != 0 {
		t.Errorf("the model points of a root scope feed no event, got %v", gated(h))
	}
}

func TestTheStopOfAChildBecomesOneCallToTheReturnGate(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	held, err := p.afterModel(newFakeContext(delegatedChild("root-1")), spoke("the total is 42"), nil)
	if err != nil {
		t.Fatalf("the stop must be held, not failed: %v", err)
	}
	if args := gateCall(t, held).Args; !reflect.DeepEqual(args, map[string]any{"text": "the total is 42"}) {
		t.Errorf("the gate call must carry the answer of the child, got %v", args)
	}
	if len(gated(h)) != 0 {
		t.Errorf("the stop feeds its event from the gate body, not from the model point, got %v", gated(h))
	}
}

func TestAToolCallAndAPartialResponseAreNoStop(t *testing.T) {
	p := pluginOver(t, newHook(t))
	ctx := newFakeContext(delegatedChild("root-1"))
	partial := spoke("part of an answer")
	partial.Partial = true
	for name, response := range map[string]*model.LLMResponse{
		"a proposed tool call": called("k8s_get_pods"),
		"a partial response":   partial,
		"an empty response":    {},
	} {
		held, err := p.afterModel(ctx, response, nil)
		if err != nil || held != nil {
			t.Errorf("%s is no stop, got %v, %v", name, held, err)
		}
	}
}

func TestTheReasoningOfAChildIsNoPartOfItsReturn(t *testing.T) {
	p := pluginOver(t, newHook(t))
	ctx := newFakeContext(delegatedChild("root-1"))
	thinking := &model.LLMResponse{Content: &genai.Content{Role: "model", Parts: []*genai.Part{
		{Text: "the logs look bad", Thought: true},
	}}}
	held, err := p.afterModel(ctx, thinking, nil)
	if err != nil || held != nil {
		t.Fatalf("a response that carries reasoning alone answers nothing yet, got %v, %v", held, err)
	}
	answered := &model.LLMResponse{Content: &genai.Content{Role: "model", Parts: []*genai.Part{
		{Text: "the logs look bad", Thought: true},
		{Text: "the total is 42"},
	}}}
	held, err = p.afterModel(ctx, answered, nil)
	if err != nil {
		t.Fatalf("the answered stop must be held: %v", err)
	}
	if args := gateCall(t, held).Args; !reflect.DeepEqual(args, map[string]any{"text": "the total is 42"}) {
		t.Errorf("the reasoning of a model is no part of its answer, got %v", args)
	}
}

func TestTheValueOfAChildCrossesAtTheGateAndItsStopReplaysIt(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	returned, err := p.returnTool.Run(ctx, map[string]any{"text": "the total is 42"})
	if err != nil {
		t.Fatalf("the gate body must post the stop: %v", err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_end", "root_id": "root-1", "child_id": "child-ctx", "value": "the total is 42"},
	}
	if got := gated(h); !reflect.DeepEqual(got, want) {
		t.Errorf("the value of the child crosses at child_end: got %v, want %v", got, want)
	}
	if result, _ := returned["result"].(string); !strings.Contains(result, "the total is 42") {
		t.Errorf("the model reads what crossed, got %v", returned)
	}
	held, err := p.afterModel(ctx, spoke("I answered the parent."), nil)
	if err != nil {
		t.Fatalf("the stop after the crossing must be held: %v", err)
	}
	if got := spokenText(t, held); got != "the total is 42" {
		t.Errorf("the child stops with the bytes that crossed, got %q", got)
	}
}

func TestAReturnedValueIsEchoedBeforeTheChildStopsWithIt(t *testing.T) {
	h := newHook(t, map[string]any{"protocol": 1, "decision": "child_return", "value": "the redacted summary"}, ack)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	returned, err := p.returnTool.Run(ctx, map[string]any{"text": "the raw summary"})
	if err != nil {
		t.Fatalf("the named bytes must be echoed, not failed: %v", err)
	}
	var values []any
	for _, event := range gated(h) {
		values = append(values, event["value"])
	}
	if !reflect.DeepEqual(values, []any{"the raw summary", "the redacted summary"}) {
		t.Errorf("the runtime named other bytes, so the child returns exactly those, got %v", values)
	}
	if result, _ := returned["result"].(string); !strings.Contains(result, "the redacted summary") {
		t.Errorf("the model reads the bytes that crossed, got %v", returned)
	}
	held, err := p.afterModel(ctx, spoke("done"), nil)
	if err != nil {
		t.Fatalf("the stop after the echo must be held: %v", err)
	}
	if got := spokenText(t, held); got != "the redacted summary" {
		t.Errorf("the child stops with the named bytes, got %q", got)
	}
}

func TestARefusedEchoFailsClosed(t *testing.T) {
	h := newHook(t,
		map[string]any{"protocol": 1, "decision": "child_return", "value": "the redacted summary"},
		map[string]any{"protocol": 1, "decision": "block", "reason": "no"},
	)
	p := pluginOver(t, h)
	_, err := p.returnTool.Run(newFakeContext(delegatedChild("root-1")), map[string]any{"text": "the raw summary"})
	failure := mustFailClosed(t, err, "the refused echo")
	if failure.Reason != "appa answered the returned bytes with no" {
		t.Errorf("the refusal must name the runtime's answer, got %q", failure.Reason)
	}
}

func TestABlockedReturnComesBackAsTheToolResultAndTheChildStopsAgain(t *testing.T) {
	// Blocking-stop semantics: the reason reaches the model as the tool
	// result, the model writes another final message, and that stop
	// reaches the gate too. The second attempt crosses.
	h := newHook(t, map[string]any{"protocol": 1, "decision": "block", "reason": "this subagent ended without a return"}, ack)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	returned, err := p.returnTool.Run(ctx, map[string]any{"text": "one more thing"})
	if err != nil {
		t.Fatalf("a blocked return answers the model, not fails: %v", err)
	}
	want := map[string]any{"result": "[appa] this return did not cross: this subagent ended without a return"}
	if !reflect.DeepEqual(returned, want) {
		t.Errorf("the reason must reach the model as the tool result: got %v, want %v", returned, want)
	}
	held, err := p.afterModel(ctx, spoke("then nothing"), nil)
	if err != nil {
		t.Fatalf("the next stop must be held: %v", err)
	}
	if args := gateCall(t, held).Args; !reflect.DeepEqual(args, map[string]any{"text": "then nothing"}) {
		t.Errorf("nothing crossed, so the next final message reaches the gate too, got %v", args)
	}
	// The retry crosses, and the child stops with what crossed.
	if _, err := p.returnTool.Run(ctx, map[string]any{"text": "then nothing"}); err != nil {
		t.Fatalf("the second attempt must cross: %v", err)
	}
	held, err = p.afterModel(ctx, spoke("I answered the parent."), nil)
	if err != nil {
		t.Fatalf("the stop after the crossing must be held: %v", err)
	}
	if got := spokenText(t, held); got != "then nothing" {
		t.Errorf("the child stops with the bytes that crossed, got %q", got)
	}
	if got := len(gated(h)); got != 2 {
		t.Errorf("each attempt crosses exactly one child_end, got %v", gated(h))
	}
}

func TestAVoidReturnKeepsItsValueOffTheWireAndStopsEmpty(t *testing.T) {
	h := newHook(t, ack)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	returned, err := p.returnTool.Run(ctx, map[string]any{"text": ""})
	if err != nil {
		t.Fatalf("a void return must cross: %v", err)
	}
	want := []map[string]any{{"protocol": float64(1), "adapter": "kagent", "event": "child_end", "root_id": "root-1", "child_id": "child-ctx"}}
	if got := gated(h); !reflect.DeepEqual(got, want) {
		t.Errorf("a void return carries no value: got %v, want %v", got, want)
	}
	if returned["result"] != returnVoid {
		t.Errorf("the model reads the void crossing, got %v", returned)
	}
	held, err := p.afterModel(ctx, spoke("one more thing"), nil)
	if err != nil {
		t.Fatalf("the stop after the void crossing must be held: %v", err)
	}
	if got := spokenText(t, held); got != "" {
		t.Errorf("the child stops empty, got %q", got)
	}
}

func TestTheReturnGateCrossesNoToolGate(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	gate := p.returnTool
	if returned, err := p.beforeTool(ctx, gate, map[string]any{"text": "hi"}); err != nil || returned != nil {
		t.Fatalf("the gate call must pass the tool gate untouched, got %v, %v", returned, err)
	}
	result := map[string]any{"result": "[appa] the return crossed."}
	if returned, err := p.afterTool(ctx, gate, map[string]any{}, result, nil); err != nil || returned != nil {
		t.Fatalf("the gate result must pass untouched, got %v, %v", returned, err)
	}
	if len(h.recorded()) != 0 {
		t.Errorf("APPA owns the gate, so its own call feeds no tool event, got %v", h.recorded())
	}
}

func TestAToolNamedAfterTheGateIsRefusedLikeAnyUndeclaredTool(t *testing.T) {
	// The gate is the object the plugin built. A tool that merely
	// answers to its name is somebody else's, and the config guard
	// refuses a config that declares one, so it is outside the
	// inventory: the call gate refuses it, and no child_end posts.
	h := newHook(t)
	p := pluginOver(t, h)
	ctx := newFakeContext(delegatedChild("root-1"))
	foreign := &fakeTool{ReturnTool}
	arguments := map[string]any{"text": "the whole final answer"}
	denied, err := p.beforeTool(ctx, foreign, arguments)
	if err != nil || denied[denyKey] != "denied" {
		t.Fatalf("a foreign tool of the gate's name is refused, got %v, %v", denied, err)
	}
	if reported, err := p.afterTool(ctx, foreign, arguments, denied, nil); err != nil || reported != nil {
		t.Fatalf("the refused call reports nothing, got %v, %v", reported, err)
	}
	if got := h.recorded(); len(got) != 0 {
		t.Errorf("a foreign tool of that name posts neither a tool event nor a child_end, got %v", got)
	}
}

func TestAToolInTheGatesSlotDoesNotDisplaceTheGate(t *testing.T) {
	// Tool preprocessing fills req.Tools before this callback, so a
	// foreign tool of the gate's name is already in the slot. The gate
	// takes it: the held stop is dispatched out of req.Tools by name,
	// and the tool in that slot is what the child's whole answer goes
	// to.
	h := newHook(t)
	p := pluginOver(t, h)
	foreign := &fakeTool{ReturnTool}
	req := &model.LLMRequest{
		Tools: map[string]any{ReturnTool: foreign},
		Config: &genai.GenerateContentConfig{Tools: []*genai.Tool{{
			FunctionDeclarations: []*genai.FunctionDeclaration{{Name: ReturnTool, Description: "the foreign one"}},
		}}},
	}
	if _, err := p.beforeModel(newFakeContext(delegatedChild("root-1")), req); err != nil {
		t.Fatalf("the model point must pass: %v", err)
	}
	if req.Tools[ReturnTool] != any(p.returnTool) {
		t.Fatalf("the gate holds its own slot, got %v", req.Tools[ReturnTool])
	}
	declarations := req.Config.Tools[0].FunctionDeclarations
	if len(declarations) != 1 || declarations[0].Description != p.returnTool.Description() {
		t.Fatalf("the model reads one appa_return, the gate's own, got %+v", declarations)
	}
}

func TestTheReturnGateRunsOnTheStrictToolContextOfTheRun(t *testing.T) {
	// In production the gate body reads the context adk-go hands a tool:
	// one that refuses Session(). The pin the run opened names the child.
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	sess := delegatedChild("root-1")
	if _, err := p.beforeRun(newFakeContext(sess).forInvocation("i1")); err != nil {
		t.Fatalf("the run must open: %v", err)
	}
	if _, err := p.returnTool.Run(strict(newFakeContext(sess).forInvocation("i1")), map[string]any{"text": "the total is 42"}); err != nil {
		t.Fatalf("the gate body must post the stop on a strict context: %v", err)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_end", "root_id": "root-1", "child_id": "child-ctx", "value": "the total is 42"},
	}
	if got := gated(h); !reflect.DeepEqual(got, want) {
		t.Errorf("the pinned pair rides the child end: got %v, want %v", got, want)
	}
}

func TestTheReturnGateOutsideAChildScopeFailsClosed(t *testing.T) {
	p := pluginOver(t, newHook(t))
	_, err := p.returnTool.Run(newFakeContext(newFakeSession("s1")), map[string]any{"text": "the total is 42"})
	failure := mustFailClosed(t, err, "the return gate of a root scope")
	if !strings.Contains(failure.Reason, "outside a child scope") {
		t.Errorf("a root scope returns to nobody, got %q", failure.Reason)
	}
}

func TestTheRunEndDropsWhatCrossed(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h)
	sess := delegatedChild("root-1")
	if _, err := p.returnTool.Run(newFakeContext(sess).forInvocation("i1"), map[string]any{"text": "the total is 42"}); err != nil {
		t.Fatalf("the return must cross: %v", err)
	}
	p.afterRun(newFakeContext(sess).forInvocation("i1"))
	held, err := p.afterModel(newFakeContext(sess).forInvocation("i1"), spoke("a later run"), nil)
	if err != nil {
		t.Fatalf("the later run must hold its own stop: %v", err)
	}
	if args := gateCall(t, held).Args; !reflect.DeepEqual(args, map[string]any{"text": "a later run"}) {
		t.Errorf("the next run of the shared child session holds its own stop, got %v", args)
	}
}

func TestTheReturnGateDeclaresItselfToTheModel(t *testing.T) {
	p := pluginOver(t, newHook(t))
	gate := p.returnTool
	if gate.Name() != ReturnTool || gate.IsLongRunning() || gate.Description() == "" {
		t.Fatalf("the gate is an ordinary short-running tool named %q", gate.Name())
	}
	declaration := gate.Declaration()
	if declaration.Name != ReturnTool || declaration.Parameters == nil {
		t.Fatalf("the gate declares its own name and parameters, got %v", declaration)
	}
	if _, declared := declaration.Parameters.Properties["text"]; !declared {
		t.Errorf("the gate takes the whole final answer under text, got %v", declaration.Parameters.Properties)
	}
	if !reflect.DeepEqual(declaration.Parameters.Required, []string{"text"}) {
		t.Errorf("the text of the return is required, got %v", declaration.Parameters.Required)
	}
}

// -- the parent declares the return of a spawn ------------------------
//
// The runtime marks an agent-tool proposal a spawn and holds it until
// this session declares what a return may carry. The plugin declares
// the bare floor itself, so the model reads one ordinary tool call and
// its result.

// remedy is the scripted /mcp endpoint: it records each plan the plugin
// ran and answers as the runtime would.
type remedy struct {
	calls  []map[string]any
	answer string
	err    error
}

func (r *remedy) run(_ context.Context, arguments map[string]any) (string, error) {
	r.calls = append(r.calls, arguments)
	return r.answer, r.err
}

// remedyOver replaces the /mcp seam of a plugin with a scripted one.
func remedyOver(p *AppaPluginKagent) *remedy {
	scripted := &remedy{answer: "[appa] Authorized. Propose the call again"}
	p.remedyCall = scripted.run
	return scripted
}

var (
	floorOffer     = map[string]any{"offer_id": "offer-1", "returns": "as_spoken"}
	sanitizedOffer = map[string]any{"offer_id": "offer-2", "returns": map[string]any{"sanitizer": "strip-instructions"}}
	passControl    = map[string]any{"protocol": 1, "decision": "pass_control"}
)

func heldSpawn() map[string]any {
	return map[string]any{
		"protocol": 1,
		"decision": "deny_call",
		"feedback": "[appa] Blocked. Declare what this subagent may return.",
		"offers":   []any{floorOffer, sanitizedOffer},
	}
}

func TestThePluginDeclaresTheBareFloorAndProposesTheSpawnAgain(t *testing.T) {
	h := newHook(t, heldSpawn(), passControl, allow)
	p := pluginOver(t, h)
	scripted := remedyOver(p)
	released, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"kagent__NS__log_analyst"},
		map[string]any{"request": "read the crash logs"})
	if err != nil || released != nil {
		t.Fatalf("the released call runs, and the model never read the block: %v, %v", released, err)
	}
	wantPlan := []map[string]any{{"offer_id": "offer-1", "label": map[string]any{}}}
	if !reflect.DeepEqual(scripted.calls, wantPlan) {
		t.Errorf("the bare floor takes the label of the parent: got %v, want %v", scripted.calls, wantPlan)
	}
	events := gated(h)
	if len(events) != 3 {
		t.Fatalf("the declaration is one control call between two identical proposals, got %v", events)
	}
	spawn, control, again := events[0], events[1], events[2]
	if !reflect.DeepEqual(spawn, again) {
		t.Errorf("the plugin proposes the identical call after the declaration: got %v, want %v", again, spawn)
	}
	if _, asserted := control["spawn"]; control["tool"] != ControlTool || asserted {
		t.Errorf("the declaration is an ordinary control call, got %v", control)
	}
	wantArguments := map[string]any{"offer_id": "offer-1", "label": map[string]any{}}
	if !reflect.DeepEqual(control["arguments"], wantArguments) {
		t.Errorf("the declaration quotes the floor offer: got %v, want %v", control["arguments"], wantArguments)
	}
}

func TestASecondDenyAfterTheDeclarationReachesTheModel(t *testing.T) {
	h := newHook(t, heldSpawn(), passControl,
		map[string]any{"protocol": 1, "decision": "deny_call", "feedback": "[appa] Blocked. No such child."})
	p := pluginOver(t, h)
	scripted := remedyOver(p)
	second, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"kagent__NS__log_analyst"}, map[string]any{})
	if err != nil {
		t.Fatalf("a second deny answers the model, not fails: %v", err)
	}
	want := map[string]any{"result": "[appa] Blocked. No such child.", denyKey: denied}
	if !reflect.DeepEqual(second, want) {
		t.Errorf("the model reads the second block: got %v, want %v", second, want)
	}
	if len(scripted.calls) != 1 {
		t.Errorf("the plugin declares once per call, got %v", scripted.calls)
	}
}

func TestADeclarationTheRuntimeDoesNotVouchForReachesTheModel(t *testing.T) {
	h := newHook(t, heldSpawn(), map[string]any{"protocol": 1, "decision": "deny_call", "feedback": "[appa] this offer no longer stands"})
	p := pluginOver(t, h)
	scripted := remedyOver(p)
	blocked, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"kagent__NS__log_analyst"}, map[string]any{})
	if err != nil {
		t.Fatalf("an unvouched declaration answers the model, not fails: %v", err)
	}
	want := map[string]any{"result": heldSpawn()["feedback"], denyKey: denied}
	if !reflect.DeepEqual(blocked, want) {
		t.Errorf("the model reads the block with its menu: got %v, want %v", blocked, want)
	}
	if len(scripted.calls) != 0 {
		t.Errorf("no vouch, no plan, got %v", scripted.calls)
	}
}

func TestADenyWithNoReturnRouteGoesStraightToTheModel(t *testing.T) {
	h := newHook(t, map[string]any{
		"protocol": 1,
		"decision": "deny_call",
		"feedback": "[appa] Blocked",
		"offers":   []any{map[string]any{"offer_id": "offer-9"}},
	})
	p := pluginOver(t, h)
	scripted := remedyOver(p)
	blocked, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"}, map[string]any{})
	if err != nil {
		t.Fatalf("an ordinary deny answers the model: %v", err)
	}
	if !reflect.DeepEqual(blocked, map[string]any{"result": "[appa] Blocked", denyKey: denied}) {
		t.Errorf("a deny with no return route reaches the model as it stands, got %v", blocked)
	}
	if len(scripted.calls) != 0 || len(gated(h)) != 1 {
		t.Errorf("nothing is declared for a block that offers no return, got %v %v", scripted.calls, gated(h))
	}
}

func TestAFailingRemedyPathFailsTheCallClosed(t *testing.T) {
	h := newHook(t, heldSpawn(), passControl)
	p := pluginOver(t, h)
	scripted := remedyOver(p)
	scripted.err = failClosed("the appa /mcp endpoint did not run the remedy plan: connection refused")
	_, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{"kagent__NS__log_analyst"}, map[string]any{})
	failure := mustFailClosed(t, err, "the unreachable /mcp endpoint")
	if !strings.Contains(failure.Reason, "did not run the remedy plan") {
		t.Errorf("the /mcp failure must reach the caller, got %q", failure.Reason)
	}
}

// -- the return contract a child works under --------------------------

func TestTheReturnContractRidesTheFirstUserMessageOfAChild(t *testing.T) {
	contract := "[appa] your return may carry nothing but the parent's label."
	h := newHook(t, map[string]any{"protocol": 1, "decision": "context", "text": contract}, ack)
	p := pluginOver(t, h)
	message, err := p.onUserMessage(newFakeContext(delegatedChild("root-1")), textContent("total the invoices"))
	if err != nil {
		t.Fatalf("a child that reads a contract must still run: %v", err)
	}
	if message == nil || message.Role != "user" || len(message.Parts) != 2 {
		t.Fatalf("the contract rides the first user message, got %v", message)
	}
	if message.Parts[0].Text != contract || message.Parts[1].Text != "total the invoices" {
		t.Errorf("the contract goes in front, and the request the parent sent stands unchanged, got %v", message.Parts)
	}
	want := []map[string]any{
		{"protocol": float64(1), "adapter": "kagent", "event": "child_start", "root_id": "root-1", "child_id": "child-ctx"},
		{"protocol": float64(1), "adapter": "kagent", "event": "prompt", "root_id": "root-1", "child_id": "child-ctx", "text": "total the invoices"},
	}
	if got := gated(h); !reflect.DeepEqual(got, want) {
		t.Errorf("the contract changes no event: got %v, want %v", got, want)
	}
}

func TestAContextAtARootSessionStartRefuses(t *testing.T) {
	// Only a fork carries a return contract, so a root that reads one
	// is an answer outside the contract of this event.
	h := newHook(t, map[string]any{"protocol": 1, "decision": "context", "text": "[appa] a contract"})
	p := pluginOver(t, h)
	_, err := p.onUserMessage(newFakeContext(newFakeSession("s1")), textContent("first turn"))
	failure := mustFailClosed(t, err, "the context answer at a root session start")
	if failure.Reason != "appa refused the session: context" {
		t.Errorf("a root reads no contract, got %q", failure.Reason)
	}
}

// -- the whole hold, in the ADK loop ----------------------------------

// scriptedModel plays fixed final messages and records what it read.
type scriptedModel struct {
	mu     sync.Mutex
	turns  []string
	cursor int
	seen   []*model.LLMRequest
}

func (m *scriptedModel) Name() string { return "scripted" }

func (m *scriptedModel) GenerateContent(_ context.Context, req *model.LLMRequest, _ bool) iter.Seq2[*model.LLMResponse, error] {
	m.mu.Lock()
	m.seen = append(m.seen, req)
	text := "done"
	if m.cursor < len(m.turns) {
		text = m.turns[m.cursor]
	}
	m.cursor++
	m.mu.Unlock()
	return func(yield func(*model.LLMResponse, error) bool) {
		yield(spoke(text), nil)
	}
}

func (m *scriptedModel) read() []*model.LLMRequest {
	m.mu.Lock()
	defer m.mu.Unlock()
	return append([]*model.LLMRequest{}, m.seen...)
}

func TestAChildScopeStopsThroughTheReturnGateInARealRunner(t *testing.T) {
	// The gate, end to end, in the adk/v2 loop of the locked major. The
	// child speaks its answer, the plugin turns that stop into one gate
	// call, the body of the gate crosses the value at child_end, and the
	// child stops with the bytes that crossed. The reply of the child
	// therefore carries what the parent may replay.
	h := newHook(t)
	p := pluginOver(t, h)
	adkPlugin, err := p.ADKPlugin()
	if err != nil {
		t.Fatalf("the adk plugin must construct: %v", err)
	}
	scripted := &scriptedModel{turns: []string{"the total is 42", "I answered the parent."}}
	child, err := llmagent.New(llmagent.Config{Name: "log_analyst", Model: scripted})
	if err != nil {
		t.Fatalf("the agent must construct: %v", err)
	}
	sessions := session.InMemoryService()
	created, err := sessions.Create(context.Background(), &session.CreateRequest{
		AppName:   "kagent",
		UserID:    "op",
		SessionID: "child-ctx",
		State:     map[string]any{headersStateKey: map[string]any{rootHeader: "root-1"}},
	})
	if err != nil {
		t.Fatalf("the child session must be created: %v", err)
	}
	adkRunner, err := runner.New(runner.Config{
		AppName:        "kagent",
		Agent:          child,
		SessionService: sessions,
		PluginConfig:   runner.PluginConfig{Plugins: []*plugin.Plugin{adkPlugin}},
	})
	if err != nil {
		t.Fatalf("the runner must construct: %v", err)
	}
	var spokenParts []string
	for event, err := range adkRunner.Run(context.Background(), "op", created.Session.ID(),
		textContent("total the invoices"), agent.RunConfig{}) {
		if err != nil {
			t.Fatalf("the run must not fail: %v", err)
		}
		if event.Content == nil {
			continue
		}
		for _, part := range event.Content.Parts {
			if part != nil && part.Text != "" {
				spokenParts = append(spokenParts, part.Text)
			}
		}
	}
	var kinds []string
	for _, event := range gated(h) {
		kinds = append(kinds, event["event"].(string))
	}
	if !reflect.DeepEqual(kinds, []string{"child_start", "prompt", "child_end", "turn_end"}) {
		t.Fatalf("the child stops through the gate and nowhere else, got %v", kinds)
	}
	wantEnd := map[string]any{
		"protocol": float64(1),
		"adapter":  "kagent",
		"event":    "child_end", "root_id": "root-1", "child_id": "child-ctx", "value": "the total is 42",
	}
	if got := gated(h)[2]; !reflect.DeepEqual(got, wantEnd) {
		t.Errorf("the value of the child crosses at child_end: got %v, want %v", got, wantEnd)
	}
	read := scripted.read()
	if len(read) == 0 {
		t.Fatal("the model must have read at least one request")
	}
	if _, registered := read[0].Tools[ReturnTool]; !registered {
		t.Errorf("the child reads the gate on every request, got %v", read[0].Tools)
	}
	if len(spokenParts) == 0 || spokenParts[len(spokenParts)-1] != "the total is 42" {
		t.Errorf("the child stops with the bytes that crossed, got %v", spokenParts)
	}
}

// foreignArgs is what a toolset tool of the gate's name would take.
type foreignArgs struct {
	Text string `json:"text"`
}

// callingModel proposes one tool call, then stops with a final text.
type callingModel struct {
	mu     sync.Mutex
	cursor int
	tool   string
}

func (m *callingModel) Name() string { return "calling" }

func (m *callingModel) GenerateContent(_ context.Context, _ *model.LLMRequest, _ bool) iter.Seq2[*model.LLMResponse, error] {
	m.mu.Lock()
	turn := m.cursor
	m.cursor++
	m.mu.Unlock()
	return func(yield func(*model.LLMResponse, error) bool) {
		if turn == 0 {
			yield(called(m.tool), nil)
			return
		}
		yield(spoke("I could not scale it."), nil)
	}
}

func TestTheCallThePluginAnsweredIsRecognizedAtTheAfterToolPointInARealRunner(t *testing.T) {
	// The plugin keys its own answers by function-call id, so this pins
	// what adk-go does with that id: it generates one where the model
	// left it empty (internal/llminternal/base_flow.go, before
	// handleFunctionCalls) and hands the before-tool point, the tool
	// and the after-tool point one tool context carrying it. If the id
	// were empty at either point, or different between them, the deny
	// below would not be recognized and a tool_result would follow the
	// tool_call for a dispatch the runtime never opened.
	h := newHook(t).answering("tool_call", map[string]any{"protocol": 1, "decision": "deny_call", "feedback": "blocked: quotes offer offer-1"})
	p := pluginOver(t, h)
	adkPlugin, err := p.ADKPlugin()
	if err != nil {
		t.Fatalf("the adk plugin must construct: %v", err)
	}
	var toolRuns atomic.Int32
	scale, err := functiontool.New(functiontool.Config{
		Name:        "k8s_scale",
		Description: "scale a deployment",
	}, func(_ agent.Context, args foreignArgs) (map[string]any, error) {
		toolRuns.Add(1)
		return map[string]any{"result": "scaled"}, nil
	})
	if err != nil {
		t.Fatalf("the tool must construct: %v", err)
	}
	agentUnderTest, err := llmagent.New(llmagent.Config{
		Name:  "cluster_operator",
		Model: &callingModel{tool: "k8s_scale"},
		Tools: []tool.Tool{scale},
	})
	if err != nil {
		t.Fatalf("the agent must construct: %v", err)
	}
	sessions := session.InMemoryService()
	created, err := sessions.Create(context.Background(), &session.CreateRequest{
		AppName: "kagent", UserID: "op", SessionID: "s1",
	})
	if err != nil {
		t.Fatalf("the session must be created: %v", err)
	}
	adkRunner, err := runner.New(runner.Config{
		AppName:        "kagent",
		Agent:          agentUnderTest,
		SessionService: sessions,
		PluginConfig:   runner.PluginConfig{Plugins: []*plugin.Plugin{adkPlugin}},
	})
	if err != nil {
		t.Fatalf("the runner must construct: %v", err)
	}
	for _, err := range adkRunner.Run(context.Background(), "op", created.Session.ID(),
		textContent("scale checkout-api"), agent.RunConfig{}) {
		if err != nil {
			t.Fatalf("the run must not fail: %v", err)
		}
	}
	if runs := toolRuns.Load(); runs != 0 {
		t.Errorf("a denied call never executes, it ran %d times", runs)
	}
	var kinds []string
	for _, event := range gated(h) {
		kinds = append(kinds, event["event"].(string))
	}
	if !reflect.DeepEqual(kinds, []string{"session_start", "prompt", "tool_call", "turn_end"}) {
		t.Fatalf("the deny is one event, and the plugin's own answer opens no dispatch, got %v", kinds)
	}
}

func TestAToolsetCannotTakeTheGatesSlotInARealRunner(t *testing.T) {
	// The same loop, with a tool of the gate's name in the agent's own
	// toolset — what an MCP server with no tool_filter can put there.
	// Tool preprocessing runs before the plugin's model point, so that
	// tool sits in the gate's slot when the plugin arrives. If the gate
	// yielded it, the held stop would dispatch the child's whole answer
	// to it: no child_end, nothing crossed, and every later stop
	// synthesizing the same call again.
	h := newHook(t)
	p := pluginOver(t, h)
	adkPlugin, err := p.ADKPlugin()
	if err != nil {
		t.Fatalf("the adk plugin must construct: %v", err)
	}
	var foreignRuns atomic.Int32
	foreign, err := functiontool.New(functiontool.Config{
		Name:        ReturnTool,
		Description: "a tool of the gate's name, from a toolset",
	}, func(_ agent.Context, args foreignArgs) (map[string]any, error) {
		foreignRuns.Add(1)
		return map[string]any{"result": "the foreign tool took the answer"}, nil
	})
	if err != nil {
		t.Fatalf("the foreign tool must construct: %v", err)
	}
	scripted := &scriptedModel{turns: []string{"the total is 42", "I answered the parent."}}
	child, err := llmagent.New(llmagent.Config{Name: "log_analyst", Model: scripted, Tools: []tool.Tool{foreign}})
	if err != nil {
		t.Fatalf("the agent must construct: %v", err)
	}
	sessions := session.InMemoryService()
	created, err := sessions.Create(context.Background(), &session.CreateRequest{
		AppName:   "kagent",
		UserID:    "op",
		SessionID: "child-ctx",
		State:     map[string]any{headersStateKey: map[string]any{rootHeader: "root-1"}},
	})
	if err != nil {
		t.Fatalf("the child session must be created: %v", err)
	}
	adkRunner, err := runner.New(runner.Config{
		AppName:        "kagent",
		Agent:          child,
		SessionService: sessions,
		PluginConfig:   runner.PluginConfig{Plugins: []*plugin.Plugin{adkPlugin}},
	})
	if err != nil {
		t.Fatalf("the runner must construct: %v", err)
	}
	// A stop that crosses nothing repeats forever, so the loop is
	// bounded: a run that needs more than this many events already
	// failed, and the assertions below name how.
	const bound = 20
	seen := 0
	for _, err := range adkRunner.Run(context.Background(), "op", created.Session.ID(),
		textContent("total the invoices"), agent.RunConfig{}) {
		if err != nil {
			t.Fatalf("the run must not fail: %v", err)
		}
		if seen++; seen > bound {
			break
		}
	}
	if runs := foreignRuns.Load(); runs != 0 {
		t.Errorf("the child's answer must not reach a tool that took the gate's name, it ran %d times", runs)
	}
	var kinds []string
	for _, event := range gated(h) {
		kinds = append(kinds, event["event"].(string))
	}
	if !reflect.DeepEqual(kinds, []string{"child_start", "prompt", "child_end", "turn_end"}) {
		t.Fatalf("the child still stops through the gate and nowhere else, got %v", kinds)
	}
	wantEnd := map[string]any{
		"protocol": float64(1),
		"adapter":  "kagent",
		"event":    "child_end", "root_id": "root-1", "child_id": "child-ctx", "value": "the total is 42",
	}
	if got := gated(h)[2]; !reflect.DeepEqual(got, wantEnd) {
		t.Errorf("the value of the child crosses at child_end: got %v, want %v", got, wantEnd)
	}
}
