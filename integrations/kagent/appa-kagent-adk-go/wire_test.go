// The wire against the shared fixtures and the decision contract.
//
// The python plugin's tests read the same fixture file, so the two
// plugins cannot drift apart silently: an event this side builds is
// the event that side builds, and both are the canonical envelope the
// runtime parses (appa-runtime-api/src/wire.rs).

package appakagentadk

import (
	"encoding/json"
	"errors"
	"os"
	"reflect"
	"strings"
	"testing"
)

const fixturesPath = "../fixtures/wire-events.jsonl"

const (
	fixtureRoot  = "adk-4f6c2f1e"
	fixtureChild = "adk-9b0d11aa"
)

func fixtureWires(t *testing.T) map[string]any {
	t.Helper()
	raw, err := os.ReadFile(fixturesPath)
	if err != nil {
		t.Fatalf("the shared fixture file must be readable: %v", err)
	}
	wires := map[string]any{}
	for _, line := range strings.Split(string(raw), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		var fixture struct {
			Name string `json:"name"`
			Wire any    `json:"wire"`
		}
		if err := json.Unmarshal([]byte(line), &fixture); err != nil {
			t.Fatalf("the fixture line must parse: %v", err)
		}
		wires[fixture.Name] = fixture.Wire
	}
	return wires
}

// canonical renders a wire event as sorted-key JSON, the byte form
// both sides of the comparison normalize to.
func canonical(t *testing.T, wire any) string {
	t.Helper()
	rendered, err := json.Marshal(wire)
	if err != nil {
		t.Fatalf("the wire event must serialize: %v", err)
	}
	var parsed any
	if err := json.Unmarshal(rendered, &parsed); err != nil {
		t.Fatalf("the wire event must round-trip: %v", err)
	}
	normalized, err := json.Marshal(parsed)
	if err != nil {
		t.Fatalf("the wire event must re-serialize: %v", err)
	}
	return string(normalized)
}

func TestEveryBuilderSpellsItsFixtureExactly(t *testing.T) {
	fixtures := fixtureWires(t)
	scale := map[string]any{"deployment": "api", "replicas": 3}
	built := map[string]map[string]any{
		"session_start":  sessionStartEvent(fixtureRoot),
		"prompt":         promptEvent(fixtureRoot, "scale the api deployment to three replicas", ""),
		"turn_end_root":  turnEndEvent(fixtureRoot, ""),
		"turn_end_child": turnEndEvent(fixtureRoot, fixtureChild),
		"tool_call_mcp":  toolCallEvent(fixtureRoot, "mcp:demo-tools/k8s_scale", scale, "", ""),
		"tool_call_agent": toolCallEvent(
			fixtureRoot, "agent:kagent/billing-agent", map[string]any{"message": "total the invoices"}, "", ""),
		"tool_call_builtin": toolCallEvent(
			fixtureRoot, "builtin:ask_user", map[string]any{"questions": []any{map[string]any{"question": "which namespace?"}}}, "", ""),
		"tool_call_gate":    toolCallEvent(fixtureRoot, "gate:code_execution", map[string]any{"code": "print(1)"}, "", ""),
		"tool_call_control": toolCallEvent(fixtureRoot, ControlTool, map[string]any{"offer_id": "offer-1"}, "", ""),
		"tool_call_control_ruled": toolCallEvent(
			fixtureRoot, ControlTool, map[string]any{"offer_id": "offer-1"}, "", "approve"),
		"tool_result_success": toolResultEvent(
			fixtureRoot, "mcp:demo-tools/k8s_scale", scale, successOutcome(map[string]any{"scaled": true}), ""),
		"tool_result_failure": toolResultEvent(
			fixtureRoot, "mcp:demo-tools/k8s_scale", scale, failureOutcome("connection refused"), ""),
		"spawn_result": spawnResultEvent(
			fixtureRoot, "agent:kagent/billing-agent", map[string]any{"message": "total the invoices"},
			successOutcome(map[string]any{"result": "the total is 42"}),
			fixtureChild, "the total is 42", ""),
		"child_start_in_flight": childStartEvent(fixtureRoot, fixtureChild, ""),
		"child_start_bound":     childStartEvent(fixtureRoot, fixtureChild, "spawn-1"),
		"child_end_returned":    childEndEvent(fixtureRoot, fixtureChild, "the total is 42"),
		"child_end_void":        childEndEvent(fixtureRoot, fixtureChild, ""),
		"ping":                  pingEvent(),
	}
	if len(built) != len(fixtures) {
		t.Fatalf("the fixture file and this test must cover the same wire kinds: %d built, %d fixtures", len(built), len(fixtures))
	}
	for name, expected := range fixtures {
		builtWire, covered := built[name]
		if !covered {
			t.Fatalf("the %s fixture has no builder in this test", name)
		}
		if got, want := canonical(t, builtWire), canonical(t, expected); got != want {
			t.Errorf("the %s wire drifted from the shared fixture:\n built %s\n fixture %s", name, got, want)
		}
	}
}

