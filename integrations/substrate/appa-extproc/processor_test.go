package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"testing"
	"time"

	corev3 "github.com/envoyproxy/go-control-plane/envoy/config/core/v3"
	extprocv3 "github.com/envoyproxy/go-control-plane/envoy/service/ext_proc/v3"
	"google.golang.org/protobuf/types/known/structpb"
)

const actor = "spiffe://cluster.local/worker-for-actor/demo/agent"

// A request to a host that is no MCP server passes on its headers and asks
// for nothing more: the processor never reads a model call or any other
// traffic.
func TestOtherHostsPassUnread(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	answer := p.handle(context.Background(), &ex, headersRequest("api.anthropic.com", "POST", actor))
	if answer.GetRequestHeaders() == nil || answer.GetModeOverride() != nil {
		t.Fatalf("want a plain pass, got %v", answer)
	}
}

func TestMCPPostAsksForBodies(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	answer := p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com:443", "POST", actor))
	if answer.GetModeOverride() == nil {
		t.Fatalf("want the gated mode override, got %v", answer)
	}
	// The notification stream is a GET and runs no tool.
	ex = exchange{}
	answer = p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com", "GET", actor))
	if answer.GetModeOverride() != nil {
		t.Fatalf("a GET must stream unbuffered, got %v", answer)
	}
}

func TestMCPRequestWithoutIdentityIsRefused(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	answer := p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com", "POST", ""))
	if answer.GetImmediateResponse().GetStatus().GetCode() != 403 {
		t.Fatalf("want 403, got %v", answer)
	}
}

// With no runtime to ask, a tool call fails closed: the client gets a tool
// error and the request never reaches the server.
func TestUnreachableRuntimeBlocksTheCall(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com", "POST", actor))
	answer := p.handle(context.Background(), &ex, bodyRequest(callBody(7, "add_issue_comment", `{"owner":"o","repo":"r","issue_number":1,"body":"x"}`)))
	result := immediateToolResult(t, answer)
	if !result.Failed || !strings.Contains(result.Text, "unreachable") {
		t.Fatalf("want an unreachable-runtime tool error, got %+v", result)
	}
}

// A message other than tools/call (initialize, tools/list, notifications)
// passes without a runtime round trip.
func TestNonToolCallMessagesPass(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com", "POST", actor))
	answer := p.handle(context.Background(), &ex, bodyRequest(`{"jsonrpc":"2.0","id":1,"method":"tools/list"}`))
	if answer.GetRequestBody() == nil {
		t.Fatalf("want a pass, got %v", answer)
	}
}

func TestUnreadableBodyIsBlocked(t *testing.T) {
	p := newTestProcessor("http://127.0.0.1:1")
	var ex exchange
	p.handle(context.Background(), &ex, headersRequest("api.githubcopilot.com", "POST", actor))
	for _, body := range []string{`{"jsonrpc":`, `[{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}]`} {
		if answer := p.handle(context.Background(), &ex, bodyRequest(body)); answer.GetImmediateResponse() == nil {
			t.Fatalf("want %q blocked, got %v", body, answer)
		}
	}
}

