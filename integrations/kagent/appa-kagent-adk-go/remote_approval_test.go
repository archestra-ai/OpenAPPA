package appakagentadk

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/a2aproject/a2a-go/a2asrv"
	ka2a "github.com/kagent-dev/kagent/go/adk/pkg/a2a"
	"github.com/kagent-dev/kagent/go/api/adk"
	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/tool/toolconfirmation"
)

type remoteApprovalContext struct{ *fakeContext }

func (c remoteApprovalContext) UserID() string    { return "test-user" }
func (c remoteApprovalContext) SessionID() string { return "parent-context" }

func TestRemoteRejectionRequiresThePausedTaskIdentityBeforeNetworking(t *testing.T) {
	for _, payload := range []map[string]any{nil, {"task_id": "task"}, {"context_id": "child"}} {
		remote, err := newRemoteApprovalTool(adk.RemoteAgentConfig{Name: "child", Url: "http://127.0.0.1:1"}, false)
		if err != nil {
			t.Fatal(err)
		}
		ctx := remoteApprovalContext{newFakeContext(newFakeSession("parent"))}
		ctx.confirmation = &toolconfirmation.ToolConfirmation{Confirmed: false, Payload: payload}
		if _, err := remote.Run(ctx, map[string]any{"request": "restart"}); err == nil || err.Error() != "remote rejection requires the paused task and context" {
			t.Fatalf("missing identity reached transport: %v", err)
		}
	}
}

func TestRemoteRejectionPreservesNativeDecisionPayloads(t *testing.T) {
	reject := remoteRejectionDecision(ka2a.HitlConfirmationPayload{RejectionReason: "do not restart"})
	if reject["decision_type"] != "reject" || reject["rejection_reason"] != "do not restart" {
		t.Fatalf("rejection changed: %v", reject)
	}
	batch := remoteRejectionDecision(ka2a.HitlConfirmationPayload{
		BatchDecisions:   map[string]ka2a.DecisionType{"first": "approve", "second": "reject"},
		RejectionReasons: map[string]string{"second": "leave it running"},
	})
	if batch["decision_type"] != "batch" || batch["decisions"].(map[string]any)["second"] != "reject" {
		t.Fatalf("batch rulings changed: %v", batch)
	}
}

func TestRemoteRejectionReachesTheOriginalChild(t *testing.T) {
	var endpoint string
	var sent map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.Method == http.MethodGet {
			json.NewEncoder(w).Encode(map[string]any{
				"protocolVersion": "0.3.0", "name": "child", "description": "test child", "version": "1",
				"url": endpoint, "preferredTransport": "JSONRPC", "capabilities": map[string]any{}, "defaultInputModes": []string{"text"},
				"defaultOutputModes": []string{"text"}, "skills": []any{},
			})
			return
		}
		if err := json.NewDecoder(r.Body).Decode(&sent); err != nil {
			t.Error(err)
		}
		if r.Header.Get("x-kagent-parent-context-id") != "parent-context" || r.Header.Get("x-kagent-root-context-id") != "parent-context" {
			t.Error("rejection lost parent lineage")
		}
		if r.Header.Get("x-user-id") != "test-user" || r.Header.Get("X-Test-Header") != "configured" {
			t.Error("rejection lost caller/configured headers")
		}
		json.NewEncoder(w).Encode(map[string]any{"jsonrpc": "2.0", "id": sent["id"], "result": map[string]any{
			"kind": "task", "id": "child-task", "contextId": "child-context", "status": map[string]any{"state": "completed"},
			"artifacts": []any{map[string]any{"artifactId": "reply", "parts": []any{map[string]any{"kind": "text", "text": "The operator rejected the restart."}}}},
		}})
	}))
	defer server.Close()
	endpoint = server.URL
	remote, err := newRemoteApprovalTool(adk.RemoteAgentConfig{Name: "kagent__NS__child", Url: endpoint, Headers: map[string]string{"X-Test-Header": "configured"}}, false)
	if err != nil {
		t.Fatal(err)
	}
	ctx := remoteApprovalContext{newFakeContext(newFakeSession("parent-context"))}
	ctx.confirmation = &toolconfirmation.ToolConfirmation{Confirmed: false, Payload: map[string]any{
		"task_id": "child-task", "context_id": "child-context", "subagent_name": remote.Name(),
	}}
	request := &model.LLMRequest{}
	if err := remote.ProcessRequest(ctx, request); err != nil {
		t.Fatal(err)
	}
	if request.Tools[remote.Name()] != remote {
		t.Fatal("ADK would dispatch the unwrapped stock tool")
	}
	result, err := remote.Run(ctx, map[string]any{"request": "restart"})
	if err != nil {
		t.Fatal(err)
	}
	if result["subagent_session_id"] != "child-context" || result["result"] != "The operator rejected the restart." {
		t.Fatalf("wrong child result: %v", result)
	}
	message := sent["params"].(map[string]any)["message"].(map[string]any)
	if message["taskId"] != "child-task" || message["contextId"] != "child-context" {
		t.Fatalf("rejection changed identity: %v", message)
	}
	data := message["parts"].([]any)[0].(map[string]any)["data"].(map[string]any)
	if data["decision_type"] != "reject" {
		t.Fatalf("native rejection was not forwarded: %v", data)
	}
}

