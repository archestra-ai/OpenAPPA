package appakagentadk

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"slices"
	"sort"
	"strings"
	"sync"

	kagentagent "github.com/kagent-dev/kagent/go/adk/pkg/agent"
	"github.com/kagent-dev/kagent/go/adk/pkg/sts"
	"github.com/kagent-dev/kagent/go/api/adk"
	"google.golang.org/adk/v2/agent"
	"google.golang.org/adk/v2/model"
	"google.golang.org/adk/v2/tool"
	"google.golang.org/adk/v2/tool/toolutils"
	"google.golang.org/genai"
)

type observedTool struct {
	Name string `json:"name"`
	Tool string `json:"tool"`
}
type inventorySource struct {
	Server  string          `json:"server"`
	Status  DiscoveryStatus `json:"status"`
	Dynamic bool            `json:"dynamic"`
	Detail  string          `json:"detail,omitempty"`
}
type observedInventory struct {
	Tools   []observedTool    `json:"tools"`
	Sources []inventorySource `json:"sources"`
}
type toolCheck struct {
	Tool   string `json:"tool"`
	Status string `json:"status"`
	Reason string `json:"reason"`
}
type validationReport struct {
	Tools       []toolCheck    `json:"tools"`
	Errors      []string       `json:"errors"`
	Accepted    []observedTool `json:"accepted_tools"`
	ActorOpened bool           `json:"actor_opened"`
}

type mcpConnection struct {
	params    mcpServerParams
	names     []string
	approvals map[string]bool
	server    string
	control   bool
}
type mcpRun struct {
	mu           sync.Mutex
	sets         []*discoveredToolset
	observations []toolsetObservation
	tools        [][]tool.Tool
	inventory    observedInventory
	names        Inventory
	opened       bool
	selected     map[string]*discoveredMCPTool
}

// MCPDiscovery is the invocation-owned MCP tool source installed by the gated
// runner. It is not callable and does not add a model-visible command.
type MCPDiscovery struct {
	connections []mcpConnection
	mu          sync.Mutex
	runs        map[string]*mcpRun
	plugin      *AppaPluginKagent
}

func (*MCPDiscovery) Name() string        { return "appa_mcp_tools" }
func (*MCPDiscovery) Description() string { return "" }
func (*MCPDiscovery) IsLongRunning() bool { return false }

func newMCPDiscovery(config *adk.AgentConfig, stsPlugin *sts.TokenPropagationPlugin) (*MCPDiscovery, error) {
	// The gated entrypoint appends this reserved source after checking user
	// configuration. Never interpret an arbitrary last source as runtime tools.
	if config == nil || len(config.HttpTools) == 0 {
		return nil, failClosed("gated runner requires its reserved runtime toolset")
	}
	reserved := config.HttpTools[len(config.HttpTools)-1].Tools
	if !slices.Equal(reserved, []string{ReservedTool}) && !slices.Equal(reserved, RuntimeTools) {
		return nil, failClosed("gated runner has no reserved runtime toolset")
	}
	seen := make(map[string]bool)
	check := func(endpoint string) error {
		id, err := mcpSourceID(endpoint)
		if err != nil {
			return err
		}
		if seen[id] {
			return failClosed("MCP endpoint is configured more than once; combine its tool filters and approval requirements")
		}
		seen[id] = true
		return nil
	}
	for _, c := range config.HttpTools {
		if err := check(c.Params.Url); err != nil {
			return nil, err
		}
	}
	for _, c := range config.SseTools {
		if err := check(c.Params.Url); err != nil {
			return nil, err
		}
	}
	result := &MCPDiscovery{runs: make(map[string]*mcpRun)}
	var provider DynamicHeaderProvider
	if stsPlugin != nil {
		provider = stsPlugin.HeaderProvider
	}
	propagate := strings.EqualFold(os.Getenv("KAGENT_PROPAGATE_TOKEN"), "true")
	appendConnection := func(params mcpServerParams, names, approvals []string, control bool) {
		params.PropagateToken, params.HeaderProvider = propagate, provider
		required := make(map[string]bool)
		for _, name := range approvals {
			required[name] = true
		}
		server, _ := mcpSourceID(params.URL) // Every endpoint was checked above.
		result.connections = append(result.connections, mcpConnection{params: params, names: names, approvals: required, server: server, control: control})
	}
	for index, c := range config.HttpTools {
		appendConnection(mcpServerParams{URL: c.Params.Url, Headers: c.Params.Headers, AllowedHeaders: c.AllowedHeaders, ServerType: "http", Timeout: c.Params.Timeout, SseReadTimeout: c.Params.SseReadTimeout, TLSInsecureSkipVerify: c.Params.TLSInsecureSkipVerify, TLSCACertPath: c.Params.TLSCACertPath, TLSDisableSystemCAs: c.Params.TLSDisableSystemCAs}, c.Tools, c.RequireApproval, index == len(config.HttpTools)-1)
	}
	for _, c := range config.SseTools {
		appendConnection(mcpServerParams{URL: c.Params.Url, Headers: c.Params.Headers, AllowedHeaders: c.AllowedHeaders, ServerType: "sse", Timeout: c.Params.Timeout, SseReadTimeout: c.Params.SseReadTimeout, TLSInsecureSkipVerify: c.Params.TLSInsecureSkipVerify, TLSCACertPath: c.Params.TLSCACertPath, TLSDisableSystemCAs: c.Params.TLSDisableSystemCAs}, c.Tools, c.RequireApproval, false)
	}
	return result, nil
}

