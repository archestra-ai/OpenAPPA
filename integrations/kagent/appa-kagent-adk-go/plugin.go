// Package appakagentadk carries AppaPluginKagent — the adk/v2 plugin
// of the ghcr.io/archestra-ai/appa-kagent-adk-go image.
//
// The plugin maps each gated ADK callback onto one wire event, posts
// it to $APPA_RUNTIME_URL/hook, and enforces the answered decision. It
// holds no policy state: every answer comes from appa-runtime, and the
// plugin's only judgment is mechanical enforcement.
//
// Fail-closed contract. A gated callback returns an error on a
// transport failure, a non-200 answer, or an answer outside the
// decision contract. On the go ADK a returned error stops the gated
// action at its own point: an OnUserMessageCallback error aborts the
// run before the session append, a BeforeToolCallback error skips the
// tool and reaches the model as an error function response, and a
// BeforeAgentCallback error fails the agent scope. The model and
// emission callbacks feed no event but still probe the /hook channel
// with a ping and fail when it is down. Turn ends are the one
// exception: blocking a finished turn wedges the harness, so turn_end
// posts are best-effort and never fail the run.
//
// This is the go twin of the appa-kagent-adk python plugin. The wire
// events, decision enforcement, and trajectory-id semantics match it;
// VERIFICATION.md records the two mechanical deltas the go ADK forces
// (spawn classification by configured tool name, and the after-tool
// error path that go runs but python does not).
package appakagentadk

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"strings"
	"sync"
	"time"

	"google.golang.org/genai"

	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/plugin"
	"google.golang.org/adk/v2/session"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/toolconfirmation"
	"runtime/debug"
)

// ReservedTool is the engine's remedy-execution tool, served by
// appa-runtime at $APPA_RUNTIME_URL/mcp. The plugin answers its
// ToolCall hook with pass_control and lets the call through untouched;
// the runtime refuses a call no hook vouched for.
const ReservedTool = "execute_remedy_plan"

const (
	denyKey  = "appa"
	denied   = "denied"
	withheld = "withheld"

	// The lineage headers kagent's remote-agent tool stamps on every
	// delegated A2A call. The python-runtime executor persists the
	// inbound header dict into session state under "headers"; the
	// rc4 go-runtime executor does not (VERIFICATION.md), so on the
	// go cells every entry classifies as root until upstream lands
	// the headers in state — at which point this plugin classifies
	// identically to the python twin with no change.
	headersStateKey = "headers"
	rootHeader      = "x-kagent-root-context-id"
	parentHeader    = "x-kagent-parent-context-id"

	gatedTimeout   = 120 * time.Second
	turnEndTimeout = 30 * time.Second
)

// FailClosedError reports that the runtime blocked, refused, or could
// not answer. The gated action stops.
type FailClosedError struct {
	Reason string
}

func (e *FailClosedError) Error() string {
	return e.Reason
}

func failClosed(format string, args ...any) *FailClosedError {
	return &FailClosedError{Reason: fmt.Sprintf(format, args...)}
}

// Config builds an AppaPluginKagent.
type Config struct {
	// RuntimeURL is the appa-runtime base URL (APPA_RUNTIME_URL).
	// Required.
	RuntimeURL string
	// SpawnTools names the agent-as-tool entries of this agent: the
	// wire names of the remote agents in the rendered config. Calls to
	// these tools classify as spawns. Name-based because the kagent go
	// runtime builds remote-agent tools as plain function tools, so no
	// distinctive type exists to classify by (VERIFICATION.md).
	SpawnTools []string
	// HTTPClient overrides the transport; nil means a default client.
	// Timeouts are per request, so the client needs none of its own.
	HTTPClient *http.Client
}

// AppaPluginKagent posts one wire event per gated ADK callback and
// enforces the answered decision. Construct it with New and register
// the plugin ADKPlugin returns.
type AppaPluginKagent struct {
	hookURL    string
	client     *http.Client
	spawnTools map[string]struct{}

	// Identity bookkeeping, not policy state.
	mu sync.Mutex
	// sessionIDs pins each session's (root, child) classification at
	// first sight: a lane that lands the lineage headers after the
	// first event must not flip one session between two trajectories.
	sessionIDs map[string]trajectoryIDs
	// invocationAgents records the first agent scope each invocation
	// opens — the invocation's own agent. A later scope with another
	// name inside the same invocation is an in-process child.
	invocationAgents map[string]string
	// reviews holds the offers a deny_call handed over: those whose
	// plans consult a human authority, with the text the person reads.
	// The control call that quotes one asks the person through kagent's
	// own confirmation before it crosses, and the answer rides the call.
	reviews map[string]string
	// invocationIDs maps each running invocation to its trajectory ids.
	// adk-go hands the tool and agent callbacks a context that refuses
	// Session() and Agent() — only the run-level InvocationContext
	// carries the session — so the ids are pinned when the run opens and
	// looked up by InvocationID() in every later callback.
	invocationIDs map[string]trajectoryIDs
}