func TestRemoteRejectionResponseAndAuthenticationBoundaries(t *testing.T) {
	for _, scenario := range []struct {
		name, taskID, contextID, state, expectedError string
		propagate, configuredAuth                     bool
	}{
		{name: "wrong task", taskID: "other", expectedError: "different task"},
		{name: "wrong child", contextID: "other", expectedError: "different task"},
		{name: "failed child", state: "failed", expectedError: "did not complete"},
		{name: "paused again", state: "input-required"},
		{name: "auth disabled"},
		{name: "forward auth", propagate: true},
		{name: "configured auth wins", propagate: true, configuredAuth: true},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			var endpoint string
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				if r.Method == http.MethodGet {
					json.NewEncoder(w).Encode(map[string]any{"protocolVersion": "0.3.0", "name": "child", "description": "child", "version": "1",
						"url": endpoint, "preferredTransport": "JSONRPC", "capabilities": map[string]any{}, "skills": []any{},
						"defaultInputModes": []string{"text"}, "defaultOutputModes": []string{"text"}})
					return
				}
				wantAuth := ""
				if scenario.propagate {
					wantAuth = "Bearer test-inbound"
				}
				if scenario.configuredAuth {
					wantAuth = "Bearer test-configured"
				}
				if r.Header.Get("Authorization") != wantAuth || r.Header.Get("x-kagent-root-context-id") != "upstream-root" {
					t.Error("rejection changed authentication or root lineage")
				}
				var call map[string]any
				if err := json.NewDecoder(r.Body).Decode(&call); err != nil {
					t.Error(err)
				}
				taskID, contextID, state := scenario.taskID, scenario.contextID, scenario.state
				if taskID == "" {
					taskID = "child-task"
				}
				if contextID == "" {
					contextID = "child-context"
				}
				if state == "" {
					state = "completed"
				}
				json.NewEncoder(w).Encode(map[string]any{"jsonrpc": "2.0", "id": call["id"], "result": map[string]any{
					"kind": "task", "id": taskID, "contextId": contextID,
					"status": map[string]any{"state": state, "message": map[string]any{"kind": "message", "messageId": "reply", "role": "agent",
						"parts": []any{map[string]any{"kind": "text", "text": "Rejected; no restart."}}}},
					"metadata": map[string]any{"kagent_usage_metadata": map[string]any{"totalTokenCount": 7}},
				}})
			}))
			defer server.Close()
			endpoint = server.URL
			headers := map[string]string{}
			if scenario.configuredAuth {
				headers["authorization"] = "Bearer test-configured"
			}
			remote, err := newRemoteApprovalTool(adk.RemoteAgentConfig{Name: "child", Url: endpoint, Headers: headers}, scenario.propagate)
			if err != nil {
				t.Fatal(err)
			}
			ctx := remoteApprovalContext{newFakeContext(newFakeSession("parent-context"))}
			inbound, _ := a2asrv.WithCallContext(context.Background(), a2asrv.NewRequestMeta(map[string][]string{
				"authorization": {"Bearer test-inbound"}, "x-kagent-root-context-id": {"upstream-root"},
			}))
			ctx.StrictContextMock = agent.NewStrictContextMock(inbound)
			ctx.confirmation = &toolconfirmation.ToolConfirmation{Confirmed: false, Payload: map[string]any{"task_id": "child-task", "context_id": "child-context"}}
			result, err := remote.Run(ctx, map[string]any{"request": "restart"})
			if scenario.expectedError != "" {
				if err == nil || !strings.Contains(err.Error(), scenario.expectedError) || result != nil {
					t.Fatalf("invalid response admitted: %v, %v", result, err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if scenario.state == "input-required" {
				if result["status"] != "pending" || len(ctx.requested) != 1 {
					t.Fatalf("child pause lost: %v", result)
				}
				payload := ctx.requested[0].payload.(map[string]any)
				if payload["context_id"] != "child-context" || payload["task_id"] != "child-task" {
					t.Fatalf("pause identity changed: %v", payload)
				}
			} else if result["result"] != "Rejected; no restart." || result["kagent_usage_metadata"] == nil {
				t.Fatalf("status-message fallback or usage lost: %v", result)
			}
		})
	}
}
