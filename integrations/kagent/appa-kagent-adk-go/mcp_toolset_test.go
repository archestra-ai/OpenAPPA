package appakagentadk

import (
	"context"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/adk/v2/agent"
)

func TestDiscoveredToolsetPaginatesAndDispatchesOnTheSameSession(t *testing.T) {
	server := mcp.NewServer(&mcp.Implementation{Name: "fixture"}, nil)
	generation, calls := 0, 0
	var listingSession mcp.Session
	server.AddReceivingMiddleware(func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, request mcp.Request) (mcp.Result, error) {
			switch method {
			case "tools/list":
				listingSession = request.GetSession()
				params := request.GetParams().(*mcp.ListToolsParams)
				if params.Cursor == "" {
					return discoveryPage("second", "read"), nil
				}
				if generation == 0 {
					return discoveryPage(""), nil
				}
				return discoveryPage("", "late"), nil
			case "tools/call":
				if request.GetSession() != listingSession {
					t.Error("execution changed MCP sessions")
				}
				calls++
				return &mcp.CallToolResult{Content: []mcp.Content{&mcp.TextContent{Text: "exact result"}}}, nil
			}
			return next(ctx, method, request)
		}
	})
	st, ct := mcp.NewInMemoryTransports()
	session, err := server.Connect(context.Background(), st, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer session.Close()
	set, err := newDiscoveredToolset(ct, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx := newFakeContext(newFakeSession("actor"))
	tools, observation, err := set.discover(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer observation.session.Close()
	if len(tools) != 1 || observation.discovery.Status != DiscoveryComplete || calls != 0 {
		t.Fatalf("initial discovery: %v %+v calls=%d", tools, observation.discovery, calls)
	}
	generation++
	tools, later, err := set.discover(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(tools) != 2 || later.session != observation.session || calls != 0 {
		t.Fatalf("late discovery: %v calls=%d", tools, calls)
	}
	runnable := tools[0].(interface {
		Run(agent.Context, any) (map[string]any, error)
	})
	result, err := runnable.Run(ctx, map[string]any{})
	if err != nil || result["output"] != "exact result" || calls != 1 {
		t.Fatalf("dispatch: %v %v calls=%d", result, err, calls)
	}
	approvedTool := &discoveredMCPTool{mcpRunnable: tools[0].(mcpRunnable), spelling: MCPSpelling("fixture", tools[0].Name()), approvals: map[string]bool{tools[0].Name(): true}}
	result, err = approvedTool.Run(ctx, map[string]any{})
	if err != nil || result["status"] != "confirmation_requested" || len(ctx.requested) != 1 || calls != 1 {
		t.Fatalf("native approval was bypassed: %v %v calls=%d", result, err, calls)
	}
	result, err = approvedTool.Run(ctx.resumed(false), map[string]any{})
	if err != nil || result["result"] != "Tool call was rejected by user." || calls != 1 {
		t.Fatalf("native rejection was bypassed: %v %v calls=%d", result, err, calls)
	}
	result, err = approvedTool.Run(ctx.resumed(true), map[string]any{})
	if err != nil || result["output"] != "exact result" || calls != 2 {
		t.Fatalf("native approval did not execute normally: %v %v calls=%d", result, err, calls)
	}
}

func TestMCPAppOnlyToolsStayHidden(t *testing.T) {
	for _, visibility := range []any{"app", []string{"app"}, []any{"app"}} {
		if !mcpAppOnly(mcp.Meta{"ui": map[string]any{"visibility": visibility}}) {
			t.Fatal(visibility)
		}
	}
	for _, visibility := range []any{nil, "model", []string{"app", "model"}, []any{"app", "model"}} {
		if mcpAppOnly(mcp.Meta{"ui": map[string]any{"visibility": visibility}}) {
			t.Fatal(visibility)
		}
	}
}
