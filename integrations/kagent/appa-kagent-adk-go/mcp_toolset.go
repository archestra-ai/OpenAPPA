package appakagentadk

import (
	"context"
	"fmt"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/mcptoolset"
)

// Each invocation owns its toolsets. ADK retains its normal tool execution and
// result conversion; only tools/list passes through bounded discovery.
type discoveredToolset struct {
	inner tool.Toolset
}

type discoveryContextKey struct{}
type discoveryPageKey struct{}
type toolsetObservation struct {
	discovery Discovery
	session   *mcp.ClientSession
}
type discoveryContext struct {
	agent.ReadonlyContext
	observation *toolsetObservation
	deadline    context.Context
}

func (ctx discoveryContext) Deadline() (time.Time, bool) { return ctx.deadline.Deadline() }
func (ctx discoveryContext) Done() <-chan struct{}       { return ctx.deadline.Done() }
func (ctx discoveryContext) Err() error                  { return ctx.deadline.Err() }
func (ctx discoveryContext) Value(key any) any {
	if _, ok := key.(discoveryContextKey); ok {
		return ctx.observation
	}
	return ctx.ReadonlyContext.Value(key)
}

func newDiscoveredToolset(transport mcp.Transport, filter []string) (*discoveredToolset, error) {
	capabilities := &mcp.ClientCapabilities{}
	capabilities.AddExtension("io.modelcontextprotocol/ui", map[string]any{"mimeTypes": []string{"text/html;profile=mcp-app"}})
	client := mcp.NewClient(&mcp.Implementation{Name: "kagent-adk"}, &mcp.ClientOptions{Capabilities: capabilities})
	client.AddSendingMiddleware(func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, request mcp.Request) (mcp.Result, error) {
			if method != "tools/list" || ctx.Value(discoveryPageKey{}) != nil {
				return next(ctx, method, request)
			}
			observation, ok := ctx.Value(discoveryContextKey{}).(*toolsetObservation)
			if !ok {
				return nil, fmt.Errorf("MCP discovery has no invocation scope")
			}
			session, ok := request.GetSession().(*mcp.ClientSession)
			if !ok {
				return nil, fmt.Errorf("MCP discovery has no client session")
			}
			observation.session = session
			result, err := Discover(context.WithValue(ctx, discoveryPageKey{}, true), session, filter)
			observation.discovery = result
			if err != nil {
				return nil, err
			}
			if result.Status == DiscoveryUnavailable && result.failure != nil {
				// ADK owns retry/reconnection. Only classify a failed connection
				// as unknown after its normal reconnect attempt has completed.
				return nil, result.failure
			}
			visible := make([]*mcp.Tool, 0, len(result.Tools))
			for _, candidate := range result.Tools {
				if !mcpAppOnly(candidate.Meta) {
					visible = append(visible, candidate)
				}
			}
			return &mcp.ListToolsResult{Tools: visible}, nil
		}
	})
	inner, err := mcptoolset.New(mcptoolset.Config{Client: client, Transport: transport})
	if err != nil {
		return nil, err
	}
	return &discoveredToolset{inner: inner}, nil
}

func (set *discoveredToolset) discover(ctx agent.ReadonlyContext) ([]tool.Tool, toolsetObservation, error) {
	deadline, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	observation := toolsetObservation{discovery: Discovery{Status: DiscoveryUnavailable, Detail: "MCP connection or metadata discovery did not complete"}}
	tools, err := set.inner.Tools(discoveryContext{ReadonlyContext: ctx, observation: &observation, deadline: deadline})
	if err != nil && ctx.Err() != nil {
		return nil, observation, ctx.Err()
	}
	if err != nil && deadline.Err() != nil {
		observation.discovery = Discovery{Status: DiscoveryUnavailable, Detail: "MCP metadata discovery timed out"}
		return nil, observation, nil
	}
	if err != nil && observation.session == nil {
		return nil, observation, nil
	}
	if err != nil && observation.discovery.Status == DiscoveryUnavailable && observation.discovery.failure != nil {
		return nil, observation, nil
	}
	return tools, observation, err
}

// Match kagent's MCP Apps visibility handling before ADK drops raw metadata.
func mcpAppOnly(meta mcp.Meta) bool {
	ui, _ := meta["ui"].(map[string]any)
	switch visibility := ui["visibility"].(type) {
	case string:
		return visibility == "app"
	case []string:
		hasApp := false
		for _, item := range visibility {
			if item == "model" {
				return false
			}
			hasApp = hasApp || item == "app"
		}
		return hasApp
	case []any:
		hasApp := false
		for _, item := range visibility {
			if item == "model" {
				return false
			}
			hasApp = hasApp || item == "app"
		}
		return hasApp
	}
	return false
}