func (d *MCPDiscovery) run(id string) *mcpRun {
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.runs[id] == nil {
		d.runs[id] = &mcpRun{sets: make([]*discoveredToolset, len(d.connections)), observations: make([]toolsetObservation, len(d.connections)), tools: make([][]tool.Tool, len(d.connections))}
	}
	return d.runs[id]
}

func (d *MCPDiscovery) close(id string) {
	d.mu.Lock()
	run := d.runs[id]
	delete(d.runs, id)
	d.mu.Unlock()
	if run == nil {
		return
	}
	run.mu.Lock()
	defer run.mu.Unlock()
	run.opened = false
	for _, observation := range run.observations {
		if observation.session != nil {
			observation.session.Close()
		}
	}
}

var errFamilyNotOpened = errors.New("APPA family has not opened")

func (d *MCPDiscovery) validate(ctx context.Context, ids trajectoryIDs, inventory observedInventory, pinned bool) (validationReport, error) {
	ctx, cancel := context.WithTimeout(ctx, gatedTimeout)
	defer cancel()
	request := map[string]any{"protocol": Protocol, "adapter": Adapter, "inventory": inventory}
	if pinned {
		request["root_id"] = ids.rootID
		if ids.childID != "" {
			request["child_id"] = ids.childID
		}
	}
	body, err := json.Marshal(request)
	if err != nil {
		return validationReport{}, err
	}
	req, err := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(d.plugin.hookURL, "/hook")+"/validate", strings.NewReader(string(body)))
	if err != nil {
		return validationReport{}, err
	}
	req.Header.Set("Content-Type", "application/json")
	response, err := d.plugin.client.Do(req)
	if err != nil {
		return validationReport{}, failClosed("MCP inventory validation is unavailable")
	}
	defer response.Body.Close()
	if pinned && response.StatusCode == http.StatusNotFound {
		return validationReport{}, errFamilyNotOpened
	}
	if response.StatusCode != http.StatusOK {
		return validationReport{}, failClosed("MCP inventory validation returned HTTP %d", response.StatusCode)
	}
	var report validationReport
	data, err := io.ReadAll(io.LimitReader(response.Body, discoveryMaxBytes+1))
	if err != nil || len(data) > discoveryMaxBytes || json.Unmarshal(data, &report) != nil || report.Tools == nil || report.Errors == nil || report.Accepted == nil {
		return report, failClosed("invalid MCP inventory validation response")
	}
	checked := make(map[string]string)
	for _, check := range report.Tools {
		if check.Tool == "" || (check.Status != "valid" && check.Status != "invalid" && check.Status != "unknown") {
			return report, failClosed("invalid MCP inventory validation status")
		}
		checked[check.Tool] = check.Status
	}
	for _, observed := range inventory.Tools {
		if observed.Tool == ControlTool {
			continue // Runtime control is not an authored policy contract.
		}
		if checked[observed.Name] != "valid" && checked[observed.Name] != "invalid" {
			return report, failClosed("MCP inventory validation omitted a known tool")
		}
	}
	return report, nil
}

