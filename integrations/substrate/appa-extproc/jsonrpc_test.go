package main

import (
	"encoding/json"
	"testing"
)

func TestParseToolCall(t *testing.T) {
	call, err := parseToolCall([]byte(`{"jsonrpc":"2.0","id":"a1","method":"tools/call","params":{"name":"add_issue_comment","arguments":{"body":"x"}}}`))
	if err != nil || call == nil || call.Name != "add_issue_comment" || string(call.ID) != `"a1"` || string(call.Arguments) != `{"body":"x"}` {
		t.Fatalf("got %+v, %v", call, err)
	}
	call, err = parseToolCall([]byte(`{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_me"}}`))
	if err != nil || string(call.Arguments) != "{}" {
		t.Fatalf("absent arguments should cross as {}, got %+v, %v", call, err)
	}
	if _, err := parseToolCall([]byte(`{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"../x"}}`)); err == nil {
		t.Fatal("a tool name outside the spelling must be refused")
	}
}

func TestParseToolResultFramings(t *testing.T) {
	id := json.RawMessage("4")
	answer := `{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"hello"}],"isError":false}}`
	if got := parseToolResult("application/json", []byte(answer), id); !got.Found || got.Text != "hello" || got.Failed {
		t.Fatalf("json: %+v", got)
	}
	stream := "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\r\ndata: " + answer + "\r\n\r\n"
	if got := parseToolResult("text/event-stream; charset=utf-8", []byte(stream), id); !got.Found || got.Text != "hello" {
		t.Fatalf("sse: %+v", got)
	}
	file := `{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"downloaded"},{"type":"resource","resource":{"uri":"repo://o/r/README.md","text":"# notes"}}]}}`
	if got := parseToolResult("application/json", []byte(file), id); got.Text != "downloaded\n# notes" {
		t.Fatalf("resource: %+v", got)
	}
	failed := `{"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"bad"}}`
	if got := parseToolResult("application/json", []byte(failed), id); !got.Found || !got.Failed || got.Text != "bad" {
		t.Fatalf("error: %+v", got)
	}
	if got := parseToolResult("application/json", []byte(answer), json.RawMessage("5")); got.Found {
		t.Fatalf("another call's answer must not match: %+v", got)
	}
}

func TestReplacementKeepsFraming(t *testing.T) {
	message := toolError(json.RawMessage("9"), "blocked")
	framed := frame("text/event-stream", message)
	if got := parseToolResult("text/event-stream", framed, json.RawMessage("9")); !got.Found || !got.Failed || got.Text != "blocked" {
		t.Fatalf("got %+v from %q", got, framed)
	}
}
