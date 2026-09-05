// The hook wire: event construction and decision parsing.
//
// One JSON object per callback crosses POST $APPA_RUNTIME_URL/hook, in
// the canonical envelope every adapter shares
// (appa-runtime-api/src/wire.rs): protocol is the wire version and
// adapter names this plugin's adapter, and the runtime refuses an event
// that carries another pair. This file owns the event shape and the
// decision envelope on the go side and imports no ADK code, so the wire
// stays testable against the shared fixtures
// (marketplace/adapters/kagent/fixtures/wire-events.jsonl) without an agent
// runtime.
//
// Ids are the harness's own: root_id is the ADK session id of the
// root trajectory (in a delegated child workload, the root id read
// from the inbound call metadata), and child_id is the delegated
// child scope's own id. The runtime applies the `kagent:` prefix; this
// file never does.
//
// A tool crosses under its structured spelling, never its bare ADK
// name (inventory.go). The runtime derives the canonical tool and
// whether the call is a spawn from that spelling; the wire asserts
// neither.

package appakagentadk

import (
	"encoding/json"
	"fmt"
)

// Protocol is the hook wire version this plugin speaks.
const Protocol = 1

// Adapter is the adapter name every event carries.
const Adapter = "kagent"

func envelope(kind string) map[string]any {
	return map[string]any{"protocol": Protocol, "adapter": Adapter, "event": kind}
}

// An empty childID means the emitting scope is the root itself: the
// field stays off the wire, exactly as the python builders omit a None
// child_id.
func event(kind, rootID, childID string) map[string]any {
	wire := envelope(kind)
	wire["root_id"] = rootID
	if childID != "" {
		wire["child_id"] = childID
	}
	return wire
}

// pingEvent is the liveness probe: parses to no event, answers 200 {}.
func pingEvent() map[string]any {
	return envelope("ping")
}

func sessionStartEvent(rootID string) map[string]any {
	return event("session_start", rootID, "")
}

func promptEvent(rootID, text, childID string) map[string]any {
	wire := event("prompt", rootID, childID)
	wire["text"] = text
	return wire
}

func turnEndEvent(rootID, childID string) map[string]any {
	return event("turn_end", rootID, childID)
}

// toolCallEvent is a proposed call of tool, under its structured
// spelling. ruling (approve or deny) rides only the control call whose
// offer a person ruled on through kagent's own confirmation; the
// runtime spends it as the human authority's answer.
func toolCallEvent(rootID, tool string, arguments any, childID string, ruling string) map[string]any {
	wire := event("tool_call", rootID, childID)
	wire["tool"] = tool
	wire["arguments"] = arguments
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
	wire := event("child_start", rootID, childID)
	if spawnBinding != "" {
		wire["spawn_binding"] = spawnBinding
	}
	return wire
}

// childEndEvent is the child's stop, carrying the value it returns to
// its parent. An empty value is a child that returns nothing, and the
// runtime reads an absent one the same way.
func childEndEvent(rootID, childID, value string) map[string]any {
	wire := event("child_end", rootID, childID)
	if value != "" {
		wire["value"] = value
	}
	return wire
}

// successOutcome carries the tool response as spelled. The body is
// exactly the body field, nil (JSON null) included; a success that
// carries no body at all is successWithoutBodyOutcome, never this with
// the field left off.
func successOutcome(body any) map[string]any {
	return map[string]any{"status": "success", "body": body}
}