// prepare is called before the opening prompt and on each model request. A
// configured source failing to enumerate is unknown; invalid new tools are
// isolated after opening. Runtime admission still reserves every accepted name.
func (d *MCPDiscovery) prepare(ctx agent.ReadonlyContext, ids trajectoryIDs, pinned bool) (*mcpRun, error) {
	run := d.run(ctx.InvocationID())
	run.mu.Lock()
	defer run.mu.Unlock()
	// A fresh host process may be continuing an existing APPA trajectory. Only
	// an explicit missing-family response permits using the serving policy.
	previous, err := d.validate(ctx, ids, observedInventory{Tools: []observedTool{}, Sources: []inventorySource{}}, true)
	if errors.Is(err, errFamilyNotOpened) && !pinned && ids.childID == "" {
		previous, err = d.validate(ctx, ids, observedInventory{Tools: []observedTool{}, Sources: []inventorySource{}}, false)
	} else if err == nil {
		pinned = true
		if previous.ActorOpened {
			run.opened = true
		}
	}
	if err != nil {
		return nil, err
	}
	limit := make(chan struct{}, 4)
	errors := make([]error, len(d.connections))
	var workers sync.WaitGroup
	for index, connection := range d.connections {
		workers.Add(1)
		go func(index int, connection mcpConnection) {
			defer workers.Done()
			select {
			case limit <- struct{}{}:
				defer func() { <-limit }()
			case <-ctx.Done():
				errors[index] = ctx.Err()
				return
			}
			if run.sets[index] == nil {
				transport, err := createTransport(ctx, connection.params)
				if err != nil {
					errors[index] = fmt.Errorf("MCP source %s has invalid transport configuration", connection.server)
					return
				}
				set, err := newDiscoveredToolset(transport, connection.names)
				if err != nil {
					errors[index] = err
					return
				}
				run.sets[index] = set
			}
			found, observation, err := run.sets[index].discover(ctx)
			if observation.session != nil {
				run.observations[index].session = observation.session
			}
			if err != nil {
				errors[index] = err
				run.observations[index].discovery = Discovery{Status: DiscoveryUnavailable, Detail: "MCP metadata update is invalid; previous tools retained"}
				return
			}
			run.observations[index] = observation
			if observation.discovery.Status != DiscoveryUnavailable {
				if observation.discovery.Status == DiscoveryPartial {
					retained := make(map[string]tool.Tool)
					for _, candidate := range run.tools[index] {
						retained[candidate.Name()] = candidate
					}
					for _, candidate := range found {
						retained[candidate.Name()] = candidate
					}
					if len(retained) > discoveryMaxTools {
						errors[index] = failClosed("MCP partial inventory exceeds its tool limit")
						return
					}
					found = make([]tool.Tool, 0, len(retained))
					for _, candidate := range retained {
						found = append(found, candidate)
					}
				}
				run.tools[index] = found
			}
		}(index, connection)
	}
	workers.Wait()
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if !run.opened {
		for _, err := range errors {
			if err != nil {
				return nil, err
			}
		}
	}
	for index, err := range errors {
		if err != nil {
			log.Printf("appa: MCP source %s metadata update refused; previous tools retained", d.connections[index].server)
		}
	}
	inventory := observedInventory{Tools: []observedTool{}, Sources: []inventorySource{}}
	for name, spelling := range d.plugin.inventory.spellings {
		inventory.Tools = append(inventory.Tools, observedTool{Name: name, Tool: spelling})
	}
	candidates := make(map[string][]*discoveredMCPTool)
	for index, connection := range d.connections {
		observation := run.observations[index].discovery
		if observation.Status == "" {
			observation.Status = DiscoveryUnavailable
		}
		if !connection.control {
			inventory.Sources = append(inventory.Sources, inventorySource{Server: connection.server, Status: observation.Status, Dynamic: true, Detail: observation.Detail})
		}
		for _, candidate := range run.tools[index] {
			spelling := MCPSpelling(connection.server, candidate.Name())
			if connection.control {
				var ok bool
				spelling, ok = d.plugin.inventory.Spelling(candidate.Name())
				if !ok {
					return nil, failClosed("unregistered runtime control tool")
				}
			}
			runnable, ok := candidate.(mcpRunnable)
			if !ok {
				return nil, failClosed("MCP tool has no executable declaration")
			}
			candidates[candidate.Name()] = append(candidates[candidate.Name()], &discoveredMCPTool{mcpRunnable: runnable, spelling: spelling, approvals: connection.approvals})
		}
	}
	// Fetch durable reservations first: a restarted plugin cannot let a new
	// source claim a name already owned by another source in this actor.
	accepted := make(map[string]string)
	for _, tool := range previous.Accepted {
		accepted[tool.Name] = tool.Tool
	}
	selected := make(map[string]*discoveredMCPTool)
	for name, choices := range candidates {
		static, isStatic := d.plugin.inventory.Spelling(name)
		for _, candidate := range choices {
			if isStatic && candidate.spelling != static {
				continue
			}
			if old, exists := accepted[name]; exists && old != candidate.spelling {
				continue
			}
			if selected[name] != nil {
				if !run.opened {
					return nil, failClosed("MCP tool name %s is ambiguous", name)
				}
				delete(selected, name)
				break
			}
			selected[name] = candidate
		}
		if !run.opened && selected[name] == nil {
			return nil, failClosed("MCP tool name %s conflicts with an existing identity", name)
		}
	}
	for name, candidate := range selected {
		if _, static := d.plugin.inventory.Spelling(name); !static {
			inventory.Tools = append(inventory.Tools, observedTool{Name: name, Tool: candidate.spelling})
		}
	}
	sort.Slice(inventory.Tools, func(i, j int) bool { return inventory.Tools[i].Name < inventory.Tools[j].Name })
	report, err := d.validate(ctx, ids, inventory, pinned)
	if err != nil {
		return nil, err
	}
	if len(report.Errors) > 0 {
		return nil, failClosed("MCP inventory is invalid: %s", strings.Join(report.Errors, "; "))
	}
	invalid := make(map[string]bool)
	for _, check := range report.Tools {
		if check.Status == "invalid" {
			// Missing coverage disables this tool, not the conversation.
			invalid[check.Tool] = true
			delete(selected, check.Tool)
		}
	}
	filtered := inventory.Tools[:0]
	run.names = Inventory{spellings: make(map[string]string), names: make(map[string]string)}
	for _, tool := range inventory.Tools {
		if !invalid[tool.Name] {
			filtered = append(filtered, tool)
			run.names.spellings[tool.Name] = tool.Tool
			run.names.names[tool.Tool] = tool.Name
		}
	}
	inventory.Tools = filtered
	run.inventory = inventory
	run.selected = selected
	return run, nil
}

