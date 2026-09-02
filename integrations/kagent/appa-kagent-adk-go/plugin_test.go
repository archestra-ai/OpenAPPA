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
	"sync"
	"testing"
	"time"

	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool/toolconfirmation"
)

var (
	ack   = map[string]any{"decision": "ack"}
	allow = map[string]any{"decision": "allow_call"}
)

// hook is the scripted runtime: answers in order, records every event.
// An int answer plays back as that HTTP status; a map answer plays
// back as a 200 decision envelope; an exhausted script answers ack.
type hook struct {
	mu      sync.Mutex
	answers []any
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
		if len(h.answers) > 0 {
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

func pluginOver(t *testing.T, h *hook, spawnTools ...string) *AppaPluginKagent {
	t.Helper()
	p, err := New(Config{RuntimeURL: h.server.URL, SpawnTools: spawnTools})
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
	p, err := New(Config{RuntimeURL: url})
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
	}
}

func (c *fakeContext) forAgent(name string) *fakeContext {
	c.agentName = name
	return c
}

func (c *fakeContext) Session() session.Session { return c.session }
func (c *fakeContext) InvocationID() string     { return c.invocationID }
func (c *fakeContext) AgentName() string        { return c.agentName }

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
		{"event": "session_start", "root_id": "s1"},
		{"event": "prompt", "root_id": "s1", "text": "deploy the chart"},
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
	h := newHook(t, ack, map[string]any{"decision": "block", "reason": "the prompt does not cross"})
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
		{"event": "child_start", "root_id": "root-ctx", "child_id": "child-ctx"},
		{"event": "prompt", "root_id": "root-ctx", "child_id": "child-ctx", "text": "total the invoices"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the delegated opening drifted: got %v, want %v", got, want)
	}
}

func TestASessionsIdentityIsPinnedAtFirstClassification(t *testing.T) {
	h := newHook(t, allow, allow)
	p := pluginOver(t, h)
	sess := newFakeSession("s1")
	ctx := newFakeContext(sess)
	if _, err := p.beforeTool(ctx, &fakeTool{"k8s_get_pods"}, map[string]any{}); err != nil {
		t.Fatalf("the first call must pass: %v", err)
	}
	sess.state.values[headersStateKey] = map[string]any{rootHeader: "root-ctx"}
	if _, err := p.beforeTool(ctx, &fakeTool{"k8s_get_pods"}, map[string]any{}); err != nil {
		t.Fatalf("the second call must pass: %v", err)
	}
	for _, event := range h.recorded() {
		if event["root_id"] != "s1" {
			t.Errorf("late headers must not flip the session between trajectories, got root %v", event["root_id"])
		}
	}
}

// -- the tool gate ------------------------------------------------

func TestAnAllowedCallPassesAndADeniedCallAnswersTheModel(t *testing.T) {
	h := newHook(t, allow, map[string]any{"decision": "deny_call", "feedback": "blocked: quotes offer offer-1"})
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
		"event":     "tool_call",
		"root_id":   "s1",
		"tool":      "k8s_scale",
		"arguments": map[string]any{"replicas": float64(3)},
		"spawn":     false,
	}
	if got := h.recorded()[0]; !reflect.DeepEqual(got, wantEvent) {
		t.Errorf("the tool_call event drifted: got %v, want %v", got, wantEvent)
	}
}

func TestTheConfiguredSpawnToolsClassifyAsTheSpawn(t *testing.T) {
	h := newHook(t, allow, allow)
	p := pluginOver(t, h, "billing-agent")
	ctx := newFakeContext(newFakeSession("s1"))
	for _, name := range []string{"billing-agent", "k8s_scale"} {
		if _, err := p.beforeTool(ctx, &fakeTool{name}, map[string]any{}); err != nil {
			t.Fatalf("the %s call must pass: %v", name, err)
		}
	}
	var spawns []bool
	for _, event := range h.recorded() {
		spawns = append(spawns, event["spawn"].(bool))
	}
	if !reflect.DeepEqual(spawns, []bool{true, false}) {
		t.Errorf("spawn classification drifted: got %v", spawns)
	}
}

