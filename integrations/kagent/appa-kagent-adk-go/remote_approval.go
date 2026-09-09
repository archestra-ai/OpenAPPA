package appakagentadk

import (
	"fmt"
	"net/http"
	"strings"

	a2atype "github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2aclient"
	"github.com/a2aproject/a2a-go/a2aclient/agentcard"
	"github.com/a2aproject/a2a-go/a2asrv"
	ka2a "github.com/kagent-dev/kagent/go/adk/pkg/a2a"
	"github.com/kagent-dev/kagent/go/adk/pkg/constants"
	"github.com/kagent-dev/kagent/go/adk/pkg/tools"
	"github.com/kagent-dev/kagent/go/api/adk"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/toolutils"
	"google.golang.org/genai"
)

type remoteFunctionTool interface {
	tool.Tool
	Declaration() *genai.FunctionDeclaration
	Run(agent.Context, any) (map[string]any, error)
}

// remoteApprovalTool retains the stock remote tool except for native rejection.
// ADK's functiontool.Run rejects Confirmed=false before kagent's handler can
// forward it. Forward that ruling explicitly, without changing it to approval.
// The normal plugin gates still check the resume and the returned child value.
type remoteApprovalTool struct {
	remoteFunctionTool
	config         adk.RemoteAgentConfig
	client         *http.Client
	propagateToken bool
}

func newRemoteApprovalTool(config adk.RemoteAgentConfig, propagateToken bool) (*remoteApprovalTool, error) {
	client := &http.Client{Transport: otelhttp.NewTransport(http.DefaultTransport)}
	stock, err := tools.NewKAgentRemoteA2ATool(config.Name, config.Description, config.Url, client, config.Headers, propagateToken, true)
	if err != nil {
		return nil, err
	}
	function, ok := stock.(remoteFunctionTool)
	if !ok {
		return nil, fmt.Errorf("remote agent tool does not implement the expected function contract")
	}
	return &remoteApprovalTool{function, config, client, propagateToken}, nil
}

func (t *remoteApprovalTool) ProcessRequest(_ agent.Context, request *model.LLMRequest) error {
	return toolutils.PackTool(request, t)
}

