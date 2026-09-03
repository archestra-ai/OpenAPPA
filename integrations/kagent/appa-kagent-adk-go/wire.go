// The adapter wire: event construction and decision parsing.
//
// One JSON object per callback crosses POST $APPA_RUNTIME_URL/hook.
// The appa-adapter-kagent codec in the runtime parses these events and
// renders every answer as one decision envelope. This file owns both
// shapes on the go side and imports no ADK code, so the wire stays
// testable against the shared fixtures
// (integrations/kagent/fixtures/wire-events.jsonl) without an agent
// runtime.
//
// Ids are the harness's own: root_id is the ADK session id of the
// root trajectory (in a delegated child workload, the root id read
// from the inbound call metadata), and child_id is the delegated
// child scope's own id. The codec applies the `kagent:` prefix; this
// file never does.

package appakagentadk

import (
	"encoding/json"
	"fmt"
)

// An empty childID means the emitting scope is the root itself: the
// field stays off the wire, exactly as the python builders omit a None
// child_id.
func event(kind, rootID, childID string) map[string]any {
	wire := map[string]any{"event": kind, "root_id": rootID}
	if childID != "" {
		wire["child_id"] = childID
	}
	return wire
}

// pingEvent is the liveness probe: parses to no event, answers 200 {}.
func pingEvent() map[string]any {
	return map[string]any{"event": "ping"}
}

func sessionStartEvent(rootID string) map[string]any {
	return map[string]any{"event": "session_start", "root_id": rootID}
}

func promptEvent(rootID, text, childID string) map[string]any {
	wire := event("prompt", rootID, childID)
	wire["text"] = text
	return wire
}

func turnEndEvent(rootID, childID string) map[string]any {
	return event("turn_end", rootID, childID)
}

// toolCallEvent is a proposed call. ruling (approve or deny) rides only
// the control call whose offer a person ruled on through kagent's own
// confirmation; the runtime spends it as the human authority's answer.
func toolCallEvent(rootID, tool string, arguments any, spawn bool, childID string, ruling string) map[string]any {
	wire := event("tool_call", rootID, childID)
	wire["tool"] = tool
	wire["arguments"] = arguments
	wire["spawn"] = spawn
	if ruling != "" {
		wire["ruling"] = ruling
	}
	return wire
}

func toolResultEvent(rootID, tool string, arguments, outcome any, childID string) map[string]any {
	wire := event("tool_result", rootID, childID)
	wire["tool"] = tool
	wire["arguments"] = arguments
	wire["outcome"] = outcome
	return wire
}

func spawnResultEvent(rootID, tool string, arguments, outcome any, spawnedID, value, childID string) map[string]any {
	wire := event("spawn_result", rootID, childID)
	wire["tool"] = tool
	wire["arguments"] = arguments
	wire["outcome"] = outcome
	if spawnedID != "" {
		wire["spawned_id"] = spawnedID
	}
	if value != "" {
		wire["value"] = value
	}
	return wire
}

func childStartEvent(rootID, childID, spawnBinding string) map[string]any {
	wire := map[string]any{"event": "child_start", "root_id": rootID, "child_id": childID}
	if spawnBinding != "" {
		wire["spawn_binding"] = spawnBinding
	}
	return wire
}

// successOutcome carries the tool response as spelled.
func successOutcome(body any) map[string]any {
	return map[string]any{"status": "success", "body": body}
}

func failureOutcome(message string) map[string]any {
	return map[string]any{"status": "failure", "message": message}
}

func indeterminateOutcome() map[string]any {
	return map[string]any{"status": "indeterminate"}
}

// WireError reports that the runtime's answer left the decision
// contract. The caller fails closed, exactly like a transport failure.
type WireError struct {
	Detail string
}

func (e *WireError) Error() string {
	return e.Detail
}

func wireErrorf(format string, args ...any) *WireError {
	return &WireError{Detail: fmt.Sprintf(format, args...)}
}