// successWithoutBodyOutcome is a success whose body the wire does not
// carry. Distinct from a body that is null: the tool succeeded and the
// runtime holds no value from it.
func successWithoutBodyOutcome() map[string]any {
	return map[string]any{"status": "success_without_body"}
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
// block, replace_output, deliver_value, child_return, context, refuse);
// the payload field, where the kind carries one, lands in the matching
// attribute.
//
// A decision that stands in for a result says which of two contents it
// carries. deliver_value and child_return carry a Value the engine
// admitted, and the plugin delivers those bytes as they crossed.
// replace_output, deny_call, block and refuse carry the runtime's own
// words, which name tools by the spelling the plugin sent, so the
// plugin spells them back before the model reads them.
type Decision struct {
	Kind     string
	Feedback string
	Reason   string
	// Output rides a replace_output: the runtime's own words in place of
	// the result, which the plugin spells back into names the model
	// dispatches.
	Output string
	// Value rides a deliver_value and a child_return: the value the
	// engine admitted, which reaches the model as it crossed.
	Value string
	// Text rides a context: what the harness hands the actor the event
	// names, which at a child's start is the return contract it works
	// under.
	Text         string
	Detail       string
	SpawnBinding string
	// Review rides a deny_call: the offers whose plans consult a human
	// authority, with the review as the person reads it.
	Review []Review
	// Offers rides a deny_call: every remedy the block offers, in the
	// order the feedback lists them.
	Offers []Offer
}

// Review is one reviewed offer: the id the control call quotes, and the
// text the plugin shows through kagent's confirmation.
type Review struct {
	OfferID string
	Text    string
}

// The return routes an offer declares: ReturnAsSpoken crosses the child's
// return as the child spoke it, and ReturnSanitized crosses what the
// named sanitizer derives.
const (
	ReturnAsSpoken  = "as_spoken"
	ReturnSanitized = "sanitized"
)

// Offer is one remedy a deny_call offers, for the plugin that routes one
// itself rather than through the model's control call. OfferID is the id
// the control call quotes. Returns is empty on an offer that declares no
// child return, and Sanitizer names the sanitizer on the ReturnSanitized
// route only.
type Offer struct {
	OfferID   string
	Returns   string
	Sanitizer string
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
	"deliver_value":  "value",
	"child_return":   "value",
	"context":        "text",
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
	// The version is an integer on the wire. Into map[string]any every
	// JSON number decodes as float64, so 1.0 and 1 are one value there;
	// a typed decode of the field alone is what tells them apart, and it
	// refuses a bool, a string and a fraction alike.
	var envelope struct {
		Protocol *int `json:"protocol"`
	}
	if err := json.Unmarshal(body, &envelope); err != nil || envelope.Protocol == nil || *envelope.Protocol != Protocol {
		return Decision{}, wireErrorf("a decision under a protocol outside the wire: %v", parsed["protocol"])
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
		case "text":
			decision.Text = value
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
		offers, err := parseOffers(parsed["offers"])
		if err != nil {
			return Decision{}, err
		}
		decision.Offers = offers
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

// parseOffers reads a deny_call's offers; absent is none, and any other
// shape is a *WireError — an offer the plugin cannot read would leave a
// route unrouted, which is fail-open.
func parseOffers(raw any) ([]Offer, error) {
	if raw == nil {
		return nil, nil
	}
	entries, ok := raw.([]any)
	if !ok {
		return nil, wireErrorf("a deny_call offers list that is not a list")
	}
	offers := make([]Offer, 0, len(entries))
	for _, entry := range entries {
		fields, ok := entry.(map[string]any)
		id, okID := fields["offer_id"].(string)
		if !ok || !okID {
			return nil, wireErrorf("a deny_call offer without its offer_id")
		}
		offer, err := parseOfferReturn(id, fields["returns"])
		if err != nil {
			return nil, err
		}
		offers = append(offers, offer)
	}
	return offers, nil
}

// parseOfferReturn reads one offer's return route. An absent route is an
// offer that declares no child return.
func parseOfferReturn(id string, raw any) (Offer, error) {
	switch route := raw.(type) {
	case nil:
		return Offer{OfferID: id}, nil
	case string:
		if route != ReturnAsSpoken {
			return Offer{}, wireErrorf("a deny_call offer with a return route outside the wire: %q", route)
		}
		return Offer{OfferID: id, Returns: ReturnAsSpoken}, nil
	case map[string]any:
		sanitizer, ok := route["sanitizer"].(string)
		if !ok {
			return Offer{}, wireErrorf("a deny_call offer with a sanitized return route that names no sanitizer")
		}
		return Offer{OfferID: id, Returns: ReturnSanitized, Sanitizer: sanitizer}, nil
	default:
		return Offer{}, wireErrorf("a deny_call offer with a return route outside the wire: %v", raw)
	}
}
