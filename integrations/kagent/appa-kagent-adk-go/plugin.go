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
// events, decision enforcement, and trajectory-id semantics match it.
// VERIFICATION.md records the mechanical deltas the go ADK forces on
// the plugin. Spawns classify by configured tool name. The invocation's
// own scope is the first agent name it opens. The after-tool error path
// runs on go and not on python. A deferred result crosses as an
// indeterminate outcome. A delegated child opens once per (root, child)
// pair, not once per session: kagent's go remote-agent tool sends every
// delegation of one parent pod into one child context id, so one child
// session id serves every parent in turn, and the pair — not the
// session — decides whether a child_start is due (openScope).
//
// The plugin holds the stop of a child scope. It registers the
// APPA-owned tool appa_return on every model request of that scope, and
// it replaces the final message of the child with one call to that
// tool. The body of the tool posts child_end, where the value of the
// child crosses. The runtime acknowledges the value, names other bytes,
// or blocks the return with a reason the model reads as a tool result.
//
// The plugin decides from what it owns, never from a name or a payload
// key something outside it can also write. The gate takes the
// appa_return slot of every request of a child scope and never yields
// it, and the tool points recognize the gate by the object the plugin
// built: a tool of that name from a toolset is a foreign tool and
// crosses the tool gate like any other. The deny and review maps the
// plugin hands the model carry the appa markers the model reads, and a
// tool result that copies them skips nothing — the plugin remembers
// which calls it answered itself, by function-call id.
//
// The plugin also declares the return of a spawn itself. A deny_call
// that offers a return route never reaches the model: the plugin takes
// the bare floor, runs that plan on the /mcp endpoint of the runtime,
// and proposes the same call again.
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

	"github.com/modelcontextprotocol/go-sdk/mcp"
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

// ReturnTool is the tool a child scope stops through. APPA owns it, so
// it crosses no tool gate.
const ReturnTool = "appa_return"

// What the return gate hands the model back. A crossing names the bytes
// the child must repeat, so its outgoing reply carries what crossed.
const (
	returnCrossed = "[appa] the return crossed. End this errand now, " +
		"with exactly this text as your final message:\n%s"
	returnVoid    = "[appa] the void return crossed. End this errand now with an empty final message."
	returnBlocked = "[appa] this return did not cross: %s"
)

// The call gate's own refusal of a name the inventory does not carry.
const outsideInventory = "[appa] the tool %s is outside the gated inventory of this agent, so the call was refused"

const (
	denyKey  = "appa"
	denied   = "denied"
	withheld = "withheld"

	// The lineage headers kagent's remote-agent tool stamps on every
	// delegated A2A call. The python-runtime executor persists the
	// inbound header dict into session state under "headers". The
	// rc4 go-runtime executor does not, so the runtime main
	// (cmd/appa-kagent-adk-go/main.go) lands them under the same key
	// on every session Get and Create (VERIFICATION.md). The plugin
	// then classifies a delegated entry exactly as the python twin.
	headersStateKey = "headers"
	rootHeader      = "x-kagent-root-context-id"
	parentHeader    = "x-kagent-parent-context-id"

	gatedTimeout   = 120 * time.Second
	turnEndTimeout = 30 * time.Second
	// The remedy call the plugin routes itself, over the MCP endpoint of
	// the runtime. One plan can hold for the whole consult window of the
	// runtime, so this budget must outlast that window.
	remedyTimeout = 300 * time.Second
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
	// Inventory spells every tool this agent can dispatch
	// (inventory.go). A call of a name outside it is refused at the
	// gate, and a spelled agent: tool classifies as a spawn.
	Inventory Inventory
	// HTTPClient overrides the transport; nil means a default client.
	// Timeouts are per request, so the client needs none of its own.
	HTTPClient *http.Client
}