// Decision is one parsed decision envelope.
//
// Kind is the wire spelling (ack, allow_call, pass_control, deny_call,
// block, replace_output, child_return, refuse); the payload field,
// where the kind carries one, lands in the matching attribute.
type Decision struct {
	Kind         string
	Feedback     string
	Reason       string
	Output       string
	Value        string
	Detail       string
	SpawnBinding string
	// Review rides a deny_call: the offers whose plans consult a human
	// authority, with the review as the person reads it.
	Review []Review
}

// Review is one reviewed offer: the id the control call quotes, and the
// text the plugin shows through kagent's confirmation.
type Review struct {
	OfferID string
	Text    string
}

// describe names the decision in a fail-closed message: the runtime's
// detail when it carries one, the kind otherwise.
func (d Decision) describe() string {
	if d.Detail != "" {
		return d.Detail
	}
	return d.Kind
}

// decisionPayloads maps each decision kind to the payload field it
// must carry. A kind outside this map is outside the wire.
var decisionPayloads = map[string]string{
	"ack":            "",
	"allow_call":     "",
	"pass_control":   "",
	"deny_call":      "feedback",
	"block":          "reason",
	"replace_output": "output",
	"child_return":   "value",
	"refuse":         "detail",
}

// parseDecision parses one decision envelope; anything else is a
// *WireError. The plugin enforces decisions mechanically, so an answer
// outside the contract must block the gated action rather than pass it.
func parseDecision(body []byte) (Decision, error) {
	var parsed map[string]any
	if err := json.Unmarshal(body, &parsed); err != nil {
		return Decision{}, wireErrorf("unreadable decision envelope: %v", err)
	}
	if parsed == nil {
		return Decision{}, wireErrorf("the decision envelope is not an object")
	}
	kind, ok := parsed["decision"].(string)
	if !ok {
		return Decision{}, wireErrorf("a decision kind outside the wire: %v", parsed["decision"])
	}
	payload, known := decisionPayloads[kind]
	if !known {
		return Decision{}, wireErrorf("a decision kind outside the wire: %q", kind)
	}
	decision := Decision{Kind: kind}
	if payload != "" {
		value, ok := parsed[payload].(string)
		if !ok {
			return Decision{}, wireErrorf("a %s decision without its %s", kind, payload)
		}
		switch payload {
		case "feedback":
			decision.Feedback = value
		case "reason":
			decision.Reason = value
		case "output":
			decision.Output = value
		case "value":
			decision.Value = value
		case "detail":
			decision.Detail = value
		}
	}
	if kind == "deny_call" {
		review, err := parseReview(parsed["review"])
		if err != nil {
			return Decision{}, err
		}
		decision.Review = review
	}
	if kind == "allow_call" {
		if binding, present := parsed["spawn_binding"]; present {
			bound, ok := binding.(string)
			if !ok {
				return Decision{}, wireErrorf("a spawn binding that is not a string")
			}
			decision.SpawnBinding = bound
		}
	}
	return decision, nil
}

// parseReview reads a deny_call's review entries; absent is none, and
// any other shape is a *WireError — a review the plugin cannot read
// would leave a person unasked, which is fail-open.
func parseReview(raw any) ([]Review, error) {
	if raw == nil {
		return nil, nil
	}
	entries, ok := raw.([]any)
	if !ok {
		return nil, wireErrorf("a deny_call review that is not a list")
	}
	review := make([]Review, 0, len(entries))
	for _, entry := range entries {
		fields, ok := entry.(map[string]any)
		offer, okOffer := fields["offer_id"].(string)
		text, okText := fields["text"].(string)
		if !ok || !okOffer || !okText {
			return nil, wireErrorf("a deny_call review entry without its offer_id and text")
		}
		review = append(review, Review{OfferID: offer, Text: text})
	}
	return review, nil
}