const (
	// reviewValue marks the plugin's own "the reviewer has been asked"
	// map under denyKey, so afterTool reports no dispatch for it.
	reviewValue = "review"
	// reviewPending is what the model reads while the person rules.
	// IsPendingReview recognizes it from outside the package.
	reviewPending = "[appa] this remedy needs a person's ruling. The reviewer has been asked through the " +
		"confirmation; wait for the answer and do not call the tool again."
)

// IsPendingReview reports whether a tool response is the plugin's own
// "the reviewer has been asked" answer for the reserved tool: the one
// beforeTool returns while a person rules on a remedy.
func IsPendingReview(tool string, response map[string]any) bool {
	return tool == ReservedTool && response[denyKey] == reviewValue
}

// trajectoryIDs is the (root, child) pair of an emitting scope. An
// empty childID means the scope is the root trajectory itself.
type trajectoryIDs struct {
	rootID  string
	childID string
}

// New validates cfg and builds the plugin.
func New(cfg Config) (*AppaPluginKagent, error) {
	if cfg.RuntimeURL == "" {
		return nil, errors.New("AppaPluginKagent needs the appa-runtime URL (APPA_RUNTIME_URL)")
	}
	client := cfg.HTTPClient
	if client == nil {
		client = &http.Client{}
	}
	spawnTools := make(map[string]struct{}, len(cfg.SpawnTools))
	for _, name := range cfg.SpawnTools {
		spawnTools[name] = struct{}{}
	}
	return &AppaPluginKagent{
		hookURL:          strings.TrimRight(cfg.RuntimeURL, "/") + "/hook",
		client:           client,
		spawnTools:       spawnTools,
		sessionIDs:       map[string]trajectoryIDs{},
		invocationAgents: map[string]string{},
		reviews:          map[string]string{},
		invocationIDs:    map[string]trajectoryIDs{},
	}, nil
}

// openInvocation pins the invocation's trajectory ids from the run-level
// context, the one place the session is readable.
func (p *AppaPluginKagent) openInvocation(ictx agent.InvocationContext) trajectoryIDs {
	ids := p.ids(ictx.Session())
	p.mu.Lock()
	p.invocationIDs[ictx.InvocationID()] = ids
	p.mu.Unlock()
	return ids
}

// closeInvocation forgets a finished invocation's ids.
func (p *AppaPluginKagent) closeInvocation(invocationID string) {
	p.mu.Lock()
	delete(p.invocationIDs, invocationID)
	p.mu.Unlock()
}

// idsFor is the trajectory of the invocation a tool or agent callback
// runs in. The run's own context pinned it; a context that still
// answers Session() (a test double) is the fallback, never the
// production path, which adk-go refuses with a logged line.
func (p *AppaPluginKagent) idsFor(ctx agent.Context) (trajectoryIDs, bool) {
	p.mu.Lock()
	ids, ok := p.invocationIDs[ctx.InvocationID()]
	p.mu.Unlock()
	if ok {
		return ids, true
	}
	if sess := ctx.Session(); sess != nil {
		return p.ids(sess), true
	}
	return trajectoryIDs{}, false
}

// rememberReviews keeps the reviews a deny handed over.
func (p *AppaPluginKagent) rememberReviews(review []Review) {
	p.mu.Lock()
	defer p.mu.Unlock()
	for _, entry := range review {
		p.reviews[entry.OfferID] = entry.Text
	}
}

// review looks up the text a person reads for this offer, if any.
func (p *AppaPluginKagent) review(offer string) (string, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	text, ok := p.reviews[offer]
	return text, ok
}

// forgetReview drops a review once the person's ruling rode the call.
func (p *AppaPluginKagent) forgetReview(offer string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	delete(p.reviews, offer)
}

