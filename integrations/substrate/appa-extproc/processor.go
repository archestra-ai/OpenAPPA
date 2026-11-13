// The ext_proc stream: one per HTTP request the egress gateway proxies.
//
// For a request to a configured MCP host, the processor asks the gateway for
// the request body and the response with a mode override, so it reads only
// what it gates; every other request passes on its headers. The trajectory
// is the actor: its SPIFFE ID, which the gateway took from the verified
// certificate of the actor's tunnel, so every MCP session and every process
// in one actor share one label, and nothing inside the sandbox can name
// another actor's trajectory.

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	corev3 "github.com/envoyproxy/go-control-plane/envoy/config/core/v3"
	filterv3 "github.com/envoyproxy/go-control-plane/envoy/extensions/filters/http/ext_proc/v3"
	extprocv3 "github.com/envoyproxy/go-control-plane/envoy/service/ext_proc/v3"
	typev3 "github.com/envoyproxy/go-control-plane/envoy/type/v3"
)

// identityAttribute is the request attribute Substrate's egress gateway
// fills with the actor's SPIFFE ID.
const identityAttribute = "filter_state['dev.ate.actor.identity']"

// gatedMode is the mode a gated request switches to. Substrate's gateway
// accepts exactly this override.
var gatedMode = &filterv3.ProcessingMode{
	RequestHeaderMode:   filterv3.ProcessingMode_SEND,
	ResponseHeaderMode:  filterv3.ProcessingMode_SEND,
	RequestBodyMode:     filterv3.ProcessingMode_BUFFERED,
	ResponseBodyMode:    filterv3.ProcessingMode_BUFFERED,
	RequestTrailerMode:  filterv3.ProcessingMode_SKIP,
	ResponseTrailerMode: filterv3.ProcessingMode_SKIP,
}

type processorConfig struct {
	servers         map[string]string
	runtime         *runtimeClient
	journal         *journal
	acceptNarrowing bool
}

type processor struct {
	extprocv3.UnimplementedExternalProcessorServer
	config processorConfig

	mu sync.Mutex
	// epochs counts resets per actor; the trajectory id carries the count.
	epochs map[string]int
	// started holds the trajectories this process has opened.
	started map[string]bool
	calls   atomic.Uint64
}

func newProcessor(config processorConfig) *processor {
	return &processor{config: config, epochs: map[string]int{}, started: map[string]bool{}}
}

// exchange is what one stream learns about its request.
type exchange struct {
	actor  string
	host   string
	server string
	gated  bool

	call        *toolCall
	tool        string
	root        string
	callID      string
	allowed     bool
	contentType string
	status      int
}

func (p *processor) Process(stream extprocv3.ExternalProcessor_ProcessServer) error {
	var ex exchange
	for {
		request, err := stream.Recv()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return err
		}
		response := p.handle(stream.Context(), &ex, request)
		if err := stream.Send(response); err != nil {
			return err
		}
	}
}

func (p *processor) handle(ctx context.Context, ex *exchange, request *extprocv3.ProcessingRequest) *extprocv3.ProcessingResponse {
	switch phase := request.Request.(type) {
	case *extprocv3.ProcessingRequest_RequestHeaders:
		return p.requestHeaders(ex, request, phase.RequestHeaders)
	case *extprocv3.ProcessingRequest_RequestBody:
		return p.requestBody(ctx, ex, phase.RequestBody)
	case *extprocv3.ProcessingRequest_ResponseHeaders:
		ex.contentType = header(phase.ResponseHeaders.GetHeaders(), "content-type")
		ex.status, _ = strconv.Atoi(header(phase.ResponseHeaders.GetHeaders(), ":status"))
		return &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_ResponseHeaders{ResponseHeaders: &extprocv3.HeadersResponse{}}}
	case *extprocv3.ProcessingRequest_ResponseBody:
		return p.responseBody(ctx, ex, phase.ResponseBody)
	case *extprocv3.ProcessingRequest_RequestTrailers:
		return &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_RequestTrailers{RequestTrailers: &extprocv3.TrailersResponse{}}}
	case *extprocv3.ProcessingRequest_ResponseTrailers:
		return &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_ResponseTrailers{ResponseTrailers: &extprocv3.TrailersResponse{}}}
	}
	return immediate(500, "text/plain", []byte("appa-extproc: an ext_proc message it does not know"))
}

func (p *processor) requestHeaders(ex *exchange, request *extprocv3.ProcessingRequest, headers *extprocv3.HttpHeaders) *extprocv3.ProcessingResponse {
	pass := &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_RequestHeaders{RequestHeaders: &extprocv3.HeadersResponse{}}}
	ex.host = hostOnly(header(headers.GetHeaders(), ":authority"))
	server, ok := p.config.servers[ex.host]
	if !ok {
		return pass
	}
	ex.server = server
	ex.actor = attribute(request, identityAttribute)
	if ex.actor == "" {
		// The gateway refuses a callout without an identity already; this
		// processor never gates a call it cannot attribute.
		return immediate(403, "text/plain", []byte("appa-extproc: the request carries no actor identity"))
	}
	if header(headers.GetHeaders(), ":method") != "POST" {
		// A GET opens the server's notification stream and a DELETE ends a
		// session; neither runs a tool.
		return pass
	}
	ex.gated = true
	pass.ModeOverride = gatedMode
	return pass
}