// AppaPluginKagent posts one wire event per gated ADK callback and
// enforces the answered decision. Construct it with New and register
// the plugin ADKPlugin returns.
type AppaPluginKagent struct {
	hookURL   string
	mcpURL    string
	client    *http.Client
	inventory Inventory
	// returnTool is the tool a child scope stops through. adk-go
	// resolves the call from the request the plugin registered it on.
	returnTool *returnGate
	// remedyCall is the declaration path the plugin routes without the
	// model: it takes the arguments of execute_remedy_plan and answers
	// with the text the runtime rendered.
	remedyCall func(ctx context.Context, arguments map[string]any) (string, error)

	// Identity bookkeeping, not policy state.
	mu sync.Mutex
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
	// opened holds the (root, child) pairs whose child_start the runtime
	// acked through this plugin instance. A pair the runtime refused, or
	// that never reached it, stays out and opens again on the next
	// entry. Nothing prunes the set: one entry per parent that delegates
	// into this pod over its life.
	opened map[trajectoryIDs]struct{}
	// crossed holds the return the gate crossed for a run, by invocation
	// id, and the exact bytes that crossed. The stop of that run then
	// carries those bytes, so the reply the child sends replays them.
	// The run's end drops the entry.
	crossed map[string]string
	// answered holds the function-call ids the plugin answered itself:
	// a deny, or a pending review. The runtime opened no dispatch for
	// those calls, so the after-tool point must open no second report.
	// The set is what decides that, never a marker in the result map: a
	// tool answers with whatever bytes it likes, the appa markers
	// included, and only the plugin knows which calls it answered. The
	// after-tool point drops the entry it reads.
	answered map[string]struct{}
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
	p := &AppaPluginKagent{
		hookURL:          strings.TrimRight(cfg.RuntimeURL, "/") + "/hook",
		mcpURL:           strings.TrimRight(cfg.RuntimeURL, "/") + "/mcp",
		client:           client,
		inventory:        cfg.Inventory,
		invocationAgents: map[string]string{},
		reviews:          map[string]string{},
		invocationIDs:    map[string]trajectoryIDs{},
		opened:           map[trajectoryIDs]struct{}{},
		crossed:          map[string]string{},
		answered:         map[string]struct{}{},
	}
	p.returnTool = &returnGate{plugin: p}
	p.remedyCall = p.remedyOverMCP
	return p, nil
}

// openInvocation pins the invocation's trajectory ids from the run-level
// context, the one place the session is readable. The pin reads the
// session state of this run, so every callback inside the run carries
// one (root, child) pair, and the next run classifies afresh.
func (p *AppaPluginKagent) openInvocation(ictx agent.InvocationContext) trajectoryIDs {
	ids := classify(ictx.Session())
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
	if ids, pinned := p.pinnedIDs(ctx.InvocationID()); pinned {
		return ids, true
	}
	if sess := ctx.Session(); sess != nil {
		return classify(sess), true
	}
	return trajectoryIDs{}, false
}

// pinnedIDs is the trajectory the run open pinned for this invocation,
// if a run open reached this plugin.
func (p *AppaPluginKagent) pinnedIDs(invocationID string) (trajectoryIDs, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	ids, ok := p.invocationIDs[invocationID]
	return ids, ok
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

// isOpened reports whether this plugin instance already opened the pair.
func (p *AppaPluginKagent) isOpened(ids trajectoryIDs) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	_, opened := p.opened[ids]
	return opened
}

// markOpened records the pair the runtime just acked.
func (p *AppaPluginKagent) markOpened(ids trajectoryIDs) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.opened[ids] = struct{}{}
}

// holdCrossed keeps the exact bytes the return of this run crossed with.
func (p *AppaPluginKagent) holdCrossed(invocationID, value string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.crossed[invocationID] = value
}

// crossedValue is what the return of this run crossed with, if it
// crossed. An empty value that crossed reads as crossed.
func (p *AppaPluginKagent) crossedValue(invocationID string) (string, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	value, crossed := p.crossed[invocationID]
	return value, crossed
}

// dropCrossed forgets what a finished run's return crossed with.
func (p *AppaPluginKagent) dropCrossed(invocationID string) {
	p.mu.Lock()
	defer p.mu.Unlock()
	delete(p.crossed, invocationID)
}