// ADKPlugin wires the callbacks into the adk/v2 plugin the runner
// registers through runner.PluginConfig.
func (p *AppaPluginKagent) ADKPlugin() (*plugin.Plugin, error) {
	return plugin.New(plugin.Config{
		Name:                  "appa_plugin_kagent",
		OnUserMessageCallback: p.onUserMessage,
		OnEventCallback:       p.onEvent,
		BeforeRunCallback:     p.beforeRun,
		AfterRunCallback:      p.afterRun,
		BeforeAgentCallback:   p.beforeAgent,
		AfterAgentCallback:    p.afterAgent,
		BeforeModelCallback:   p.beforeModel,
		AfterModelCallback:    p.afterModel,
		OnModelErrorCallback:  p.onModelError,
		BeforeToolCallback:    p.beforeTool,
		AfterToolCallback:     p.afterTool,
		OnToolErrorCallback:   p.onToolError,
	})
}

// -- transport ----------------------------------------------------

func (p *AppaPluginKagent) send(ctx context.Context, wire map[string]any, timeout time.Duration) (int, []byte, error) {
	body, err := json.Marshal(wire)
	if err != nil {
		// plainJSON sanitizes the payload fields, so a whole-event
		// marshal failure is a plugin bug, not tool data.
		return 0, nil, fmt.Errorf("the wire event does not serialize: %w", err)
	}
	sendCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	request, err := http.NewRequestWithContext(sendCtx, http.MethodPost, p.hookURL, bytes.NewReader(body))
	if err != nil {
		return 0, nil, err
	}
	request.Header.Set("Content-Type", "application/json")
	answer, err := p.client.Do(request)
	if err != nil {
		return 0, nil, err
	}
	defer answer.Body.Close()
	answered, err := io.ReadAll(answer.Body)
	if err != nil {
		return 0, nil, err
	}
	return answer.StatusCode, answered, nil
}

// post sends one gated event; anything but a 200 decision fails closed.
func (p *AppaPluginKagent) post(ctx context.Context, wire map[string]any) (Decision, error) {
	status, answered, err := p.send(ctx, wire, gatedTimeout)
	if err != nil {
		return Decision{}, failClosed("the appa /hook channel is down: %v", err)
	}
	if status != http.StatusOK {
		return Decision{}, failClosed("appa answered %d: %s", status, truncate(answered, 500))
	}
	decision, err := parseDecision(answered)
	if err != nil {
		return Decision{}, failClosed("%v", err)
	}
	return decision, nil
}

// postQuiet sends a turn end; a finished turn never blocks the harness.
func (p *AppaPluginKagent) postQuiet(wire map[string]any) {
	if _, _, err := p.send(context.Background(), wire, turnEndTimeout); err != nil {
		log.Printf("appa: a turn end did not reach appa-runtime: %v", err)
	}
}

// pingHook is the liveness gate: pass only while the /hook channel
// answers. A ping feeds no event, so the runtime answers a bare 200 {}
// — reachability is the whole check.
func (p *AppaPluginKagent) pingHook(ctx context.Context) error {
	status, _, err := p.send(ctx, pingEvent(), gatedTimeout)
	if err != nil {
		return failClosed("the appa /hook channel is down: %v", err)
	}
	if status != http.StatusOK {
		return failClosed("the liveness probe answered %d", status)
	}
	return nil
}

func truncate(body []byte, limit int) string {
	if len(body) > limit {
		return string(body[:limit])
	}
	return string(body)
}

// -- ids ----------------------------------------------------------

// ids returns the (root, child) pair of the emitting scope.
//
// A delegated entry carries the caller's lineage headers in session
// state: the root context id names the root trajectory, and the
// session's own id becomes the child id. A plain session is the root
// itself.
func (p *AppaPluginKagent) ids(sess session.Session) trajectoryIDs {
	p.mu.Lock()
	defer p.mu.Unlock()
	if pinned, ok := p.sessionIDs[sess.ID()]; ok {
		return pinned
	}
	root := lineageRoot(sess)
	ids := trajectoryIDs{rootID: sess.ID()}
	if root != "" && root != sess.ID() {
		ids = trajectoryIDs{rootID: root, childID: sess.ID()}
	}
	p.sessionIDs[sess.ID()] = ids
	return ids
}