type mcpRunnable interface {
	tool.Tool
	Declaration() *genai.FunctionDeclaration
	Run(agent.Context, any) (map[string]any, error)
}
type discoveredMCPTool struct {
	mcpRunnable
	spelling  string
	approvals map[string]bool
}

func (t *discoveredMCPTool) ProcessRequest(ctx agent.Context, req *model.LLMRequest) error {
	return toolutils.PackTool(req, t)
}
func (t *discoveredMCPTool) Run(ctx agent.Context, args any) (map[string]any, error) {
	values, ok := args.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("MCP arguments must be an object")
	}
	result, err := kagentagent.MakeApprovalCallback(t.approvals)(ctx, t, values)
	if result != nil || err != nil {
		return result, err
	}
	return t.mcpRunnable.Run(ctx, args)
}

func (d *MCPDiscovery) ProcessRequest(ctx agent.Context, req *model.LLMRequest) error {
	if d.plugin == nil {
		return failClosed("MCP discovery is not attached to the APPA plugin")
	}
	ids, ok := d.plugin.idsFor(ctx)
	if !ok {
		return failClosed("MCP discovery has no trajectory scope")
	}
	run, err := d.prepare(ctx, ids, true)
	if err != nil {
		return err
	}
	run.mu.Lock()
	defer run.mu.Unlock()
	names := make([]string, 0, len(run.selected))
	for name := range run.selected {
		names = append(names, name)
	}
	sort.Strings(names)
	for _, name := range names {
		if err := toolutils.PackTool(req, run.selected[name]); err != nil {
			return err
		}
	}
	apps := make(map[string]bool)
	for _, observation := range run.observations {
		for _, candidate := range observation.discovery.Tools {
			ui, _ := candidate.Meta["ui"].(map[string]any)
			uri, _ := ui["resourceUri"].(string)
			if uri == "" {
				uri, _ = candidate.Meta["ui/resourceUri"].(string)
			}
			if uri != "" && run.selected[candidate.Name] != nil {
				apps[candidate.Name] = true
			}
		}
	}
	if len(apps) > 0 {
		_, err := kagentagent.MakeMCPAppModelResultCallback(apps)(ctx, req)
		if err != nil {
			return err
		}
	}
	return nil
}