func (p *processor) requestBody(ctx context.Context, ex *exchange, body *extprocv3.HttpBody) *extprocv3.ProcessingResponse {
	pass := &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_RequestBody{RequestBody: &extprocv3.BodyResponse{}}}
	if !ex.gated {
		return pass
	}
	call, err := parseToolCall(body.GetBody())
	if err != nil {
		return p.refuse(ex, nil, "", fmt.Sprintf("[appa] Blocked: the egress gateway could not read this MCP request (%v).", err))
	}
	if call == nil {
		return pass
	}
	ex.call = call
	ex.tool = "mcp:" + ex.server + "/" + call.Name
	ex.root = p.trajectory(ex.actor)
	ex.callID = fmt.Sprintf("extproc-%d", p.calls.Add(1))

	if err := p.ensureStarted(ctx, ex.root); err != nil {
		return p.refuse(ex, call.ID, "", "[appa] Blocked: "+err.Error())
	}
	var narrowed bool
	for attempt := 0; attempt < 3; attempt++ {
		answer, err := p.config.runtime.toolCall(ctx, ex.root, ex.callID, ex.tool, call.Arguments)
		if err != nil {
			return p.refuse(ex, call.ID, "", "[appa] Blocked: "+err.Error())
		}
		if answer.Kind == "allow_call" {
			ex.allowed = true
			verdict := "allowed"
			if narrowed {
				verdict = "allowed after narrowing"
			}
			p.record(ctx, ex, verdict, "")
			return pass
		}
		offer, ok := p.narrowing(answer)
		if !ok {
			return p.refuse(ex, call.ID, answer.Kind, answer.message())
		}
		p.record(ctx, ex, "held", reasons(answer.message()))
		if err := p.config.runtime.takeRemedy(ctx, ex.root, offer); err != nil {
			return p.refuse(ex, call.ID, "", "[appa] Blocked: "+err.Error())
		}
		narrowed = true
	}
	return p.refuse(ex, call.ID, "", "[appa] Blocked: the call stayed held after its remedy ran.")
}

func (p *processor) responseBody(ctx context.Context, ex *exchange, body *extprocv3.HttpBody) *extprocv3.ProcessingResponse {
	pass := &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_ResponseBody{ResponseBody: &extprocv3.BodyResponse{}}}
	if ex.call == nil || !ex.allowed {
		return pass
	}
	result := parseToolResult(ex.contentType, body.GetBody(), ex.call.ID)
	outcome := map[string]any{"status": "indeterminate"}
	switch {
	case ex.status >= 400:
		outcome = map[string]any{"status": "failure", "message": fmt.Sprintf("HTTP %d", ex.status)}
	case result.Found && result.Failed:
		outcome = map[string]any{"status": "failure", "message": result.Text}
	case result.Found:
		outcome = map[string]any{"status": "success", "body": result.Text}
	}
	answer, err := p.config.runtime.toolResult(ctx, ex.root, ex.callID, ex.tool, ex.call.Arguments, outcome)
	if err != nil {
		// The result is withheld: the runtime never saw it, so the label
		// would not cover it.
		p.record(ctx, ex, "result withheld", err.Error())
		return replaceBody(ex.contentType, toolError(ex.call.ID, "[appa] The result was withheld: "+err.Error()))
	}
	switch answer.Kind {
	case "ack":
		p.record(ctx, ex, "result labeled", "")
		return pass
	case "replace_output":
		p.record(ctx, ex, "result replaced", "")
		return replaceBody(ex.contentType, toolAnswer(ex.call.ID, answer.Output, false))
	case "deliver_value":
		p.record(ctx, ex, "result sanitized", "")
		return replaceBody(ex.contentType, toolAnswer(ex.call.ID, answer.Value, false))
	}
	p.record(ctx, ex, "result withheld", answer.message())
	return replaceBody(ex.contentType, toolError(ex.call.ID, answer.message()))
}

// reset starts a new trajectory for an actor: its next call runs under a
// fresh label. The old trajectory stays in the runtime's log.
func (p *processor) reset(actor string) string {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.epochs[actor]++
	return p.trajectoryLocked(actor)
}

func (p *processor) trajectory(actor string) string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.trajectoryLocked(actor)
}

func (p *processor) trajectoryLocked(actor string) string {
	if epoch := p.epochs[actor]; epoch > 0 {
		return fmt.Sprintf("%s/run-%d", actor, epoch)
	}
	return actor
}

