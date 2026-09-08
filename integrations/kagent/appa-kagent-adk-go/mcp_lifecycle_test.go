package appakagentadk

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/kagent-dev/kagent/go/api/adk"
	"github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/adk/v2/model"
	"google.golang.org/genai"
)

func TestDiscoveryRequiresUnambiguousConfiguredSources(t *testing.T) {
	reserved := adk.HttpMcpServerConfig{Params: adk.StreamableHTTPConnectionParams{Url: "http://appa:8080/mcp"}, Tools: []string{ReservedTool}}
	external := adk.HttpMcpServerConfig{Params: adk.StreamableHTTPConnectionParams{Url: "https://mcp.example.com/mcp"}}
	for _, config := range []*adk.AgentConfig{nil, {}, {HttpTools: []adk.HttpMcpServerConfig{external}}, {HttpTools: []adk.HttpMcpServerConfig{external, external, reserved}}} {
		if _, err := newMCPDiscovery(config, nil); err == nil {
			t.Fatal("accepted missing runtime source or duplicate endpoint")
		}
	}
	discovery, err := newMCPDiscovery(&adk.AgentConfig{HttpTools: []adk.HttpMcpServerConfig{external, reserved}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if discovery.connections[0].control || !discovery.connections[1].control || len(discovery.connections[0].names) != 0 {
		t.Fatal("external source became a control toolset or acquired a filter")
	}
	id, err := mcpSourceID(external.Params.Url)
	if err != nil || discovery.connections[0].server != id {
		t.Fatal("configured and discovered source identities differ")
	}
}

func discoveryRuntime(t *testing.T) string {
	t.Helper()
	binary := os.Getenv("APPA_TEST_RUNTIME_BIN")
	if binary == "" {
		t.Skip("set APPA_TEST_RUNTIME_BIN to run against the real APPA runtime")
	}
	directory := t.TempDir()
	config := filepath.Join(directory, "appa.toml")
	if err := os.WriteFile(config, []byte("[externals]\ntimeout_ms = 30000\nmax_body_bytes = 65536\n[policy]\nversion = 2\n[[policy.tool]]\nname = 'read'\n[[policy.tool]]\nname = 'late'\n"), 0600); err != nil {
		t.Fatal(err)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := listener.Addr().String()
	listener.Close()
	log, err := os.Create(filepath.Join(directory, "runtime.log"))
	if err != nil {
		t.Fatal(err)
	}
	command := exec.Command(binary, "runtime", "--config", config, "--db", filepath.Join(directory, "appa.db"), "--adapter", "kagent", "--listen", address)
	command.Stdout = log
	command.Stderr = log
	if err := command.Start(); err != nil {
		log.Close()
		t.Fatal(err)
	}
	t.Cleanup(func() { command.Process.Kill(); command.Wait(); log.Close() })
	client := http.Client{Timeout: time.Second}
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		response, err := client.Get("http://" + address + "/health")
		if err == nil {
			response.Body.Close()
			if response.StatusCode == 200 {
				return "http://" + address
			}
		}
		time.Sleep(20 * time.Millisecond)
	}
	data, _ := os.ReadFile(filepath.Join(directory, "runtime.log"))
	t.Fatalf("runtime did not start: %s", data)
	return ""
}

func TestMCPDiscoveryLifecycleAgainstRuntime(t *testing.T) {
	runtimeURL := discoveryRuntime(t)
	server := mcp.NewServer(&mcp.Implementation{Name: "lifecycle-fixture"}, nil)
	var generation, calls atomic.Int32
	server.AddReceivingMiddleware(func(next mcp.MethodHandler) mcp.MethodHandler {
		return func(ctx context.Context, method string, request mcp.Request) (mcp.Result, error) {
			switch method {
			case "tools/list":
				if generation.Load() == 2 {
					return discoveryPage("", "read", "read"), nil
				}
				params := request.GetParams().(*mcp.ListToolsParams)
				if params.Cursor == "" {
					return discoveryPage("next", "read"), nil
				}
				if generation.Load() == 3 {
					return nil, fmt.Errorf("fixture page unavailable")
				}
				if generation.Load() == 0 {
					return discoveryPage(""), nil
				}
				return discoveryPage("", "late", "uncovered"), nil
			case "tools/call":
				calls.Add(1)
				return &mcp.CallToolResult{Content: []mcp.Content{&mcp.TextContent{Text: "original result bytes"}}}, nil
			}
			return next(ctx, method, request)
		}
	})
	handler := mcp.NewStreamableHTTPHandler(func(*http.Request) *mcp.Server { return server }, nil)
	endpoint := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "fixture-token" {
			http.Error(w, "unauthorized", 401)
			return
		}
		handler.ServeHTTP(w, r)
	}))
	defer endpoint.Close()
	discovery := &MCPDiscovery{connections: []mcpConnection{{params: mcpServerParams{URL: endpoint.URL, Headers: map[string]string{"Authorization": "fixture-token"}}, server: "fixture"}}, runs: make(map[string]*mcpRun)}
	plugin, err := New(Config{RuntimeURL: runtimeURL, Discovery: discovery})
	if err != nil {
		t.Fatal(err)
	}
	ctx := newFakeContext(newFakeSession("discovery-lifecycle"))
	message := &genai.Content{Role: "user", Parts: []*genai.Part{{Text: "read the data"}}}
	if _, err := plugin.onUserMessage(ctx, message); err != nil {
		t.Fatal(err)
	}
	defer discovery.close(ctx.InvocationID())
	request := &model.LLMRequest{}
	if err := discovery.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if len(request.Tools) != 1 || request.Tools["read"] == nil || calls.Load() != 0 {
		t.Fatalf("initial tools=%v calls=%d", request.Tools, calls.Load())
	}
	generation.Store(1)
	request = &model.LLMRequest{}
	if err := discovery.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if request.Tools["late"] == nil || request.Tools["read"] == nil || request.Tools["uncovered"] != nil {
		t.Fatalf("late tool filtering: %v", request.Tools)
	}
	candidate := request.Tools["late"].(*discoveredMCPTool)
	args := map[string]any{}
	result, err := plugin.beforeTool(ctx, candidate, args)
	if err != nil || result != nil {
		t.Fatalf("covered late call refused: %v %v", result, err)
	}
	result, err = candidate.Run(ctx, args)
	if err != nil {
		t.Fatal(err)
	}
	if result["output"] != "original result bytes" || calls.Load() != 1 {
		t.Fatalf("result=%v calls=%d", result, calls.Load())
	}
	if _, err := plugin.afterTool(ctx, candidate, args, result, nil); err != nil {
		t.Fatal(err)
	}
	if calls.Load() != 1 {
		t.Fatal("validation executed an extra tool")
	}
	// An invalid replacement listing must not erase working tools.
	generation.Store(2)
	request = &model.LLMRequest{}
	if err := discovery.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if request.Tools["read"] == nil || request.Tools["late"] == nil || request.Tools["uncovered"] != nil {
		t.Fatalf("invalid update erased working tools: %v", request.Tools)
	}
	// A failed later page is not evidence that previously listed tools vanished.
	generation.Store(3)
	request = &model.LLMRequest{}
	if err := discovery.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if request.Tools["read"] == nil || request.Tools["late"] == nil || request.Tools["uncovered"] != nil {
		t.Fatalf("partial update erased working tools: %v", request.Tools)
	}
	// Recreate both APPA plugin and MCP sessions. Even an apparently fresh
	// host context must use durable reservations and isolate the late invalid tool.
	generation.Store(1)
	discovery.close(ctx.InvocationID())
	restarted := &MCPDiscovery{connections: discovery.connections, runs: make(map[string]*mcpRun)}
	plugin, err = New(Config{RuntimeURL: runtimeURL, Discovery: restarted})
	if err != nil {
		t.Fatal(err)
	}
	ctx = newFakeContext(newFakeSession("discovery-lifecycle"))
	if _, err := plugin.onUserMessage(ctx, message); err != nil {
		t.Fatal(err)
	}
	defer restarted.close(ctx.InvocationID())
	request = &model.LLMRequest{}
	if err := restarted.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if request.Tools["read"] == nil || request.Tools["late"] == nil || request.Tools["uncovered"] != nil {
		t.Fatalf("restart lost pinned validation: %v", request.Tools)
	}
	// The same uncovered tool is a known configuration error for a NEW
	// trajectory. No MCP execution occurs and failed startup closes its sessions.
	initial := &MCPDiscovery{connections: discovery.connections, runs: make(map[string]*mcpRun)}
	plugin, err = New(Config{RuntimeURL: runtimeURL, Discovery: initial})
	if err != nil {
		t.Fatal(err)
	}
	fresh := newFakeContext(newFakeSession("initial-invalid"))
	if _, err := plugin.onUserMessage(fresh, message); err == nil {
		t.Fatal("activated a known uncovered tool")
	}
	if len(initial.runs) != 0 || calls.Load() != 1 {
		t.Fatalf("failed activation leaked a session or executed a tool: runs=%d calls=%d", len(initial.runs), calls.Load())
	}
}

func TestMCPValidationRefusesMalformedSuccess(t *testing.T) {
	for index, body := range []string{
		`{}`, `null`,
		`{"tools":[],"errors":[],"accepted_tools":[]}`,
		`{"tools":[{"tool":"read","status":"unknown"}],"errors":[],"accepted_tools":[]}`,
		`{"tools":[{"tool":"read","status":"allowed"}],"errors":[],"accepted_tools":[]}`,
		`{"tools":[{"tool":"read","status":"valid"}],"errors":[],"accepted_tools":[]} {}`,
	} {
		t.Run(fmt.Sprint(index), func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { fmt.Fprint(w, body) }))
			defer server.Close()
			d := &MCPDiscovery{runs: make(map[string]*mcpRun)}
			if _, err := New(Config{RuntimeURL: server.URL, Discovery: d}); err != nil {
				t.Fatal(err)
			}
			_, err := d.validate(context.Background(), trajectoryIDs{}, observedInventory{Tools: []observedTool{{Name: "read", Tool: "mcp:fixture/read"}}}, false)
			if err == nil {
				t.Fatal("accepted malformed or incomplete validation response")
			}
		})
	}
}