func TestTheReservedToolPassesControl(t *testing.T) {
	h := newHook(t, map[string]any{"decision": "pass_control"})
	p := pluginOver(t, h)
	returned, err := p.beforeTool(newFakeContext(newFakeSession("s1")), &fakeTool{ReservedTool}, map[string]any{"offer_id": "offer-1"})
	if err != nil || returned != nil {
		t.Fatalf("pass_control must let the call through to /mcp untouched, got %v, %v", returned, err)
	}
}

func TestADenyMapIsNotReportedTwice(t *testing.T) {
	h := newHook(t)
	p := pluginOver(t, h)
	returned, err := p.afterTool(
		newFakeContext(newFakeSession("s1")), &fakeTool{"k8s_scale"},
		map[string]any{}, map[string]any{"result": "blocked", denyKey: denied}, nil)
	if err != nil || returned != nil {
		t.Fatalf("the deny map must flow back untouched, got %v, %v", returned, err)
	}
	if len(h.recorded()) != 0 {
		t.Errorf("the denied call was reported at the call and no dispatch is open, got %v", h.recorded())
	}
}

func TestAToolResultCrossesAndEnforcesEachAnswer(t *testing.T) {
	h := newHook(t,
		ack,
		map[string]any{"decision": "replace_output", "output": "the output is confined"},
		map[string]any{"decision": "block", "reason": "nothing crosses"},
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
		"event":     "tool_result",
		"root_id":   "s1",
		"tool":      "k8s_get_pods",
		"arguments": map[string]any{"namespace": "prod"},
		"outcome":   map[string]any{"status": "success", "body": map[string]any{"pods": []any{"api-1"}}},
	}
	if got := h.recorded()[0]; !reflect.DeepEqual(got, wantEvent) {
		t.Errorf("the tool_result event drifted: got %v, want %v", got, wantEvent)
	}
}

func TestASpawnReturnCrossesAsTheSpawnResultInBothReplyShapes(t *testing.T) {
	h := newHook(t, ack, ack)
	p := pluginOver(t, h, "billing-agent")
	ctx := newFakeContext(newFakeSession("s1"))
	if _, err := p.afterTool(ctx, &fakeTool{"billing-agent"},
		map[string]any{"request": "total the invoices"},
		map[string]any{"result": "the total is 42", "subagent_session_id": "child-ctx"}, nil); err != nil {
		t.Fatalf("the task reply must cross: %v", err)
	}
	if _, err := p.afterTool(ctx, &fakeTool{"billing-agent"},
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
	h := newHook(t, map[string]any{"decision": "child_return", "value": "the redacted summary"})
	p := pluginOver(t, h, "billing-agent")
	returned, err := p.afterTool(
		newFakeContext(newFakeSession("s1")), &fakeTool{"billing-agent"},
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
		{"event": "ping"},
		{"event": "child_start", "root_id": "s1", "child_id": "i1:billing-agent"},
		{"event": "turn_end", "root_id": "s1", "child_id": "i1:billing-agent"},
	}
	if got := h.recorded(); !reflect.DeepEqual(got, want) {
		t.Errorf("the agent-scope events drifted: got %v, want %v", got, want)
	}
}

func TestARefusedChildScopeFailsClosed(t *testing.T) {
	h := newHook(t, ack, map[string]any{"decision": "refuse", "detail": "storage failure"})
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
		if !reflect.DeepEqual(event, map[string]any{"event": "ping"}) {
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
		map[string]any{"decision": "approve"},
		map[string]any{"decision": "deny_call"},
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
	want := []map[string]any{{"event": "turn_end", "root_id": "s1"}}
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
	want := []map[string]any{{"event": "turn_end", "root_id": "root-ctx", "child_id": "child-ctx"}}
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
		h := newHook(t, denyWithReview(), map[string]any{"decision": "pass_control"}, map[string]any{"decision": "pass_control"})
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
		if last["tool"] != ReservedTool || last["ruling"] != tc.ruling {
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
	h := newHook(t, map[string]any{"decision": "pass_control"})
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
	h := newHook(t)
	p := pluginOver(t, h)
	returned, err := p.afterTool(newFakeContext(newFakeSession("s1")), &fakeTool{name: ReservedTool},
		map[string]any{"offer_id": "offer-1"}, map[string]any{"result": reviewPending, denyKey: reviewValue}, nil)
	if err != nil || returned != nil || len(h.recorded()) != 0 {
		t.Fatalf("the plugin's own review map opens no dispatch: %v %v %v", returned, err, h.recorded())
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