// The flow against a served runtime: a private read is held until the
// processor takes the narrowing, its result labels the actor, and a public
// write afterwards is refused before it leaves the cluster. Run with
// APPA_TEST_RUNTIME_URL pointing at
// `appa runtime --adapter kagent --config testdata/flow.appa.toml`.
func TestFlowAgainstRuntime(t *testing.T) {
	url := os.Getenv("APPA_TEST_RUNTIME_URL")
	if url == "" {
		t.Skip("set APPA_TEST_RUNTIME_URL to run against a served runtime")
	}
	p := newTestProcessor(url)
	// A runtime keeps its trajectories, so each run names a new one.
	id := fmt.Sprintf("%s/%s-%d", actor, t.Name(), time.Now().UnixNano())

	var read exchange
	p.handle(context.Background(), &read, headersRequest("api.githubcopilot.com", "POST", id))
	answer := p.handle(context.Background(), &read, bodyRequest(callBody(1, "get_file_contents", `{"owner":"acme","repo":"internal","path":"README.md"}`)))
	if answer.GetRequestBody() == nil {
		t.Fatalf("want the read admitted after the narrowing, got %v", answer)
	}
	p.handle(context.Background(), &read, responseHeaders("200", "text/event-stream"))
	answer = p.handle(context.Background(), &read, responseBody(
		"event: message\ndata: "+string(toolAnswer(json.RawMessage("1"), "# roadmap", false))+"\n\n"))
	if answer.GetResponseBody().GetResponse().GetBodyMutation() != nil {
		t.Fatalf("want the result passed unchanged, got %v", answer)
	}

	var write exchange
	p.handle(context.Background(), &write, headersRequest("api.githubcopilot.com", "POST", id))
	answer = p.handle(context.Background(), &write, bodyRequest(callBody(2, "add_issue_comment", `{"owner":"acme","repo":"public","issue_number":1,"body":"# roadmap"}`)))
	result := immediateToolResult(t, answer)
	if !result.Failed || !strings.Contains(result.Text, "cannot run") {
		t.Fatalf("want the public write refused, got %+v", result)
	}

	verdicts := []string{}
	for _, e := range p.config.journal.after(0) {
		verdicts = append(verdicts, e.Verdict)
	}
	want := "held,allowed after narrowing,result labeled,blocked"
	if strings.Join(verdicts, ",") != want {
		t.Fatalf("decision log %v, want %s", verdicts, want)
	}
}

func newTestProcessor(runtimeURL string) *processor {
	return newProcessor(processorConfig{
		servers:         map[string]string{"api.githubcopilot.com": "github"},
		runtime:         newRuntimeClient(runtimeURL),
		journal:         newJournal(50),
		acceptNarrowing: true,
	})
}

func headersRequest(authority, method, identity string) *extprocv3.ProcessingRequest {
	request := &extprocv3.ProcessingRequest{Request: &extprocv3.ProcessingRequest_RequestHeaders{RequestHeaders: &extprocv3.HttpHeaders{
		Headers: &corev3.HeaderMap{Headers: []*corev3.HeaderValue{
			{Key: ":authority", RawValue: []byte(authority)},
			{Key: ":method", RawValue: []byte(method)},
		}},
	}}}
	if identity != "" {
		attributes, _ := structpb.NewStruct(map[string]any{identityAttribute: identity})
		request.Attributes = map[string]*structpb.Struct{"envoy.filters.http.ext_proc": attributes}
	}
	return request
}

func bodyRequest(body string) *extprocv3.ProcessingRequest {
	return &extprocv3.ProcessingRequest{Request: &extprocv3.ProcessingRequest_RequestBody{RequestBody: &extprocv3.HttpBody{Body: []byte(body), EndOfStream: true}}}
}

func responseHeaders(status, contentType string) *extprocv3.ProcessingRequest {
	return &extprocv3.ProcessingRequest{Request: &extprocv3.ProcessingRequest_ResponseHeaders{ResponseHeaders: &extprocv3.HttpHeaders{
		Headers: &corev3.HeaderMap{Headers: []*corev3.HeaderValue{
			{Key: ":status", RawValue: []byte(status)},
			{Key: "content-type", RawValue: []byte(contentType)},
		}},
	}}}
}

func responseBody(body string) *extprocv3.ProcessingRequest {
	return &extprocv3.ProcessingRequest{Request: &extprocv3.ProcessingRequest_ResponseBody{ResponseBody: &extprocv3.HttpBody{Body: []byte(body), EndOfStream: true}}}
}

func callBody(id int, tool, arguments string) string {
	return `{"jsonrpc":"2.0","id":` + string(rune('0'+id)) + `,"method":"tools/call","params":{"name":"` + tool + `","arguments":` + arguments + `}}`
}

func immediateToolResult(t *testing.T, answer *extprocv3.ProcessingResponse) toolResult {
	t.Helper()
	immediate := answer.GetImmediateResponse()
	if immediate == nil {
		t.Fatalf("want an immediate response, got %v", answer)
	}
	var message struct {
		ID json.RawMessage `json:"id"`
	}
	_ = json.Unmarshal(immediate.GetBody(), &message)
	return parseToolResult("application/json", immediate.GetBody(), message.ID)
}
