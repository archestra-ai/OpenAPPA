package appakagentadk

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

func discoverySession(t *testing.T, pages []*mcp.ListToolsResult) *mcp.ClientSession {
	t.Helper()
	server := mcp.NewServer(&mcp.Implementation{Name: "discovery-test", Version: "1"}, nil)
	index := 0
	server.AddReceivingMiddleware(func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, request mcp.Request) (mcp.Result, error) {
			if method != "tools/list" {
				return next(ctx, method, request)
			}
			if index >= len(pages) {
				return nil, fmt.Errorf("secret-auth-token")
			}
			page := pages[index]
			index++
			return page, nil
		}
	})
	client := mcp.NewClient(&mcp.Implementation{Name: "discovery-client", Version: "1"}, nil)
	st, ct := mcp.NewInMemoryTransports()
	ss, err := server.Connect(context.Background(), st, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ss.Close() })
	cs, err := client.Connect(context.Background(), ct, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { cs.Close() })
	return cs
}

func discoveryPage(cursor string, names ...string) *mcp.ListToolsResult {
	page := &mcp.ListToolsResult{NextCursor: cursor, Tools: []*mcp.Tool{}}
	for _, name := range names {
		page.Tools = append(page.Tools, &mcp.Tool{Name: name, InputSchema: map[string]any{"type": "object"}})
	}
	return page
}

func TestDiscoveryPaginationAndOptionalFilter(t *testing.T) {
	for _, filter := range [][]string{nil, {"read"}} {
		session := discoverySession(t, []*mcp.ListToolsResult{discoveryPage("next", "write"), discoveryPage("", "read")})
		result, err := Discover(context.Background(), session, filter)
		if err != nil || result.Status != DiscoveryComplete {
			t.Fatalf("%+v: %v", result, err)
		}
		want := 2
		if len(filter) != 0 {
			want = 1
		}
		if len(result.Tools) != want || result.Tools[0].Name != "read" {
			t.Fatalf("%+v", result)
		}
	}
}

func TestDiscoveryIncompleteEvidenceAndKnownErrors(t *testing.T) {
	for _, tc := range []struct {
		name    string
		pages   []*mcp.ListToolsResult
		filter  []string
		status  DiscoveryStatus
		invalid bool
	}{
		{name: "unavailable", status: DiscoveryUnavailable},
		{name: "partial", pages: []*mcp.ListToolsResult{discoveryPage("next", "read")}, status: DiscoveryPartial},
		{name: "duplicate", pages: []*mcp.ListToolsResult{discoveryPage("next", "read"), discoveryPage("", "read")}, invalid: true},
		{name: "invalid-name", pages: []*mcp.ListToolsResult{discoveryPage("", "invalid/name")}, invalid: true},
		{name: "empty-name", pages: []*mcp.ListToolsResult{discoveryPage("", "")}, invalid: true},
		{name: "cursor-cycle", pages: []*mcp.ListToolsResult{discoveryPage("next", "read"), discoveryPage("next", "write")}, invalid: true},
		{name: "missing-filter", pages: []*mcp.ListToolsResult{discoveryPage("", "read")}, filter: []string{"missing"}, invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			result, err := Discover(context.Background(), discoverySession(t, tc.pages), tc.filter)
			if (err != nil) != tc.invalid {
				t.Fatalf("%+v: %v", result, err)
			}
			if !tc.invalid && result.Status != tc.status {
				t.Fatalf("%+v", result)
			}
			if strings.Contains(result.Detail, "secret-auth-token") {
				t.Fatal("transport credential leaked")
			}
		})
	}
}

func TestDiscoveryLimitsAndCancellation(t *testing.T) {
	oversized := discoveryPage("", "read")
	oversized.Tools[0].Description = strings.Repeat("x", discoveryMaxBytes)
	if _, err := Discover(context.Background(), discoverySession(t, []*mcp.ListToolsResult{oversized}), []string{"other"}); err == nil {
		t.Fatal("metadata byte limit was bypassed by a filter")
	}
	many := discoveryPage("")
	for i := 0; i <= discoveryMaxTools; i++ {
		many.Tools = append(many.Tools, &mcp.Tool{Name: fmt.Sprintf("tool_%d", i), InputSchema: map[string]any{"type": "object"}})
	}
	if _, err := Discover(context.Background(), discoverySession(t, []*mcp.ListToolsResult{many}), nil); err == nil {
		t.Fatal("metadata tool-count limit was not enforced")
	}
	session := discoverySession(t, nil)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := Discover(ctx, session, nil); err != context.Canceled {
		t.Fatalf("cancellation did not propagate: %v", err)
	}
}