func lineageRoot(sess session.Session) string {
	value, err := sess.State().Get(headersStateKey)
	if err != nil {
		return ""
	}
	// Session state round-trips through JSON, so the header dict
	// arrives as map[string]any; a runtime handing the native go map
	// classifies the same.
	header := func(name string) string {
		switch headers := value.(type) {
		case map[string]any:
			if text, ok := headers[name].(string); ok {
				return text
			}
		case map[string]string:
			return headers[name]
		}
		return ""
	}
	if root := header(rootHeader); root != "" {
		return root
	}
	return header(parentHeader)
}

// isFresh reports whether no content has crossed this session yet.
//
// The kagent executor may append a state-only event before the first
// user message, so an empty event list is too strict — fresh means no
// content-bearing event.
func isFresh(sess session.Session) bool {
	events := sess.Events()
	for i := 0; i < events.Len(); i++ {
		if ev := events.At(i); ev != nil && ev.Content != nil {
			return false
		}
	}
	return true
}

func (p *AppaPluginKagent) isSpawn(t tool.Tool) bool {
	_, spawn := p.spawnTools[t.Name()]
	return spawn
}

// claimScope reports whether the named agent scope is the invocation's
// own: the first scope an invocation opens, or a re-entry of it.
func (p *AppaPluginKagent) claimScope(invocationID, agentName string) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	first, seen := p.invocationAgents[invocationID]
	if !seen {
		p.invocationAgents[invocationID] = agentName
		return true
	}
	return first == agentName
}

func (p *AppaPluginKagent) releaseScope(invocationID string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	delete(p.invocationAgents, invocationID)
}

// localChildID is an in-process child scope's id, unique per
// invocation.
func localChildID(invocationID, agentName string) string {
	return invocationID + ":" + agentName
}

// -- session and prompt -------------------------------------------

func (p *AppaPluginKagent) onUserMessage(ictx agent.InvocationContext, userMessage *genai.Content) (*genai.Content, error) {
	sess := ictx.Session()
	ids := p.openInvocation(ictx)
	if isFresh(sess) {
		opening := sessionStartEvent(ids.rootID)
		if ids.childID != "" {
			opening = childStartEvent(ids.rootID, ids.childID, "")
			log.Printf("appa: child %s opens under root %s", ids.childID, ids.rootID)
		} else {
			log.Printf("appa: trajectory %s opens as a root", ids.rootID)
		}
		decision, err := p.post(ictx, opening)
		if err != nil {
			return nil, err
		}
		if decision.Kind != "ack" {
			return nil, failClosed("appa refused the session: %s", decision.describe())
		}
	}
	decision, err := p.post(ictx, promptEvent(ids.rootID, contentText(userMessage), ids.childID))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "ack":
		return nil, nil
	case "block":
		return nil, failClosed("appa blocked the prompt: %s", decision.Reason)
	default:
		return nil, failClosed("appa answered the prompt with %s", decision.describe())
	}
}

// -- liveness gates -----------------------------------------------

func (p *AppaPluginKagent) beforeRun(ictx agent.InvocationContext) (*genai.Content, error) {
	p.openInvocation(ictx)
	return nil, p.pingHook(ictx)
}

func (p *AppaPluginKagent) onEvent(ictx agent.InvocationContext, _ *session.Event) (*session.Event, error) {
	return nil, p.pingHook(ictx)
}

func (p *AppaPluginKagent) beforeModel(ctx agent.Context, req *model.LLMRequest) (*model.LLMResponse, error) {
	stripConfirmationParts(req)
	return nil, p.pingHook(ctx)
}

// stripConfirmationParts keeps the confirmation exchange out of the
// model's view: the adk_request_confirmation call and its response are
// the harness asking a person, not the model's own history. kagent's
// stock approval gate strips them the same way.
func stripConfirmationParts(req *model.LLMRequest) {
	if req == nil {
		return
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			log.Printf("appa: stripping the confirmation parts panicked (ignored): %v\n%s", recovered, debug.Stack())
		}
	}()
	kept := req.Contents[:0]
	for _, content := range req.Contents {
		if content == nil {
			continue
		}
		parts := content.Parts[:0]
		for _, part := range content.Parts {
			if part == nil {
				continue
			}
			if part.FunctionCall != nil && part.FunctionCall.Name == toolconfirmation.FunctionCallName {
				continue
			}
			if part.FunctionResponse != nil && part.FunctionResponse.Name == toolconfirmation.FunctionCallName {
				continue
			}
			parts = append(parts, part)
		}
		content.Parts = parts
		if len(content.Parts) > 0 {
			kept = append(kept, content)
		}
	}
	req.Contents = kept
}