func TestEveryEventCarriesTheEnvelopeAndNoSpawnFlag(t *testing.T) {
	for name, wire := range fixtureWires(t) {
		event := wire.(map[string]any)
		if event["protocol"] != float64(Protocol) || event["adapter"] != Adapter {
			t.Errorf("the %s event must carry the envelope, got %v", name, event)
		}
		if _, present := event["spawn"]; present {
			t.Errorf("the %s event must assert no spawn, got %v", name, event)
		}
	}
}

func TestIDsCrossUnprefixed(t *testing.T) {
	wire := toolCallEvent("session-1", "mcp:demo-tools/k8s_scale", map[string]any{}, "child-1", "")
	if wire["root_id"] != "session-1" || wire["child_id"] != "child-1" {
		t.Errorf("the ids cross as the harness spells them, got %v", wire)
	}
}

func TestAbsentOptionalsStayOffTheWire(t *testing.T) {
	wire := spawnResultEvent("s1", "t", map[string]any{}, indeterminateOutcome(), "", "", "")
	for _, absent := range []string{"spawned_id", "value", "child_id"} {
		if _, present := wire[absent]; present {
			t.Errorf("an absent %s must stay off the wire, got %v", absent, wire[absent])
		}
	}
}

func TestEveryDecisionEnvelopeParses(t *testing.T) {
	for _, tc := range []struct {
		name string
		body string
		want Decision
	}{
		{"ack", `{"protocol": 1, "decision": "ack"}`, Decision{Kind: "ack"}},
		{"allow", `{"protocol": 1, "decision": "allow_call"}`, Decision{Kind: "allow_call"}},
		{
			"allow_bound",
			`{"protocol": 1, "decision": "allow_call", "spawn_binding": "b1"}`,
			Decision{Kind: "allow_call", SpawnBinding: "b1"},
		},
		{"pass", `{"protocol": 1, "decision": "pass_control"}`, Decision{Kind: "pass_control"}},
		{
			"deny",
			`{"protocol": 1, "decision": "deny_call", "feedback": "blocked: the recipient cannot read this"}`,
			Decision{Kind: "deny_call", Feedback: "blocked: the recipient cannot read this"},
		},
		{
			"block",
			`{"protocol": 1, "decision": "block", "reason": "the prompt does not cross"}`,
			Decision{Kind: "block", Reason: "the prompt does not cross"},
		},
		{
			"replace",
			`{"protocol": 1, "decision": "replace_output", "output": "the output is confined"}`,
			Decision{Kind: "replace_output", Output: "the output is confined"},
		},
		{
			"child_return",
			`{"protocol": 1, "decision": "child_return", "value": "the redacted summary"}`,
			Decision{Kind: "child_return", Value: "the redacted summary"},
		},
		{
			"context",
			`{"protocol": 1, "decision": "context", "text": "your return crosses as you speak it"}`,
			Decision{Kind: "context", Text: "your return crosses as you speak it"},
		},
		{
			"refuse",
			`{"protocol": 1, "decision": "refuse", "detail": "storage failure: disk full"}`,
			Decision{Kind: "refuse", Detail: "storage failure: disk full"},
		},
	} {
		got, err := parseDecision([]byte(tc.body))
		if err != nil {
			t.Errorf("the %s envelope must parse, got %v", tc.name, err)
			continue
		}
		if !reflect.DeepEqual(got, tc.want) {
			t.Errorf("the %s envelope drifted: got %+v, want %+v", tc.name, got, tc.want)
		}
	}
}

