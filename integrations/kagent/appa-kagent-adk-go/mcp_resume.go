package appakagentadk

import (
	"sort"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/genai"
)

// ADK resumes confirmations from the agent's static tools, before request
// processors populate the model's dynamic tools. Register dispatchers for the
// names that can pause, but never retain a previous invocation's MCP handle.
func (d *MCPDiscovery) resumeTools() []tool.Tool {
	names := map[string]bool{ReservedTool: true}
	for _, connection := range d.connections {
		for name, required := range connection.approvals {
			if required {
				names[name] = true
			}
		}
	}
	ordered := make([]string, 0, len(names))
	for name := range names {
		ordered = append(ordered, name)
	}
	sort.Strings(ordered)
	result := make([]tool.Tool, 0, len(ordered))
	for _, name := range ordered {
		result = append(result, &mcpResumeTool{name: name, discovery: d})
	}
	return result
}

type mcpResumeTool struct {
	name      string
	discovery *MCPDiscovery
}

func (t *mcpResumeTool) Name() string      { return t.name }
func (*mcpResumeTool) Description() string { return "" }
func (*mcpResumeTool) IsLongRunning() bool { return false }
func (t *mcpResumeTool) Declaration() *genai.FunctionDeclaration {
	return &genai.FunctionDeclaration{Name: t.name}
}

// Discovery alone publishes the validated schemas and callable tools. A resume
// dispatcher must neither advertise a placeholder schema nor replace that tool.
func (*mcpResumeTool) ProcessRequest(agent.Context, *model.LLMRequest) error { return nil }

func (t *mcpResumeTool) selected(ctx agent.Context) (*discoveredMCPTool, bool) {
	d := t.discovery
	d.mu.Lock()
	run := d.runs[ctx.InvocationID()]
	d.mu.Unlock()
	if run == nil {
		return nil, false
	}
	run.mu.Lock()
	defer run.mu.Unlock()
	selected := run.selected[t.name]
	return selected, run.opened && selected != nil
}

func (t *mcpResumeTool) Run(ctx agent.Context, args any) (map[string]any, error) {
	selected, ok := t.selected(ctx)
	if !ok {
		return nil, failClosed("MCP tool %s is unavailable in this invocation", t.name)
	}
	return selected.Run(ctx, args)
}