func (p *AppaPluginKagent) afterModel(ctx agent.Context, _ *model.LLMResponse, _ error) (*model.LLMResponse, error) {
	return nil, p.pingHook(ctx)
}

func (p *AppaPluginKagent) onModelError(ctx agent.Context, _ *model.LLMRequest, _ error) (*model.LLMResponse, error) {
	// On a live channel the original model error propagates.
	return nil, p.pingHook(ctx)
}

// -- agent scopes -------------------------------------------------

func (p *AppaPluginKagent) beforeAgent(ctx agent.Context) (*genai.Content, error) {
	agentName := ctx.AgentName()
	if p.claimScope(ctx.InvocationID(), agentName) {
		// The invocation's own agent: the prompt already gated these
		// bytes (root), or the delegated entry did (child pod).
		return nil, p.pingHook(ctx)
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	decision, err := p.post(ctx, childStartEvent(ids.rootID, localChildID(ctx.InvocationID(), agentName), ""))
	if err != nil {
		return nil, err
	}
	if decision.Kind != "ack" {
		return nil, failClosed("appa refused the child scope: %s", decision.describe())
	}
	return nil, nil
}

func (p *AppaPluginKagent) afterAgent(ctx agent.Context) (*genai.Content, error) {
	agentName := ctx.AgentName()
	if p.claimScope(ctx.InvocationID(), agentName) {
		return nil, p.pingHook(ctx)
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, nil // a scope with no pinned run: nothing to close
	}
	p.postQuiet(turnEndEvent(ids.rootID, localChildID(ctx.InvocationID(), agentName)))
	return nil, nil
}

// -- the tool gate ------------------------------------------------

func (p *AppaPluginKagent) beforeTool(ctx agent.Context, t tool.Tool, args map[string]any) (map[string]any, error) {
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	ruling := ""
	if t.Name() == ReservedTool {
		offer, _ := args["offer_id"].(string)
		if confirmation := ctx.ToolConfirmation(); confirmation == nil {
			if text, reviewed := p.review(offer); reviewed {
				// The person rules before the act, through kagent's stock
				// confirmation. The hint is the consult artifact as the
				// runtime renders it — nothing the model said — and the
				// answer comes back on the resumed call, never through the
				// model.
				if err := ctx.RequestConfirmation(text, map[string]any{"appa": reviewValue, "offer_id": offer}); err != nil {
					return nil, failClosed("the review could not be raised: %v", err)
				}
				return map[string]any{"result": reviewPending, denyKey: reviewValue}, nil
			}
		} else {
			ruling = "deny"
			if confirmation.Confirmed {
				ruling = "approve"
			}
			p.forgetReview(offer)
		}
	}
	decision, err := p.post(ctx, toolCallEvent(ids.rootID, t.Name(), plainJSON(orEmpty(args)), p.isSpawn(t), ids.childID, ruling))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "allow_call", "pass_control":
		return nil, nil
	case "deny_call":
		p.rememberReviews(decision.Review)
		return map[string]any{"result": decision.Feedback, denyKey: denied}, nil
	default:
		return nil, failClosed("appa answered the tool call with %s", decision.describe())
	}
}