func TestAnAnswerOutsideTheContractIsAWireError(t *testing.T) {
	for _, tc := range []struct {
		name string
		body string
	}{
		{"not_json", "not json"},
		{"not_object", `["decision"]`},
		{"null", `null`},
		{"no_protocol", `{"decision": "ack"}`},
		{"other_protocol", `{"protocol": 2, "decision": "ack"}`},
		{"protocol_as_bool", `{"protocol": true, "decision": "ack"}`},
		{"protocol_as_string", `{"protocol": "1", "decision": "ack"}`},
		{"unknown_kind", `{"protocol": 1, "decision": "approve"}`},
		{"deny_without_feedback", `{"protocol": 1, "decision": "deny_call"}`},
		{"block_without_reason", `{"protocol": 1, "decision": "block"}`},
		{"replace_without_output", `{"protocol": 1, "decision": "replace_output"}`},
		{"refuse_without_detail", `{"protocol": 1, "decision": "refuse"}`},
		{"context_without_text", `{"protocol": 1, "decision": "context"}`},
		{"offers_not_a_list", `{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": {}}`},
		{"offer_without_an_id", `{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": [{"returns": "as_spoken"}]}`},
		{
			"offer_route_outside_the_wire",
			`{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": [{"offer_id": "o1", "returns": "shaped"}]}`,
		},
		{
			"sanitized_route_without_a_sanitizer",
			`{"protocol": 1, "decision": "deny_call", "feedback": "f", "offers": [{"offer_id": "o1", "returns": {}}]}`,
		},
		{"binding_not_a_string", `{"protocol": 1, "decision": "allow_call", "spawn_binding": 7}`},
	} {
		if _, err := parseDecision([]byte(tc.body)); err == nil {
			t.Errorf("the %s answer must be a wire error", tc.name)
		} else {
			var wireErr *WireError
			if !errors.As(err, &wireErr) {
				t.Errorf("the %s answer must fail as a WireError, got %T", tc.name, err)
			}
		}
	}
}

func TestADenyCarriesItsReviewsAndRefusesAnUnreadableOne(t *testing.T) {
	decision, err := parseDecision([]byte(`{"protocol":1,"decision":"deny_call","feedback":"f","review":[{"offer_id":"o1","text":"APPA asks you"}]}`))
	if err != nil || len(decision.Review) != 1 || decision.Review[0] != (Review{OfferID: "o1", Text: "APPA asks you"}) {
		t.Fatalf("the review rides the deny: %+v %v", decision, err)
	}
	if plain, err := parseDecision([]byte(`{"protocol":1,"decision":"deny_call","feedback":"f"}`)); err != nil || plain.Review != nil {
		t.Fatalf("no review is none: %+v %v", plain, err)
	}
	if _, err := parseDecision([]byte(`{"protocol":1,"decision":"deny_call","feedback":"f","review":[{"offer_id":"o1"}]}`)); err == nil {
		t.Fatal("a review entry without its text is malformed")
	}
}

func TestADenyCarriesEveryOfferWithItsReturnRoute(t *testing.T) {
	decision, err := parseDecision([]byte(`{"protocol":1,"decision":"deny_call","feedback":"blocked","offers":[` +
		`{"offer_id":"o1"},` +
		`{"offer_id":"o2","returns":"as_spoken"},` +
		`{"offer_id":"o3","returns":{"sanitizer":"redact-invoices"}}]}`))
	if err != nil {
		t.Fatalf("the offers must parse: %v", err)
	}
	want := []Offer{
		{OfferID: "o1"},
		{OfferID: "o2", Returns: ReturnAsSpoken},
		{OfferID: "o3", Returns: ReturnSanitized, Sanitizer: "redact-invoices"},
	}
	if !reflect.DeepEqual(decision.Offers, want) {
		t.Errorf("the offers drifted: got %+v, want %+v", decision.Offers, want)
	}
	plain, err := parseDecision([]byte(`{"protocol":1,"decision":"deny_call","feedback":"f"}`))
	if err != nil || plain.Offers != nil {
		t.Fatalf("no offers is none: %+v %v", plain, err)
	}
}

func TestAControlCallCarriesItsRulingOnlyWhenGiven(t *testing.T) {
	ruled := toolCallEvent("s1", ControlTool, map[string]any{"offer_id": "o1"}, "", "approve")
	if ruled["ruling"] != "approve" {
		t.Errorf("the ruling rides the control call, got %v", ruled)
	}
	if _, present := toolCallEvent("s1", ControlTool, map[string]any{"offer_id": "o1"}, "", "")["ruling"]; present {
		t.Error("an absent ruling must stay off the wire")
	}
}

func TestAChildEndWithoutAValueKeepsItOffTheWire(t *testing.T) {
	if _, present := childEndEvent("s1", "c1", "")["value"]; present {
		t.Error("an absent value must stay off the wire")
	}
	if value := childEndEvent("s1", "c1", "the total is 42")["value"]; value != "the total is 42" {
		t.Errorf("the child's value rides the event, got %v", value)
	}
}
