package main

import (
	"reflect"
	"testing"

	"github.com/kagent-dev/kagent/go/api/adk"

	"context"
	a2atype "github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv"
	"github.com/a2aproject/a2a-go/a2asrv/eventqueue"
	appakagentadk "github.com/archestra-ai/OpenAPPA/integrations/kagent/appa-kagent-adk-go"
	adksession "google.golang.org/adk/v2/session"
)

func TestTheRuntimeRefusesToStartUngated(t *testing.T) {
	t.Setenv("APPA_RUNTIME_URL", "")
	if _, err := appaRuntimeURL(); err == nil {
		t.Fatal("a missing APPA_RUNTIME_URL must refuse startup")
	}
	t.Setenv("APPA_RUNTIME_URL", "http://appa-runtime.appa-system:8787")
	url, err := appaRuntimeURL()
	if err != nil {
		t.Fatalf("a set APPA_RUNTIME_URL must pass: %v", err)
	}
	if url != "http://appa-runtime.appa-system:8787" {
		t.Errorf("the runtime URL drifted: %q", url)
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

func TestSpawnToolNamesFollowTheStockBuilder(t *testing.T) {
	agentConfig := &adk.AgentConfig{
		RemoteAgents: []adk.RemoteAgentConfig{
			{Name: "billing-agent", Url: "http://billing.agents:8080"},
			{Name: "skipped-agent"}, // no URL: the stock builder skips it
		},
	}
	if got := spawnToolNames(agentConfig); !reflect.DeepEqual(got, []string{"billing-agent"}) {
		t.Errorf("spawn names must match what the stock builder wires, got %v", got)
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
