package appakagentadk

import (
	"context"
	"encoding/json"
	"fmt"
	"regexp"
	"sort"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

const discoveryMaxTools = 10_000
const discoveryMaxBytes = 10 * 1024 * 1024

var discoveredToolName = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)

type DiscoveryStatus string

const (
	DiscoveryComplete    DiscoveryStatus = "complete"
	DiscoveryPartial     DiscoveryStatus = "partial"
	DiscoveryUnavailable DiscoveryStatus = "unavailable"
)

// Discovery is metadata evidence, not permission to execute the returned tools.
type Discovery struct {
	Tools  []*mcp.Tool
	Status DiscoveryStatus
	Detail string
}

// Discover reuses the host's authenticated session. It never creates a transport
// or invokes a tool, and the host remains responsible for session cleanup.
func Discover(ctx context.Context, session *mcp.ClientSession, filter []string) (Discovery, error) {
	deadline, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	found := make(map[string]*mcp.Tool)
	selected := make(map[string]bool)
	for _, name := range filter {
		selected[name] = true
	}
	result := func(status DiscoveryStatus, detail string) Discovery {
		tools := make([]*mcp.Tool, 0, len(found))
		for name, tool := range found {
			if len(selected) == 0 || selected[name] {
				tools = append(tools, tool)
			}
		}
		sort.Slice(tools, func(i, j int) bool { return tools[i].Name < tools[j].Name })
		return Discovery{Tools: tools, Status: status, Detail: detail}
	}
	cursor := ""
	cursors := make(map[string]bool)
	size := 0
	for {
		page, err := session.ListTools(deadline, &mcp.ListToolsParams{Cursor: cursor})
		if err != nil {
			if ctx.Err() != nil {
				return Discovery{}, ctx.Err()
			}
			status := DiscoveryUnavailable
			if len(found) != 0 {
				status = DiscoveryPartial
			}
			// Transport errors can quote credentials; retain no exception text.
			return result(status, "MCP metadata discovery did not complete"), nil
		}
		if page == nil {
			return Discovery{}, fmt.Errorf("MCP tool listing has no result")
		}
		encoded, err := json.Marshal(page)
		if err != nil {
			return Discovery{}, fmt.Errorf("MCP tool listing contains invalid metadata")
		}
		size += len(encoded)
		if size > discoveryMaxBytes || len(found)+len(page.Tools) > discoveryMaxTools {
			return Discovery{}, fmt.Errorf("MCP tool listing exceeds the metadata discovery limit")
		}
		for _, tool := range page.Tools {
			if tool == nil || !discoveredToolName.MatchString(tool.Name) || found[tool.Name] != nil {
				return Discovery{}, fmt.Errorf("MCP tool listing contains an invalid or duplicate tool")
			}
			found[tool.Name] = tool
		}
		cursor = page.NextCursor
		if cursor == "" {
			break
		}
		if cursors[cursor] {
			return Discovery{}, fmt.Errorf("MCP tool listing repeats a pagination cursor")
		}
		cursors[cursor] = true
	}
	for name := range selected {
		if found[name] == nil {
			return Discovery{}, fmt.Errorf("MCP tool filter names tools absent from the complete listing")
		}
	}
	return result(DiscoveryComplete, ""), nil
}