// answerOwn records that the plugin answered this call itself, so the
// after-tool point of the same call opens no second report.
//
// adk-go builds one tool context per function call and hands that same
// context to the before-tool point, the tool, the error point and the
// after-tool point (internal/llminternal/base_flow.go:1041-1091,
// 1232-1272), so FunctionCallID names the same call at every point. A
// call with no id records nothing: the after-tool point then reports it
// like any other result, which is the safe direction — the runtime
// refuses a dispatch it never opened, and no gate is skipped.
func (p *AppaPluginKagent) answerOwn(callID string) {
	if callID == "" {
		return
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	p.answered[callID] = struct{}{}
}

// consumeAnswer reports whether the plugin answered this call itself
// and drops the entry, so the set holds only the calls in flight.
func (p *AppaPluginKagent) consumeAnswer(callID string) bool {
	if callID == "" {
		return false
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	_, own := p.answered[callID]
	delete(p.answered, callID)
	return own
}

// isReturnGate reports whether this call is the plugin's own return
// gate: the one object the plugin built and registered. A tool that
// merely carries the name — an MCP server that advertises it, a remote
// agent named after it — is a foreign tool, and it crosses the tool
// gate like every other tool.
func (p *AppaPluginKagent) isReturnGate(t tool.Tool) bool {
	gate, own := t.(*returnGate)
	return own && gate == p.returnTool
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

// remedyOverMCP runs one remedy plan on the MCP endpoint of the
// runtime.
//
// The vouch of the preceding tool_call names the trajectory, so the
// call itself carries only the quoted offer and its arguments. The
// runtime answers with the text it rendered, and a failure to reach it
// fails closed like every other post.
func (p *AppaPluginKagent) remedyOverMCP(ctx context.Context, arguments map[string]any) (string, error) {
	callCtx, cancel := context.WithTimeout(ctx, remedyTimeout)
	defer cancel()
	client := mcp.NewClient(&mcp.Implementation{Name: "appa-kagent-adk-go", Version: "1"}, nil)
	transport := &mcp.StreamableClientTransport{Endpoint: p.mcpURL, HTTPClient: p.client}
	mcpSession, err := client.Connect(callCtx, transport, nil)
	if err != nil {
		return "", failClosed("the appa /mcp endpoint did not run the remedy plan: %v", err)
	}
	defer mcpSession.Close()
	answer, err := mcpSession.CallTool(callCtx, &mcp.CallToolParams{Name: ReservedTool, Arguments: arguments})
	if err != nil {
		return "", failClosed("the appa /mcp endpoint did not run the remedy plan: %v", err)
	}
	return mcpText(answer), nil
}

// mcpText is the text of one MCP tool result, joined. An error result
// reads the same way.
func mcpText(answer *mcp.CallToolResult) string {
	if answer == nil {
		return ""
	}
	var texts []string
	for _, block := range answer.Content {
		if text, ok := block.(*mcp.TextContent); ok && text.Text != "" {
			texts = append(texts, text.Text)
		}
	}
	return strings.Join(texts, "\n")
}

func truncate(body []byte, limit int) string {
	if len(body) > limit {
		return string(body[:limit])
	}
	return string(body)
}

// -- ids ----------------------------------------------------------

// classify returns the (root, child) pair of the emitting scope as the
// session state reads now.
//
// A delegated entry carries the caller's lineage headers in session
// state: the root context id names the root trajectory, and the
// session's own id becomes the child id. A plain session is the root
// itself. The runtime main lands the headers of each request before
// its run, so the same session id classifies under a different root
// when a different parent delegates into it.
func classify(sess session.Session) trajectoryIDs {
	root := lineageRoot(sess)
	if root != "" && root != sess.ID() {
		return trajectoryIDs{rootID: root, childID: sess.ID()}
	}
	return trajectoryIDs{rootID: sess.ID()}
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

// spelling is the wire spelling of a dispatched tool; false outside the
// inventory.
func (p *AppaPluginKagent) spelling(t tool.Tool) (string, bool) {
	return p.inventory.Spelling(t.Name())
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

// opening is the event the emitting scope needs before its prompt
// crosses, or nil when none is due.
//
// A root session opens with session_start while no content has crossed
// it. A delegated entry opens with child_start while this plugin
// instance has not opened its (root, child) pair: the child session id
// can be shared by every parent that delegates into this pod, so the
// pair, not the session, decides.
//
// A re-entry of an opened pair sends no child_start. That re-entry is a
// second delegation from the same parent into the same child context.
// The runtime ended the child trajectory when its first return crossed
// the parent's gate, and the child context id can bind no second fork.
// The child then runs in the ended trajectory, the runtime refuses its
// tool calls, and the parent's return comes back withheld with "the
// spawn did not take". The log line tells that case from a child opened
// under another parent's root, which opens with its own child_start.
func (p *AppaPluginKagent) opening(sess session.Session, ids trajectoryIDs) map[string]any {
	if ids.childID != "" {
		if p.isOpened(ids) {
			log.Printf("appa: child %s re-enters under root %s with its pair already open; no child_start is sent. "+
				"A re-entry after the child's return runs in the ended child trajectory: the runtime refuses its "+
				"tool calls, and the parent's return comes back withheld", ids.childID, ids.rootID)
			return nil
		}
		log.Printf("appa: child %s opens under root %s", ids.childID, ids.rootID)
		return childStartEvent(ids.rootID, ids.childID, "")
	}
	if isFresh(sess) {
		log.Printf("appa: trajectory %s opens as a root", ids.rootID)
		return sessionStartEvent(ids.rootID)
	}
	return nil
}

// openScope sends the opening event the emitting scope needs and
// returns the return contract a fork answered with, if any. An empty
// contract is a scope that works under no words of its own.
func (p *AppaPluginKagent) openScope(ctx context.Context, sess session.Session, ids trajectoryIDs) (string, error) {
	opening := p.opening(sess, ids)
	if opening == nil {
		return "", nil
	}
	decision, err := p.post(ctx, opening)
	if err != nil {
		return "", err
	}
	contract := ""
	switch {
	case decision.Kind == "context" && ids.childID != "":
		// The return policy of the fork needs words. The child reads
		// them in front of the request its parent sent, and that
		// request stands unchanged.
		contract = decision.Text
	case decision.Kind != "ack":
		return "", failClosed("appa refused the session: %s", decision.describe())
	}
	if ids.childID != "" {
		// A child_start for a pair the runtime already holds open
		// answers ack too, so a repeat after a plugin restart changes
		// nothing.
		p.markOpened(ids)
	}
	return contract, nil
}

func (p *AppaPluginKagent) onUserMessage(ictx agent.InvocationContext, userMessage *genai.Content) (*genai.Content, error) {
	ids := p.openInvocation(ictx)
	contract, err := p.openScope(ictx, ictx.Session(), ids)
	if err != nil {
		return nil, err
	}
	decision, err := p.post(ictx, promptEvent(ids.rootID, contentText(userMessage), ids.childID))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "ack":
		if contract == "" {
			return nil, nil
		}
		return withContract(contract, userMessage), nil
	case "block":
		return nil, failClosed("appa blocked the prompt: %s", decision.Reason)
	default:
		return nil, failClosed("appa answered the prompt with %s", decision.describe())
	}
}

// withContract is the first user message of a child, with the return
// contract in front. kagent carries no side channel for the contract,
// so it rides as the first part of the message the child reads.
func withContract(text string, message *genai.Content) *genai.Content {
	contracted := &genai.Content{Role: "user", Parts: []*genai.Part{{Text: text}}}
	if message == nil {
		return contracted
	}
	if message.Role != "" {
		contracted.Role = message.Role
	}
	contracted.Parts = append(contracted.Parts, message.Parts...)
	return contracted
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
	if err := p.pingHook(ctx); err != nil {
		return nil, err
	}
	if p.inChildScope(ctx) {
		// adk-go rebuilds the request for every step, so the gate tool
		// is registered again for every step.
		p.registerReturnGate(req)
	}
	return nil, nil
}

// inChildScope reports whether this callback runs in a delegated child
// scope, the one scope whose stop the plugin holds.
//
// A callback whose run pinned no trajectory is no child scope here. The
// pin is what names a child on the go ADK: the model callbacks read a
// context that refuses Session(). A run that somehow reached the model
// unpinned therefore stops through nothing, and its parent's return
// comes back withheld — the runtime saw no child_end.
func (p *AppaPluginKagent) inChildScope(ctx agent.Context) bool {
	ids, ok := p.idsFor(ctx)
	return ok && ids.childID != ""
}

// registerReturnGate writes the gate tool into the request the flow
// builds its dispatch dict from, and declares it to the model.
//
// adk-go's own appendTools (internal/llminternal/agent_transfer.go) is
// package-private, so the plugin does what it does: the tool lands in
// req.Tools, which internal/llminternal/base_flow.go:598-608 ranges
// over to resolve a call, and its declaration lands on the first
// genai.Tool of the request config.
//
// The gate takes its slot, and never yields it. Tool preprocessing
// fills req.Tools before this callback runs, so a foreign tool of the
// gate's name is already in the slot when the plugin arrives. Leaving
// it there hands the child's whole final answer to that tool: the stop
// the plugin holds is dispatched out of req.Tools by name, no child_end
// posts, and — nothing having crossed — every later stop synthesizes
// the same call again. Overwriting the entry is what makes the
// synthesized call resolve to the gate the plugin owns.
func (p *AppaPluginKagent) registerReturnGate(req *model.LLMRequest) {
	if req == nil {
		return
	}
	if req.Tools == nil {
		req.Tools = map[string]any{}
	}
	if held, taken := req.Tools[ReturnTool]; taken && held == any(p.returnTool) {
		// The gate already holds the slot on this request, so it
		// declares itself once per request.
		return
	}
	req.Tools[ReturnTool] = p.returnTool
	dropForeignDeclaration(req)
	declaration := p.returnTool.Declaration()
	if req.Config == nil {
		req.Config = &genai.GenerateContentConfig{}
	}
	for _, declared := range req.Config.Tools {
		if declared != nil && declared.FunctionDeclarations != nil {
			declared.FunctionDeclarations = append(declared.FunctionDeclarations, declaration)
			return
		}
	}
	req.Config.Tools = append(req.Config.Tools, &genai.Tool{
		FunctionDeclarations: []*genai.FunctionDeclaration{declaration},
	})
}

// dropForeignDeclaration removes a declaration of the gate's name the
// request already carried, so the model reads exactly one appa_return
// and it is the gate's own — with the gate's own parameters, which the
// held stop fills.
func dropForeignDeclaration(req *model.LLMRequest) {
	if req.Config == nil {
		return
	}
	for _, declared := range req.Config.Tools {
		if declared == nil {
			continue
		}
		kept := declared.FunctionDeclarations[:0]
		for _, declaration := range declared.FunctionDeclarations {
			if declaration != nil && declaration.Name == ReturnTool {
				continue
			}
			kept = append(kept, declaration)
		}
		declared.FunctionDeclarations = kept
	}
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

func (p *AppaPluginKagent) afterModel(ctx agent.Context, resp *model.LLMResponse, _ error) (*model.LLMResponse, error) {
	if err := p.pingHook(ctx); err != nil {
		return nil, err
	}
	if !p.inChildScope(ctx) {
		return nil, nil
	}
	// A non-nil response replaces the model's own
	// (internal/llminternal/base_flow.go:804-819), and the flow
	// dispatches the function calls it carries.
	return p.holdTheStop(ctx.InvocationID(), resp), nil
}

// holdTheStop is the stop of a child scope: the gate call, or the value
// that crossed.
//
// A response that proposes a tool call is no stop, and neither is a
// partial one or one that carries reasoning alone. A stop before the
// return crossed becomes one call to the return gate, carrying the
// answer of the child. A stop after it carries the bytes that crossed,
// so the reply the child sends replays them.
func (p *AppaPluginKagent) holdTheStop(invocationID string, resp *model.LLMResponse) *model.LLMResponse {
	if resp == nil || resp.Partial || resp.Content == nil {
		return nil
	}
	// The reasoning of a model is no part of its answer, and a response
	// that carries reasoning alone answers nothing yet.
	var answer []*genai.Part
	for _, part := range resp.Content.Parts {
		if part == nil {
			continue
		}
		if part.FunctionCall != nil {
			return nil
		}
		if part.Thought {
			continue
		}
		answer = append(answer, part)
	}
	if len(answer) == 0 {
		return nil
	}
	if crossed, ok := p.crossedValue(invocationID); ok {
		return spokenResponse(crossed)
	}
	var texts []string
	for _, part := range answer {
		if part.Text != "" {
			texts = append(texts, part.Text)
		}
	}
	return returnCallResponse(strings.Join(texts, "\n"))
}

// returnCallResponse is the stop of a child, as one call to the return
// gate.
func returnCallResponse(text string) *model.LLMResponse {
	call := &genai.FunctionCall{Name: ReturnTool, Args: map[string]any{"text": text}}
	return modelResponse(&genai.Part{FunctionCall: call})
}

// spokenResponse is the stop of a child, carrying the bytes that
// crossed.
func spokenResponse(value string) *model.LLMResponse {
	return modelResponse(&genai.Part{Text: value})
}

func modelResponse(part *genai.Part) *model.LLMResponse {
	return &model.LLMResponse{Content: &genai.Content{Role: "model", Parts: []*genai.Part{part}}}
}

func (p *AppaPluginKagent) onModelError(ctx agent.Context, _ *model.LLMRequest, _ error) (*model.LLMResponse, error) {
	// On a live channel the original model error propagates.
	return nil, p.pingHook(ctx)
}

// -- agent scopes -------------------------------------------------

func (p *AppaPluginKagent) beforeAgent(ctx agent.Context) (*genai.Content, error) {
	agentName := ctx.AgentName()
	if p.claimScope(ctx.InvocationID(), agentName) {
		// The invocation's own agent: the prompt hook already marked
		// this turn (root), or the delegated entry did (child pod).
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
	if p.isReturnGate(t) {
		// APPA owns the return gate. Its body posts the stop of the
		// child, so the call itself crosses no tool gate. The test is
		// the plugin's own pointer: a foreign tool of that name skips
		// nothing.
		return nil, nil
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	spelled, known := p.spelling(t)
	if !known {
		// A name the inventory never saw has no spelling on the wire,
		// so nothing crosses: the gate refuses it here and the model
		// reads the refusal as the result of its call.
		log.Printf("appa: the tool %s is outside the gated inventory, so the call is refused", t.Name())
		p.answerOwn(ctx.FunctionCallID())
		return map[string]any{"result": fmt.Sprintf(outsideInventory, t.Name()), denyKey: denied}, nil
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
				p.answerOwn(ctx.FunctionCallID())
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
	call := toolCallEvent(ids.rootID, spelled, plainJSON(orEmpty(args)), ids.childID, ruling)
	decision, err := p.post(ctx, call)
	if err != nil {
		return nil, err
	}
	if offer, routed := returnOffer(decision); routed {
		decision, err = p.declareReturn(ctx, ids, call, offer, decision)
		if err != nil {
			return nil, err
		}
	}
	switch decision.Kind {
	case "allow_call", "pass_control":
		return nil, nil
	case "deny_call":
		p.rememberReviews(decision.Review)
		p.answerOwn(ctx.FunctionCallID())
		return map[string]any{"result": decision.Feedback, denyKey: denied}, nil
	default:
		return nil, failClosed("appa answered the tool call with %s", decision.describe())
	}
}

func (p *AppaPluginKagent) afterTool(ctx agent.Context, t tool.Tool, args, result map[string]any, toolErr error) (map[string]any, error) {
	if p.isReturnGate(t) {
		// The return gate reported the stop at the child_end it posted,
		// and the runtime opened no dispatch for it. Identity again: a
		// foreign tool of that name reports its result like any other.
		return nil, nil
	}
	if p.consumeAnswer(ctx.FunctionCallID()) {
		// The plugin answered this call itself — a deny, or a pending
		// review — so the runtime opened no dispatch for it. The appa
		// markers that map carries are for the model to read; the call
		// the plugin answered is what decides here, because any tool
		// can put those markers in its own result.
		return nil, nil
	}
	if toolErr != nil {
		// The go ADK runs the after-tool point on error paths too.
		// The failure already crossed at onToolError — or it is the
		// plugin's own fail-closed error from the call gate — so a
		// second report would double-count one dispatch.
		return nil, nil
	}
	ids, ok := p.idsFor(ctx)
	if !ok {
		return nil, failClosed("no trajectory is pinned for invocation %s", ctx.InvocationID())
	}
	spelled, known := p.spelling(t)
	if !known {
		return nil, failClosed("the tool %s is outside the gated inventory, and its result cannot cross", t.Name())
	}
	arguments := plainJSON(orEmpty(args))
	// A nil result with no error is a deferred or long-running tool:
	// nothing has entered attention, and the dispatch is genuinely
	// unresolved at this point.
	outcome := indeterminateOutcome()
	if result != nil {
		outcome = successOutcome(plainJSON(result))
	}
	if IsSpawn(spelled) {
		spawnedID, value := spawnReturn(result)
		decision, err := p.post(ctx, spawnResultEvent(ids.rootID, spelled, arguments, outcome, spawnedID, value, ids.childID))
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
	decision, err := p.post(ctx, toolResultEvent(ids.rootID, spelled, arguments, outcome, ids.childID))
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
	spelled, known := p.spelling(t)
	if !known {
		return nil, failClosed("the tool %s is outside the gated inventory, and its failure cannot cross", t.Name())
	}
	decision, err := p.post(ctx, toolResultEvent(ids.rootID, spelled, plainJSON(orEmpty(args)), failureOutcome(toolErr.Error()), ids.childID))
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

// -- the return gate ----------------------------------------------

// returnGate is the APPA-owned tool a child scope stops through.
//
// adk-go calls a tool through Run(agent.Context, any), the shape its
// own synthetic tools carry
// (internal/llminternal/outputschema_processor.go:137), and resolves
// the call from the request beforeModel registered this tool on.
type returnGate struct {
	plugin *AppaPluginKagent
}

func (g *returnGate) Name() string { return ReturnTool }

func (g *returnGate) Description() string {
	return "End this errand and return this text to the agent that sent it. " +
		"Call this tool with your whole final answer. " +
		"The answer of this tool tells you what reached the caller."
}

func (g *returnGate) IsLongRunning() bool { return false }

func (g *returnGate) Declaration() *genai.FunctionDeclaration {
	return &genai.FunctionDeclaration{
		Name:        g.Name(),
		Description: g.Description(),
		Parameters: &genai.Schema{
			Type: genai.TypeObject,
			Properties: map[string]*genai.Schema{
				"text": {Type: genai.TypeString, Description: "The whole final answer of this errand."},
			},
			Required: []string{"text"},
		},
	}
}

func (g *returnGate) Run(ctx agent.Context, args any) (map[string]any, error) {
	text := ""
	if fields, ok := args.(map[string]any); ok {
		text, _ = fields["text"].(string)
	}
	return g.plugin.holdTheReturn(ctx, text)
}

// holdTheReturn posts the stop of a child scope and enforces the ruling
// of the runtime. It is the body of the return gate.
//
// An ack crossed the value as the child spoke it. A child_return names
// other bytes, which the plugin echoes as a second child_end before the
// child stops with them. A block carries the reason to the model, which
// writes another final message this gate holds the same way.
func (p *AppaPluginKagent) holdTheReturn(ctx agent.Context, text string) (map[string]any, error) {
	ids, ok := p.idsFor(ctx)
	if !ok || ids.childID == "" {
		return nil, failClosed("the return gate ran outside a child scope, where no return crosses")
	}
	decision, err := p.post(ctx, childEndEvent(ids.rootID, ids.childID, text))
	if err != nil {
		return nil, err
	}
	switch decision.Kind {
	case "ack":
		return p.crossing(ctx.InvocationID(), text), nil
	case "child_return":
		echo, err := p.post(ctx, childEndEvent(ids.rootID, ids.childID, decision.Value))
		if err != nil {
			return nil, err
		}
		if echo.Kind != "ack" {
			return nil, failClosed("appa answered the returned bytes with %s", describeAnswer(echo))
		}
		return p.crossing(ctx.InvocationID(), decision.Value), nil
	case "block":
		return map[string]any{"result": fmt.Sprintf(returnBlocked, decision.Reason)}, nil
	default:
		return nil, failClosed("appa answered the child end with %s", decision.describe())
	}
}

// crossing holds what crossed for this run and tells the model to stop
// with it.
func (p *AppaPluginKagent) crossing(invocationID, value string) map[string]any {
	p.holdCrossed(invocationID, value)
	if value == "" {
		return map[string]any{"result": returnVoid}
	}
	return map[string]any{"result": fmt.Sprintf(returnCrossed, value)}
}

// describeAnswer names a decision in a fail-closed message: the
// runtime's detail, then the reason a block carries, then the kind.
func describeAnswer(decision Decision) string {
	if decision.Detail != "" {
		return decision.Detail
	}
	if decision.Reason != "" {
		return decision.Reason
	}
	return decision.Kind
}

// returnOffer is the offer of a deny that takes the return of a child
// as spoken.
//
// That offer is the bare floor of the return menu, which the runtime
// lists first. A deny with no such offer carries no return route, and
// the model reads it.
func returnOffer(decision Decision) (Offer, bool) {
	if decision.Kind != "deny_call" {
		return Offer{}, false
	}
	for _, offer := range decision.Offers {
		if offer.Returns == ReturnAsSpoken {
			return offer, true
		}
	}
	return Offer{}, false
}

// declareReturn declares the return of a held spawn, then proposes the
// call again.
//
// The runtime holds a marked spawn until this session declares what a
// return may carry. The plugin declares the bare floor itself, so the
// model reads one ordinary tool call and its result. An empty label is
// the label the parent holds now.
//
// The plugin declares once per call. A runtime that does not vouch for
// the plan hands the block back with its menu, and a second deny goes
// to the model as it stands.
func (p *AppaPluginKagent) declareReturn(
	ctx agent.Context, ids trajectoryIDs, call map[string]any, offer Offer, denial Decision,
) (Decision, error) {
	arguments := map[string]any{"offer_id": offer.OfferID, "label": map[string]any{}}
	vouch, err := p.post(ctx, toolCallEvent(ids.rootID, ControlTool, arguments, ids.childID, ""))
	if err != nil {
		return Decision{}, err
	}
	if vouch.Kind != "pass_control" {
		log.Printf("appa: appa answered the return declaration %s with %s, so the block goes to the model",
			offer.OfferID, vouch.Kind)
		return denial, nil
	}
	if _, err := p.remedyCall(ctx, arguments); err != nil {
		return Decision{}, err
	}
	return p.post(ctx, call)
}

// -- turn ends ----------------------------------------------------

// afterRun ends the turn under the ids the run open pinned, so the
// turn_end lands on the (root, child) pair the prompt and the tool
// calls of that run carried. A run no open pinned classifies from the
// session as it reads now.
func (p *AppaPluginKagent) afterRun(ictx agent.InvocationContext) {
	ids, pinned := p.pinnedIDs(ictx.InvocationID())
	if !pinned {
		ids = classify(ictx.Session())
	}
	p.postQuiet(turnEndEvent(ids.rootID, ids.childID))
	p.releaseScope(ictx.InvocationID())
	p.closeInvocation(ictx.InvocationID())
	p.dropCrossed(ictx.InvocationID())
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