// ensureStarted opens a trajectory once per process. The runtime acks a
// repeated start and keeps the label, so a restarted processor resumes it.
func (p *processor) ensureStarted(ctx context.Context, root string) error {
	p.mu.Lock()
	started := p.started[root]
	p.mu.Unlock()
	if started {
		return nil
	}
	answer, err := p.config.runtime.sessionStart(ctx, root)
	if err != nil {
		return err
	}
	if answer.Kind != "ack" {
		return fmt.Errorf("the runtime did not open the session: %s", answer.message())
	}
	p.mu.Lock()
	p.started[root] = true
	p.mu.Unlock()
	return nil
}

// narrowing picks the offer the processor may take for the actor: one that
// no person reviews and that declares no child return. Such a plan narrows
// what the actor may do next and widens nothing, which is why it needs no
// one's consent. An offer that asks a person is left to the agent.
func (p *processor) narrowing(answer decision) (string, bool) {
	if !p.config.acceptNarrowing || answer.Kind != "deny_call" || len(answer.Review) > 0 {
		return "", false
	}
	for _, candidate := range answer.Offers {
		if len(candidate.Returns) == 0 && candidate.OfferID != "" {
			return candidate.OfferID, true
		}
	}
	return "", false
}

// refuse answers the call before it leaves the cluster, with the runtime's
// words as the tool's error result.
func (p *processor) refuse(ex *exchange, id json.RawMessage, kind, text string) *extprocv3.ProcessingResponse {
	verdict := "blocked"
	if kind == "refuse" {
		verdict = "refused"
	}
	p.record(context.Background(), ex, verdict, reasons(text))
	log.Printf("appa-extproc: %s %s for %s", verdict, ex.tool, ex.actor)
	return immediate(200, "application/json", toolError(id, text))
}

func (p *processor) record(ctx context.Context, ex *exchange, verdict, reason string) {
	entry := entry{
		Time:    time.Now().UTC(),
		Actor:   ex.actor,
		Root:    ex.root,
		Tool:    ex.tool,
		Verdict: verdict,
		Reason:  reason,
	}
	if ex.call != nil {
		entry.Arguments = ex.call.Arguments
	}
	if ex.root != "" {
		statusCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
		defer cancel()
		if status, err := p.config.runtime.status(statusCtx, ex.root); err == nil {
			entry.Status = status
		}
	}
	p.config.journal.add(entry)
}

func immediate(code int, contentType string, body []byte) *extprocv3.ProcessingResponse {
	return &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_ImmediateResponse{ImmediateResponse: &extprocv3.ImmediateResponse{
		Status: &typev3.HttpStatus{Code: typev3.StatusCode(code)},
		Headers: &extprocv3.HeaderMutation{SetHeaders: []*corev3.HeaderValueOption{{
			Header: &corev3.HeaderValue{Key: "content-type", RawValue: []byte(contentType)},
		}}},
		Body:    body,
		Details: "appa-extproc",
	}}}
}

func replaceBody(contentType string, message []byte) *extprocv3.ProcessingResponse {
	return &extprocv3.ProcessingResponse{Response: &extprocv3.ProcessingResponse_ResponseBody{ResponseBody: &extprocv3.BodyResponse{
		Response: &extprocv3.CommonResponse{
			BodyMutation: &extprocv3.BodyMutation{Mutation: &extprocv3.BodyMutation_Body{Body: frame(contentType, message)}},
		},
	}}}
}

func header(headers *corev3.HeaderMap, name string) string {
	for _, h := range headers.GetHeaders() {
		if strings.EqualFold(h.GetKey(), name) {
			if len(h.GetRawValue()) > 0 {
				return string(h.GetRawValue())
			}
			return h.GetValue()
		}
	}
	return ""
}

// attribute reads a request attribute. The attributes are keyed by the
// filter's name in the chain, which the gateway's config owns, so every
// entry is scanned.
func attribute(request *extprocv3.ProcessingRequest, name string) string {
	for _, attributes := range request.GetAttributes() {
		if value, ok := attributes.GetFields()[name]; ok {
			return value.GetStringValue()
		}
	}
	return ""
}

func hostOnly(authority string) string {
	host := strings.ToLower(authority)
	if strings.HasPrefix(host, "[") {
		if end := strings.Index(host, "]"); end > 0 {
			return host[1:end]
		}
	}
	if colon := strings.LastIndex(host, ":"); colon > 0 {
		return host[:colon]
	}
	return host
}

// reasons is the "Why:" lines of a refusal, for the decision log.
func reasons(text string) string {
	_, why, found := strings.Cut(text, "\nWhy:\n")
	if !found {
		return strings.TrimSpace(text)
	}
	if end := strings.Index(why, "\n\n"); end >= 0 {
		why = why[:end]
	}
	var lines []string
	for _, line := range strings.Split(why, "\n") {
		if line = strings.TrimSpace(strings.TrimPrefix(strings.TrimSpace(line), "-")); line != "" {
			lines = append(lines, line)
		}
	}
	return strings.Join(lines, "\n")
}
