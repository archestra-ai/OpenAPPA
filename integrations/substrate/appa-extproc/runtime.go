// The OpenAPPA runtime client: the canonical hook wire over POST /hook, and
// the remedy plan over the runtime's MCP endpoint.
//
// The processor speaks for the egress gateway, which carries no harness, so
// it uses the kagent adapter's structured spelling: a call to tool T of MCP
// server S crosses as `mcp:S/T`, and the runtime names it `mcp/S/T`, the
// same canonical id the batteries' rules use.

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

const (
	wireProtocol = 1
	wireAdapter  = "kagent"
	controlTool  = "appa:execute_remedy_plan"
)

// decision is one parsed answer from POST /hook.
type decision struct {
	Kind     string  `json:"decision"`
	Feedback string  `json:"feedback"`
	Reason   string  `json:"reason"`
	Output   string  `json:"output"`
	Value    string  `json:"value"`
	Detail   string  `json:"detail"`
	Class    string  `json:"class"`
	Offers   []offer `json:"offers"`
	Review   []struct {
		OfferID string `json:"offer_id"`
	} `json:"review"`
}

type offer struct {
	OfferID string `json:"offer_id"`
	// Returns is set on an offer that declares a child's return; the
	// processor never spawns, so it never takes one.
	Returns json.RawMessage `json:"returns,omitempty"`
}

// message is the runtime's own text a decision carries, if any.
func (d decision) message() string {
	for _, text := range []string{d.Feedback, d.Reason, d.Output, d.Detail} {
		if text != "" {
			return text
		}
	}
	return "[appa] the call was refused"
}

type runtimeClient struct {
	base   string
	client *http.Client
}

func newRuntimeClient(base string) *runtimeClient {
	return &runtimeClient{base: strings.TrimRight(base, "/"), client: &http.Client{Timeout: 30 * time.Second}}
}

func (r *runtimeClient) sessionStart(ctx context.Context, root string) (decision, error) {
	return r.post(ctx, map[string]any{"event": "session_start", "root_id": root})
}

func (r *runtimeClient) toolCall(ctx context.Context, root, callID, tool string, arguments json.RawMessage) (decision, error) {
	wire := map[string]any{"event": "tool_call", "root_id": root, "tool": tool, "arguments": arguments}
	if callID != "" {
		wire["call_id"] = callID
	}
	return r.post(ctx, wire)
}

func (r *runtimeClient) toolResult(ctx context.Context, root, callID, tool string, arguments json.RawMessage, outcome map[string]any) (decision, error) {
	wire := map[string]any{"event": "tool_result", "root_id": root, "tool": tool, "arguments": arguments, "outcome": outcome}
	if callID != "" {
		wire["call_id"] = callID
	}
	return r.post(ctx, wire)
}

// takeRemedy runs one offer the way a plugin does for its model: the control
// call names the trajectory, the runtime's MCP endpoint runs the plan, and
// the result closes the control call.
func (r *runtimeClient) takeRemedy(ctx context.Context, root, offerID string) error {
	arguments, _ := json.Marshal(map[string]string{"offer_id": offerID})
	vouch, err := r.toolCall(ctx, root, "", controlTool, arguments)
	if err != nil {
		return err
	}
	if vouch.Kind != "pass_control" {
		return fmt.Errorf("the runtime answered the remedy call with %s: %s", vouch.Kind, vouch.message())
	}
	text, failed, err := r.runPlan(ctx, offerID)
	if err != nil {
		return err
	}
	outcome := map[string]any{"status": "success", "body": text}
	if failed {
		outcome = map[string]any{"status": "failure", "message": text}
	}
	closing, err := r.toolResult(ctx, root, "", controlTool, arguments, outcome)
	if err != nil {
		return err
	}
	if failed || closing.Kind != "ack" {
		return fmt.Errorf("the remedy was not authorized: %s", text)
	}
	return nil
}

func (r *runtimeClient) runPlan(ctx context.Context, offerID string) (string, bool, error) {
	client := mcp.NewClient(&mcp.Implementation{Name: "appa-substrate-extproc", Version: "1"}, nil)
	transport := &mcp.StreamableClientTransport{Endpoint: r.base + "/mcp", HTTPClient: r.client}
	session, err := client.Connect(ctx, transport, nil)
	if err != nil {
		return "", false, fmt.Errorf("the runtime's MCP endpoint did not answer: %w", err)
	}
	defer session.Close()
	answer, err := session.CallTool(ctx, &mcp.CallToolParams{Name: "execute_remedy_plan", Arguments: map[string]any{"offer_id": offerID}})
	if err != nil {
		return "", false, fmt.Errorf("the runtime did not run the remedy plan: %w", err)
	}
	var texts []string
	for _, block := range answer.Content {
		if text, ok := block.(*mcp.TextContent); ok {
			texts = append(texts, text.Text)
		}
	}
	return strings.Join(texts, "\n"), answer.IsError, nil
}

// status is the runtime's view of one trajectory. The runtime answers it on
// loopback only, which is why the processor runs beside it.
func (r *runtimeClient) status(ctx context.Context, root string) (json.RawMessage, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, r.base+"/status?trajectory="+wireAdapter+":"+root, nil)
	if err != nil {
		return nil, err
	}
	response, err := r.client.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("status answered %d: %s", response.StatusCode, body)
	}
	return body, nil
}

// post sends one event. A transport failure, an unreadable answer, or a
// status outside 200 and 409 is an error, and the caller fails closed.
func (r *runtimeClient) post(ctx context.Context, wire map[string]any) (decision, error) {
	wire["protocol"] = wireProtocol
	wire["adapter"] = wireAdapter
	body, err := json.Marshal(wire)
	if err != nil {
		return decision{}, err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, r.base+"/hook", bytes.NewReader(body))
	if err != nil {
		return decision{}, err
	}
	request.Header.Set("Content-Type", "application/json")
	response, err := r.client.Do(request)
	if err != nil {
		return decision{}, fmt.Errorf("the OpenAPPA runtime is unreachable: %w", err)
	}
	defer response.Body.Close()
	answer, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return decision{}, err
	}
	if response.StatusCode != http.StatusOK && response.StatusCode != http.StatusConflict {
		return decision{}, fmt.Errorf("the OpenAPPA runtime answered %d: %s", response.StatusCode, answer)
	}
	var parsed decision
	if err := json.Unmarshal(answer, &parsed); err != nil || parsed.Kind == "" {
		return decision{}, fmt.Errorf("unreadable decision from the OpenAPPA runtime: %s", answer)
	}
	if (parsed.Kind == "refuse") != (response.StatusCode == http.StatusConflict) {
		return decision{}, fmt.Errorf("a %d answer carrying a %s decision", response.StatusCode, parsed.Kind)
	}
	return parsed, nil
}
