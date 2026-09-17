package appakagentadk

import (
	"context"
	"fmt"
	"iter"
	"testing"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/agent/llmagent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/plugin"
	"google.golang.org/adk/v2/runner"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/functiontool"
	"google.golang.org/adk/v2/tool/toolconfirmation"
	"google.golang.org/adk/v2/tool/toolutils"
	"google.golang.org/genai"
)

// Represents request-time MCP publication without requiring a live server. The
// real ADK runner must resume a confirmation before calling this processor again.
type resumeTestPublisher struct{ selected *discoveredMCPTool }

func (*resumeTestPublisher) Name() string        { return "test_discovery" }
func (*resumeTestPublisher) Description() string { return "" }
func (*resumeTestPublisher) IsLongRunning() bool { return false }
func (p *resumeTestPublisher) ProcessRequest(_ agent.Context, req *model.LLMRequest) error {
	return toolutils.PackTool(req, p.selected)
}

type resumeInspectModel struct {
	callingModel
	t *testing.T
}

func (m *resumeInspectModel) GenerateContent(ctx context.Context, req *model.LLMRequest, stream bool) iter.Seq2[*model.LLMResponse, error] {
	counts := make(map[string]int)
	for _, group := range req.Config.Tools {
		for _, declaration := range group.FunctionDeclarations {
			counts[declaration.Name]++
		}
	}
	if counts[m.tool] != 1 || len(counts) != 1 {
		m.t.Fatalf("resume registration changed model-visible declarations: %v", counts)
	}
	if _, ok := req.Tools[m.tool].(*discoveredMCPTool); !ok {
		m.t.Fatalf("resume dispatcher replaced dynamic model tool: %T", req.Tools[m.tool])
	}
	return m.callingModel.GenerateContent(ctx, req, stream)
}

func TestMCPConfirmationResumesInRealRunner(t *testing.T) {
	for _, name := range []string{ReservedTool, "restart_deployment"} {
		for _, confirmed := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/approve=%t", name, confirmed), func(t *testing.T) {
				runs := 0
				native, err := functiontool.New(functiontool.Config{Name: name, Description: "test action"},
					func(agent.Context, map[string]any) (map[string]any, error) {
						runs++
						return map[string]any{"executed": true}, nil
					})
				if err != nil {
					t.Fatal(err)
				}
				selected := &discoveredMCPTool{mcpRunnable: native.(mcpRunnable), spelling: "mcp/test/" + name,
					approvals: map[string]bool{name: true}}
				d := &MCPDiscovery{runs: make(map[string]*mcpRun), connections: []mcpConnection{{approvals: map[string]bool{name: true}}}}
				prepare, err := plugin.New(plugin.Config{Name: "test_inventory", BeforeRunCallback: func(ctx agent.InvocationContext) (*genai.Content, error) {
					d.run(ctx.InvocationID()).selected = map[string]*discoveredMCPTool{name: selected}
					d.run(ctx.InvocationID()).opened = true
					return nil, nil
				}})
				if err != nil {
					t.Fatal(err)
				}
				tools := append([]tool.Tool{&resumeTestPublisher{selected}}, d.resumeTools()...)
				a, err := llmagent.New(llmagent.Config{Name: "test_agent", Model: &resumeInspectModel{callingModel: callingModel{tool: name}, t: t}, Tools: tools})
				if err != nil {
					t.Fatal(err)
				}
				sessions := session.InMemoryService()
				_, err = sessions.Create(context.Background(), &session.CreateRequest{AppName: "test", UserID: "user", SessionID: "session"})
				if err != nil {
					t.Fatal(err)
				}
				r, err := runner.New(runner.Config{AppName: "test", Agent: a, SessionService: sessions,
					PluginConfig: runner.PluginConfig{Plugins: []*plugin.Plugin{prepare}}})
				if err != nil {
					t.Fatal(err)
				}
				confirmationID := ""
				for event, err := range r.Run(context.Background(), "user", "session", textContent("perform action"), agent.RunConfig{}) {
					if err != nil {
						t.Fatal(err)
					}
					if event.Content != nil {
						for _, part := range event.Content.Parts {
							if call := part.FunctionCall; call != nil && call.Name == toolconfirmation.FunctionCallName {
								confirmationID = call.ID
							}
						}
					}
				}
				if confirmationID == "" || runs != 0 {
					t.Fatalf("expected pending confirmation and no execution: id=%q runs=%d", confirmationID, runs)
				}
				response := &genai.Content{Role: "user", Parts: []*genai.Part{{FunctionResponse: &genai.FunctionResponse{
					ID: confirmationID, Name: toolconfirmation.FunctionCallName, Response: map[string]any{"confirmed": confirmed},
				}}}}
				for _, err := range r.Run(context.Background(), "user", "session", response, agent.RunConfig{}) {
					if err != nil {
						t.Fatal(err)
					}
				}
				want := 0
				if confirmed {
					want = 1
				}
				if runs != want {
					t.Fatalf("approved=%t: executed %d times, want %d", confirmed, runs, want)
				}
			})
		}
	}
}

func TestMCPResumeDispatcherUsesOnlyCurrentValidatedHandle(t *testing.T) {
	d := &MCPDiscovery{runs: make(map[string]*mcpRun)}
	proxy := d.resumeTools()[0].(*mcpResumeTool)
	ctx := newFakeContext(newFakeSession("session"))
	if _, err := proxy.Run(ctx, map[string]any{}); err == nil {
		t.Fatal("missing invocation was accepted")
	}
	selected := &discoveredMCPTool{spelling: "appa:" + ReservedTool}
	run := d.run(ctx.InvocationID())
	run.selected = map[string]*discoveredMCPTool{ReservedTool: selected}
	if _, ok := proxy.selected(ctx); ok {
		t.Fatal("unopened inventory was accepted")
	}
	run.opened = true
	if got, ok := proxy.selected(ctx); !ok || got != selected {
		t.Fatal("current selection not used")
	}
	other := newFakeContext(newFakeSession("other")).forInvocation("other")
	if _, ok := proxy.selected(other); ok {
		t.Fatal("another invocation borrowed the handle")
	}
	req := &model.LLMRequest{}
	if err := proxy.ProcessRequest(ctx, req); err != nil {
		t.Fatal(err)
	}
	if len(req.Tools) != 0 || req.Config != nil {
		t.Fatal("resume dispatcher advertised a model tool")
	}
	delete(run.selected, ReservedTool)
	if _, ok := proxy.selected(ctx); ok {
		t.Fatal("removed tool remained available")
	}
	d.close(ctx.InvocationID())
	if _, ok := proxy.selected(ctx); ok {
		t.Fatal("closed invocation remained available")
	}
}