func (p *AppaPluginKagent) afterTool(ctx agent.Context, t tool.Tool, args, result map[string]any, toolErr error) (map[string]any, error) {
	if toolErr != nil {
		// The go ADK runs the after-tool point on error paths too.
		// The failure already crossed at onToolError — or it is the
		// plugin's own fail-closed error from the call gate — so a
		// second report would double-count one dispatch.
		return nil, nil
	}
	if result != nil && (result[denyKey] == denied || result[denyKey] == reviewValue) {
		// The plugin's own deny or review map flowing back: already
		// reported at the call, and the runtime never opened a dispatch.
		return nil, nil
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	arguments := plainJSON(orEmpty(args))
	// A nil result with no error is a deferred or long-running tool:
	// nothing has entered attention, and the dispatch is genuinely
	// unresolved at this point.
	outcome := indeterminateOutcome()
	if result != nil {
		outcome = successOutcome(plainJSON(result))
	}
	if p.isSpawn(t) {
		spawnedID, value := spawnReturn(result)
		decision, err := p.post(ctx, spawnResultEvent(ids.rootID, t.Name(), arguments, outcome, spawnedID, value, ids.childID))
		if err != nil {
			return nil, err
		}
		switch decision.Kind {
		case "ack":
			return nil, nil
		case "child_return":
			return map[string]any{"result": decision.Value}, nil
		case "replace_output":
			return map[string]any{"result": decision.Output}, nil
		case "block":
			return withheldResult(decision.Reason), nil
		default:
			return nil, failClosed("appa answered the spawn result with %s", decision.describe())
		}
	}
	decision, err := p.post(ctx, toolResultEvent(ids.rootID, t.Name(), arguments, outcome, ids.childID))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "ack":
		return nil, nil
	case "replace_output":
		return map[string]any{"result": decision.Output}, nil
	case "block":
		return withheldResult(decision.Reason), nil
	default:
		return nil, failClosed("appa answered the tool result with %s", decision.describe())
	}
}

func (p *AppaPluginKagent) onToolError(ctx agent.Context, t tool.Tool, args map[string]any, toolErr error) (map[string]any, error) {
	var own *FailClosedError
	if errors.As(toolErr, &own) {
		// The plugin's own gate stopped this call: already reported,
		// and the runtime never opened a dispatch. Returning nothing
		// keeps the fail-closed error terminal.
		return nil, nil
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	decision, err := p.post(ctx, toolResultEvent(ids.rootID, t.Name(), plainJSON(orEmpty(args)), failureOutcome(toolErr.Error()), ids.childID))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "ack":
		return nil, nil // the original error propagates
	case "replace_output":
		return map[string]any{"result": decision.Output}, nil
	case "block":
		return withheldResult(decision.Reason), nil
	default:
		return nil, failClosed("appa answered the tool failure with %s", decision.describe())
	}
}

// -- turn ends ----------------------------------------------------

func (p *AppaPluginKagent) afterRun(ictx agent.InvocationContext) {
	ids := p.ids(ictx.Session())
	p.postQuiet(turnEndEvent(ids.rootID, ids.childID))
	p.releaseScope(ictx.InvocationID())
	p.closeInvocation(ictx.InvocationID())
}

// -- value shaping ------------------------------------------------

func withheldResult(reason string) map[string]any {
	return map[string]any{"result": "[appa] the tool result was withheld: " + reason, denyKey: withheld}
}

func orEmpty(args map[string]any) map[string]any {
	if args == nil {
		return map[string]any{}
	}
	return args
}

// plainJSON returns a JSON-representable form of an ADK value. Almost
// every value the go ADK hands over is already one (tool args and
// results are JSON-decoded maps); a value that does not serialize is
// carried as its go rendering so no flow becomes invisible to policy.
func plainJSON(value any) any {
	if _, err := json.Marshal(value); err != nil {
		return fmt.Sprintf("%v", value)
	}
	return value
}

// contentText is the prompt text of a user message: its text parts,
// joined. A message with no text part still crosses the gate — as its
// JSON rendering, so non-text ingress is never invisible to the policy.
func contentText(content *genai.Content) string {
	if content == nil {
		return ""
	}
	var texts []string
	for _, part := range content.Parts {
		if part != nil && part.Text != "" {
			texts = append(texts, part.Text)
		}
	}
	if len(texts) > 0 {
		return strings.Join(texts, "\n")
	}
	if rendered, err := json.Marshal(content); err == nil {
		return string(rendered)
	}
	return fmt.Sprintf("%v", content)
}

// spawnReturn extracts the (spawned child id, returned value) a spawn
// result carries. The kagent remote-agent tool answers with a result
// map whose "result" holds the child's reply and whose
// "subagent_session_id" holds the child's context id; error and
// input-required branches carry neither, and both shapes cross.
func spawnReturn(result map[string]any) (spawnedID, value string) {
	if result == nil {
		return "", ""
	}
	switch v := result["result"].(type) {
	case nil:
	case string:
		value = v
	default:
		if rendered, err := json.Marshal(v); err == nil {
			value = string(rendered)
		} else {
			value = fmt.Sprintf("%v", v)
		}
	}
	if s, ok := result["subagent_session_id"].(string); ok {
		spawnedID = s
	}
	return spawnedID, value
}