func (t *remoteApprovalTool) Run(ctx agent.Context, args any) (map[string]any, error) {
	confirmation := ctx.ToolConfirmation()
	if confirmation == nil || confirmation.Confirmed {
		return t.remoteFunctionTool.Run(ctx, args)
	}
	payloadMap, _ := confirmation.Payload.(map[string]any)
	payload := ka2a.ParseHitlConfirmationPayload(payloadMap)
	if payload.TaskID == "" || payload.ContextID == "" {
		return nil, fmt.Errorf("remote rejection requires the paused task and context")
	}
	decision := remoteRejectionDecision(payload)
	meta := a2aclient.CallMeta{}
	meta.Append("x-kagent-source", "agent")
	var resolveOptions []agentcard.ResolveOption
	for key, value := range t.config.Headers {
		meta.Append(key, value)
		resolveOptions = append(resolveOptions, agentcard.WithRequestHeader(key, value))
	}
	meta.Append("x-user-id", ctx.UserID())
	root := ctx.SessionID()
	if inbound, ok := a2asrv.CallContextFrom(ctx); ok && inbound.RequestMeta() != nil {
		if values, ok := inbound.RequestMeta().Get(tools.RootContextIDHeader); ok && len(values) > 0 {
			root = values[0]
		}
		if t.propagateToken && len(meta.Get(constants.AuthorizationHeader)) == 0 {
			if values, ok := inbound.RequestMeta().Get(constants.AuthorizationHeader); ok && len(values) > 0 {
				meta.Append(constants.AuthorizationHeader, values[0])
			}
		}
	}
	if len(meta.Get(tools.ParentContextIDHeader)) == 0 {
		meta.Append(tools.ParentContextIDHeader, ctx.SessionID())
	}
	if len(meta.Get(tools.RootContextIDHeader)) == 0 {
		meta.Append(tools.RootContextIDHeader, root)
	}
	card, err := agentcard.NewResolver(t.client).Resolve(ctx, t.config.Url, resolveOptions...)
	if err != nil {
		return nil, fmt.Errorf("could not resolve the remote agent for rejection")
	}
	client, err := a2aclient.NewFromCard(ctx, card, a2aclient.WithJSONRPCTransport(t.client),
		a2aclient.WithInterceptors(a2aclient.NewStaticCallMetaInjector(meta)))
	if err != nil {
		return nil, fmt.Errorf("could not construct the remote rejection client")
	}
	result, err := client.SendMessage(ctx, &a2atype.MessageSendParams{Message: &a2atype.Message{
		ID: a2atype.NewMessageID(), TaskID: a2atype.TaskID(payload.TaskID), ContextID: payload.ContextID,
		Role: a2atype.MessageRoleUser, Parts: a2atype.ContentParts{a2atype.DataPart{Data: decision}},
	}})
	if err != nil {
		return nil, fmt.Errorf("remote rejection delivery failed")
	}
	task, ok := result.(*a2atype.Task)
	if !ok || task.ContextID != payload.ContextID || string(task.ID) != payload.TaskID {
		return nil, fmt.Errorf("remote rejection returned a different task or context")
	}
	if task.Status.State == a2atype.TaskStateInputRequired {
		var parts []ka2a.HitlPartInfo
		if task.Status.Message != nil {
			parts = ka2a.ExtractHitlInfoFromParts(task.Status.Message.Parts)
		}
		next := ka2a.HitlConfirmationPayload{TaskID: string(task.ID), ContextID: task.ContextID, SubagentName: t.Name(), HitlParts: parts}
		if err := ctx.RequestConfirmation("The remote agent requires further input after rejection.", next.ToMap()); err != nil {
			return nil, err
		}
		return map[string]any{"status": "pending", "waiting_for": "subagent_approval", "subagent": t.Name(), "subagent_session_id": task.ContextID}, nil
	}
	if task.Status.State != a2atype.TaskStateCompleted {
		return nil, fmt.Errorf("remote rejection did not complete the paused task")
	}
	var texts []string
	appendText := func(parts a2atype.ContentParts) {
		for _, part := range parts {
			if text, ok := part.(a2atype.TextPart); ok && text.Text != "" {
				texts = append(texts, text.Text)
			}
		}
	}
	for _, artifact := range task.Artifacts {
		appendText(artifact.Parts)
	}
	if len(texts) == 0 && task.Status.Message != nil {
		appendText(task.Status.Message.Parts)
	}
	response := map[string]any{"result": strings.Join(texts, "\n"), "subagent_session_id": task.ContextID}
	if usage, ok := task.Metadata["kagent_usage_metadata"].(map[string]any); ok && len(usage) > 0 {
		response["kagent_usage_metadata"] = usage
	}
	return response, nil
}

// Match kagent's native payload precedence: batches and ask-user answers carry
// their own decisions; Confirmed=false represents rejection only otherwise.
func remoteRejectionDecision(payload ka2a.HitlConfirmationPayload) map[string]any {
	if len(payload.BatchDecisions) > 0 {
		decisions := map[string]any{}
		for id, decision := range payload.BatchDecisions {
			decisions[id] = string(decision)
		}
		data := map[string]any{ka2a.KAgentHitlDecisionTypeKey: ka2a.KAgentHitlDecisionTypeBatch, ka2a.KAgentHitlDecisionsKey: decisions}
		if len(payload.RejectionReasons) > 0 {
			data[ka2a.KAgentHitlRejectionReasonsKey] = payload.RejectionReasons
		}
		return data
	}
	if len(payload.Answers) > 0 {
		answers := make([]map[string]any, 0, len(payload.Answers))
		for _, answer := range payload.Answers {
			answers = append(answers, map[string]any{"answer": answer.Answer})
		}
		return map[string]any{ka2a.KAgentHitlDecisionTypeKey: ka2a.KAgentHitlDecisionTypeApprove, ka2a.KAgentAskUserAnswersKey: answers}
	}
	data := map[string]any{ka2a.KAgentHitlDecisionTypeKey: ka2a.KAgentHitlDecisionTypeReject}
	if payload.RejectionReason != "" {
		data["rejection_reason"] = payload.RejectionReason
	}
	return data
}
