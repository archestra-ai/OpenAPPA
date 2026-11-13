// MCP over streamable HTTP, as far as the processor reads it: a POST carries
// one JSON-RPC message, and the answer is either one JSON object or an SSE
// stream whose events carry JSON-RPC messages.

package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"strings"
)

// toolCall is a `tools/call` request: its id, tool name, and arguments.
type toolCall struct {
	ID        json.RawMessage
	Name      string
	Arguments json.RawMessage
}

// errBatch marks a JSON-RPC batch that holds a tools/call. The processor
// gates one call per request, so it refuses the batch instead of guessing.
var errBatch = errors.New("a JSON-RPC batch that holds a tools/call")

// parseToolCall reads a request body. It answers nil for any message that is
// not a tools/call, and an error for a body it cannot read as JSON-RPC: a
// request the processor cannot read never reaches a tool unchecked.
func parseToolCall(body []byte) (*toolCall, error) {
	trimmed := bytes.TrimSpace(body)
	if len(trimmed) == 0 {
		return nil, nil
	}
	if trimmed[0] == '[' {
		var batch []struct {
			Method string `json:"method"`
		}
		if err := json.Unmarshal(trimmed, &batch); err != nil {
			return nil, err
		}
		for _, message := range batch {
			if message.Method == "tools/call" {
				return nil, errBatch
			}
		}
		return nil, nil
	}
	var message struct {
		ID     json.RawMessage `json:"id"`
		Method string          `json:"method"`
		Params struct {
			Name      string          `json:"name"`
			Arguments json.RawMessage `json:"arguments"`
		} `json:"params"`
	}
	if err := json.Unmarshal(trimmed, &message); err != nil {
		return nil, err
	}
	if message.Method != "tools/call" {
		return nil, nil
	}
	if !validSegment(message.Params.Name) {
		return nil, errors.New("a tools/call without a valid tool name")
	}
	arguments := message.Params.Arguments
	if len(bytes.TrimSpace(arguments)) == 0 || string(bytes.TrimSpace(arguments)) == "null" {
		arguments = json.RawMessage("{}")
	}
	return &toolCall{ID: message.ID, Name: message.Params.Name, Arguments: arguments}, nil
}

// toolResult is what one tools/call returned.
type toolResult struct {
	// Text is the result's text content, joined.
	Text string
	// Failed is a JSON-RPC error or a result with isError set.
	Failed bool
	Found  bool
}

// parseToolResult finds the response to the call with this id in an answer
// body, framed as JSON or as an SSE stream.
func parseToolResult(contentType string, body []byte, id json.RawMessage) toolResult {
	if strings.HasPrefix(strings.ToLower(strings.TrimSpace(contentType)), "text/event-stream") {
		for _, data := range sseData(body) {
			if result := matchResponse([]byte(data), id); result.Found {
				return result
			}
		}
		return toolResult{}
	}
	return matchResponse(body, id)
}

func matchResponse(raw []byte, id json.RawMessage) toolResult {
	var message struct {
		ID     json.RawMessage `json:"id"`
		Result *struct {
			Content []struct {
				Type     string `json:"type"`
				Text     string `json:"text"`
				Resource *struct {
					Text string `json:"text"`
				} `json:"resource"`
			} `json:"content"`
			IsError bool `json:"isError"`
		} `json:"result"`
		Error *struct {
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(bytes.TrimSpace(raw), &message); err != nil {
		return toolResult{}
	}
	if !sameID(message.ID, id) {
		return toolResult{}
	}
	if message.Error != nil {
		return toolResult{Text: message.Error.Message, Failed: true, Found: true}
	}
	if message.Result == nil {
		return toolResult{}
	}
	var texts []string
	for _, block := range message.Result.Content {
		switch {
		case block.Type == "text":
			texts = append(texts, block.Text)
		// A file read returns its content as an embedded resource.
		case block.Type == "resource" && block.Resource != nil && block.Resource.Text != "":
			texts = append(texts, block.Resource.Text)
		}
	}
	return toolResult{Text: strings.Join(texts, "\n"), Failed: message.Result.IsError, Found: true}
}

// toolError is a complete JSON-RPC answer to a call: a tool result carrying
// text and isError, which a client hands its model as the tool's output.
func toolError(id json.RawMessage, text string) []byte {
	return toolAnswer(id, text, true)
}

func toolAnswer(id json.RawMessage, text string, isError bool) []byte {
	if len(id) == 0 {
		id = json.RawMessage("null")
	}
	body, _ := json.Marshal(map[string]any{
		"jsonrpc": "2.0",
		"id":      id,
		"result": map[string]any{
			"content": []map[string]string{{"type": "text", "text": text}},
			"isError": isError,
		},
	})
	return body
}

// frame wraps one JSON-RPC message the way the answer it replaces was framed.
func frame(contentType string, message []byte) []byte {
	if strings.HasPrefix(strings.ToLower(strings.TrimSpace(contentType)), "text/event-stream") {
		return []byte("event: message\ndata: " + string(message) + "\n\n")
	}
	return message
}

// sseData is the data of each event in an SSE stream, multi-line data joined.
func sseData(body []byte) []string {
	var events []string
	var current []string
	flush := func() {
		if len(current) > 0 {
			events = append(events, strings.Join(current, "\n"))
			current = nil
		}
	}
	for _, line := range strings.Split(strings.ReplaceAll(string(body), "\r\n", "\n"), "\n") {
		switch {
		case line == "":
			flush()
		case strings.HasPrefix(line, "data:"):
			current = append(current, strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
		}
	}
	flush()
	return events
}

func sameID(a, b json.RawMessage) bool {
	return len(a) > 0 && bytes.Equal(bytes.TrimSpace(a), bytes.TrimSpace(b))
}
