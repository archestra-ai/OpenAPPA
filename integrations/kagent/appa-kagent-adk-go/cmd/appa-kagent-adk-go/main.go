// The appa-kagent-adk-go runtime main.
//
// This binary is a drop-in replacement for the stock kagent go runtime
// image. It replays the stock runtime construction — adk/cmd/main.go at
// kagent tag go/v0.10.0-rc4 — through the exported go/adk packages only
// (no kagent internal/ imports).
//
// The agent container's APPA_ENABLED selects what this image runs. The
// knob is a closed set. Unset, empty and "false" serve the stock
// runtime. "true" gates the agent. Any other value refuses the start,
// because a typo must never disable the gate in silence.
//
// While the knob is off the image builds and serves exactly what the
// stock runtime builds: no delta below applies, and the agent runs
// ungated. The image ignores APPA_RUNTIME_URL there. So an operator can
// set this image as the cluster default agent image, or on one Agent,
// and every agent that leaves the knob off keeps stock behavior.
//
// While the knob is on the image needs APPA_RUNTIME_URL. A gated agent
// that names no runtime refuses the start, because a gate that cannot
// reach its runtime must fail closed.
//
// With the knob on the image adds exactly six deltas:
//
//  1. It appends the reserved-tool toolset to the rendered config: a
//     streamable-HTTP MCP toolset at $APPA_RUNTIME_URL/mcp serving
//     execute_remedy_plan, through the same HttpTools path kagent uses
//     for CRD MCP tools.
//  2. It registers AppaPluginKagent after the stock plugins in
//     runner.PluginConfig — the registration point kagent itself uses
//     (go/adk/pkg/runner/adapter.go).
//  3. It fills the OpenAI model's reasoning_effort from
//     APPA_KAGENT_OPENAI_REASONING_EFFORT when the rendered config
//     leaves it unset. The v1alpha2 ModelConfig enum has no "none",
//     which some OpenAI models require for function tools on chat
//     completions; a value the CRD set wins. This fill is an OpenAPPA
//     delta, so an ungated agent never gets it and behaves as it does
//     on the stock image.
//  4. It lands the inbound lineage headers in session state on every
//     session Get and Create, so a delegated entry classifies as a
//     child — the python executor persists them as a header_update
//     event; the go controller session service folds no state back.
//  5. It drops the plugin's own pending-review response from the A2A
//     task while a person rules on a remedy, so the task history stays
//     python-shaped and the kagent dashboard renders the approval card.
//  6. It refuses a rendered config this image cannot run as declared.
//     That is in-process sub_agents or agent_plugins, any other
//     top-level key outside the rc4 schema, a declared tool named
//     appa_return, and a value the Go runtime would ignore
//     (execute_code true, a non-null context_config). The plugin's
//     return gate owns appa_return, so a config that declares the name
//     would collide with the gate at dispatch.
//     The stock loader drops such keys, ignores such values, and runs
//     a narrower agent than declared. The guard decodes the config the
//     runtime runs from the bytes it checked. The main then loads only
//     the agent card from disk (configguard.go). An ungated agent gets
//     the stock loader and no guard.
//
// Everything else keeps the stock contract: --host/--port/--filepath
// args, config.json + agent-card.json under the config dir (or the
// KAGENT_CONFIG_JSON / KAGENT_AGENT_CARD_JSON env delivery), A2A on
// the given port, and readiness at /.well-known/agent-card.json.
package main

import (
	"cmp"
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"

	a2atype "github.com/a2aproject/a2a-go/a2a"
	"github.com/a2aproject/a2a-go/a2asrv/eventqueue"
	"github.com/go-logr/logr"
	"github.com/go-logr/zapr"
	"github.com/kagent-dev/kagent/go/adk/pkg/a2a"
	"github.com/kagent-dev/kagent/go/adk/pkg/app"
	"github.com/kagent-dev/kagent/go/adk/pkg/auth"
	"github.com/kagent-dev/kagent/go/adk/pkg/config"
	kagentmemory "github.com/kagent-dev/kagent/go/adk/pkg/memory"
	runnerpkg "github.com/kagent-dev/kagent/go/adk/pkg/runner"
	"github.com/kagent-dev/kagent/go/adk/pkg/session"
	"github.com/kagent-dev/kagent/go/adk/pkg/telemetry"
	"github.com/kagent-dev/kagent/go/api/adk"
	"go.uber.org/zap"
	"go.uber.org/zap/zapcore"

	"github.com/a2aproject/a2a-go/a2asrv"
	appakagentadk "github.com/archestra-ai/OpenAPPA/integrations/kagent/appa-kagent-adk-go"
	adkrunner "google.golang.org/adk/v2/runner"
	adksession "google.golang.org/adk/v2/session"
	"log"
	"reflect"
	"runtime/debug"
)

func setupLogger(logLevel string) (logr.Logger, *zap.Logger) {
	var zapLevel zapcore.Level
	switch strings.ToLower(logLevel) {
	case "debug":
		zapLevel = zapcore.DebugLevel
	case "info":
		zapLevel = zapcore.InfoLevel
	case "warn", "warning":
		zapLevel = zapcore.WarnLevel
	case "error":
		zapLevel = zapcore.ErrorLevel
	default:
		zapLevel = zapcore.InfoLevel
	}

	zapConfig := zap.NewProductionConfig()
	zapConfig.Level = zap.NewAtomicLevelAt(zapLevel)
	zapConfig.EncoderConfig.TimeKey = "timestamp"
	zapConfig.EncoderConfig.EncodeTime = zapcore.ISO8601TimeEncoder

	zapLogger, err := zapConfig.Build()
	if err != nil {
		devConfig := zap.NewDevelopmentConfig()
		devConfig.Level = zap.NewAtomicLevelAt(zapLevel)
		zapLogger, _ = devConfig.Build()
	}
	logger := zapr.NewLogger(zapLogger)
	logger.Info("Logger initialized", "level", logLevel)
	return logger, zapLogger
}

// appaEnabledEnv names the one knob that turns OpenAPPA on. Both
// runtime images and the quickstart entrypoint read this variable.
const appaEnabledEnv = "APPA_ENABLED"

// runtimeURLEnv names the OpenAPPA runtime a gated agent talks to.
const runtimeURLEnv = "APPA_RUNTIME_URL"

// appaMode is the closed set of modes this image serves.
type appaMode int

const (
	// appaOff serves exactly what the stock kagent runtime serves.
	appaOff appaMode = iota
	// appaOn adds every OpenAPPA delta.
	appaOn
)

// knobRefusal reports an APPA_ENABLED value outside the closed set. It
// carries the value the operator wrote, so the diagnostic names it.
type knobRefusal struct {
	value string
}

func (r *knobRefusal) Error() string {
	return fmt.Sprintf("%s is %q, which is not a value this image knows. Set it to true, or to false, or leave it unset",
		appaEnabledEnv, r.value)
}

// appaModeFromEnv reads the knob. Unset, empty and "false" select
// appaOff, the default of this image. "true" selects appaOn. Any other
// value returns a *knobRefusal, and the caller ends the start. The
// match trims space and folds case.
func appaModeFromEnv() (appaMode, error) {
	value := os.Getenv(appaEnabledEnv)
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", "false":
		return appaOff, nil
	case "true":
		return appaOn, nil
	default:
		return appaOff, &knobRefusal{value: value}
	}
}

// errMissingRuntimeURL refuses a gated start that names no runtime. A
// gate that cannot reach its runtime must fail closed.
var errMissingRuntimeURL = fmt.Errorf("%s is true but %s is not set. A gated agent needs the OpenAPPA runtime URL",
	appaEnabledEnv, runtimeURLEnv)

// gating is the one choice this image makes at startup: gate this agent
// through an OpenAPPA runtime, or serve the stock kagent runtime. The
// agent container's APPA_ENABLED makes the choice. Every delta asks
// this value, and nothing else decides.
type gating struct {
	// mode is the knob, read once.
	mode appaMode
	// runtimeURL is the OpenAPPA runtime the deltas talk to. It is
	// non-empty exactly while the mode is appaOn.
	runtimeURL string
	// ignoredRuntimeURL carries a runtime URL an operator set while the
	// knob is off. No delta reads it. The startup line names it,
	// because that pair is an operator mistake.
	ignoredRuntimeURL string
}

// gatingFromEnv reads the choice from the agent container's env. The
// knob alone decides, and an image an operator merely points kagent at
// serves the stock runtime, because this image is a drop-in replacement
// for the stock runtime image.
func gatingFromEnv() (gating, error) {
	mode, err := appaModeFromEnv()
	if err != nil {
		return gating{}, err
	}
	runtimeURL := strings.TrimSpace(os.Getenv(runtimeURLEnv))
	if mode == appaOff {
		return gating{mode: appaOff, ignoredRuntimeURL: runtimeURL}, nil
	}
	if runtimeURL == "" {
		return gating{}, errMissingRuntimeURL
	}
	return gating{mode: appaOn, runtimeURL: runtimeURL}, nil
}

// enabled reports whether the OpenAPPA deltas apply.
func (g gating) enabled() bool {
	return g.mode == appaOn
}

// gatedStartupMessage names the mode of a gated agent at startup.
const gatedStartupMessage = "This agent runs gated by the OpenAPPA runtime"

// ungatedStartupMessage names the mode of an agent the knob leaves off.
// It goes to the zap logger at WARN, because logr carries no Warn
// helper and both loggers write the same stream.
const ungatedStartupMessage = "APPA_ENABLED is not true. This agent runs UNGATED as the stock kagent runtime, " +
	"and no OpenAPPA policy applies. Set APPA_ENABLED=true to gate this agent."

// ignoredRuntimeURLMessage names an operator mistake: a runtime URL on
// an agent the knob leaves off. The image ignores that URL.
const ignoredRuntimeURLMessage = "APPA_RUNTIME_URL is set, and this image ignores it, because APPA_ENABLED is not true."

// logStartupMode names the mode of this start, once.
func logStartupMode(gate gating, logger logr.Logger, zapLogger *zap.Logger) {
	if gate.enabled() {
		logger.Info(gatedStartupMessage, "runtimeURL", gate.runtimeURL)
		return
	}
	zapLogger.Warn(ungatedStartupMessage)
	if gate.ignoredRuntimeURL != "" {
		zapLogger.Warn(ignoredRuntimeURLMessage)
	}
}

// reasoningEffortEnv names the image setting that fills the OpenAI
// model's reasoning_effort when the rendered config leaves it unset.
const reasoningEffortEnv = "APPA_KAGENT_OPENAI_REASONING_EFFORT"

// withReasoningEffort fills ReasoningEffort on an OpenAI model from the
// image env. The v1alpha2 ModelConfig enum admits minimal, low, medium
// and high — and no "none", which some OpenAI models require for
// function tools on chat completions. A value the CRD set wins, and a
// model of another type is untouched.
func withReasoningEffort(agentConfig *adk.AgentConfig, effort string) {
	effort = strings.TrimSpace(effort)
	model, ok := agentConfig.Model.(*adk.OpenAI)
	if effort == "" || !ok || model.ReasoningEffort != nil {
		return
	}
	model.ReasoningEffort = &effort
}

// withLineageHeaders decorates a session service with delta 4 while
// the knob is on. While the knob is off it returns the service the
// stock construction built, and a nil service stays nil.
func withLineageHeaders(gate gating, service adksession.Service) adksession.Service {
	if !gate.enabled() || service == nil {
		return service
	}
	return lineageSessionService{service}
}

// lineageSessionService lands the inbound A2A lineage headers in the
// session's state under the python-shaped "headers" key on every Get
// and Create, so AppaPluginKagent classifies a delegated entry as a
// child exactly as it does on the python runtime. a2a-go stashes the
// request headers in the call context before dispatch; the python
// executor persists them as a header_update event, which the go
// controller session service would not fold back on Get, so this
// decorator sets them per request instead. Stateless; it persists
// nothing.
type lineageSessionService struct {
	adksession.Service
}

// lineageHeaders are the two headers kagent's remote agent tool stamps
// on a delegated call.
var lineageHeaders = []string{"x-kagent-root-context-id", "x-kagent-parent-context-id"}

func (s lineageSessionService) Create(ctx context.Context, req *adksession.CreateRequest) (*adksession.CreateResponse, error) {
	resp, err := s.Service.Create(ctx, req)
	if err == nil && resp != nil && resp.Session != nil {
		landLineageHeaders(ctx, resp.Session)
	}
	return resp, err
}

func (s lineageSessionService) Get(ctx context.Context, req *adksession.GetRequest) (*adksession.GetResponse, error) {
	resp, err := s.Service.Get(ctx, req)
	if err == nil && resp != nil && resp.Session != nil {
		landLineageHeaders(ctx, resp.Session)
	}
	return resp, err
}

// landLineageHeaders sets the lineage headers the request carried on the
// session's own state map — the concrete session the inner service
// returned, so AppendEvent's type asserts keep holding.
func landLineageHeaders(ctx context.Context, sess adksession.Session) {
	// Bookkeeping, never a gate: a session the inner service returned as
	// a typed nil, or a state it cannot set, must not take the run down.
	if sess == nil || (reflect.ValueOf(sess).Kind() == reflect.Ptr && reflect.ValueOf(sess).IsNil()) {
		return
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			log.Printf("appa: landing the lineage headers panicked (ignored): %v\n%s", recovered, debug.Stack())
		}
	}()
	callCtx, ok := a2asrv.CallContextFrom(ctx)
	if !ok || callCtx.RequestMeta() == nil {
		return
	}
	headers := map[string]any{}
	for _, name := range lineageHeaders {
		if values, ok := callCtx.RequestMeta().Get(name); ok && len(values) > 0 && values[0] != "" {
			headers[name] = values[0]
		}
	}
	if len(headers) == 0 {
		return
	}
	state := sess.State()
	if state == nil {
		return
	}
	_ = state.Set("headers", headers)
}

// remedyCallTimeoutSeconds is the reserved toolset's request timeout. A
// remedy execution holds execute_remedy_plan open for as long as its
// plan takes — a sanitizer's model call, a URL authority parked at a
// remote approval board, the runtime's whole consult window — so the
// timeout must outlast the runtime's [externals] consult timeout.
const remedyCallTimeoutSeconds = 300.0

// withReservedToolset appends the engine's remedy-execution toolset to
// the rendered config: the reserved execute_remedy_plan tool over
// streamable HTTP at $APPA_RUNTIME_URL/mcp, built by the same stock
// HttpTools path as every CRD MCP toolset.
func withReservedToolset(agentConfig *adk.AgentConfig, runtimeURL string) {
	timeout := remedyCallTimeoutSeconds
	agentConfig.HttpTools = append(agentConfig.HttpTools, adk.HttpMcpServerConfig{
		Params: adk.StreamableHTTPConnectionParams{
			Url:            strings.TrimRight(runtimeURL, "/") + "/mcp",
			Timeout:        &timeout,
			SseReadTimeout: &timeout,
			Headers:        map[string]string{},
		},
		Tools: []string{appakagentadk.ReservedTool},
	})
}

// applyConfigDeltas changes the rendered config before the stock
// builder reads it: deltas 1 and 3. The reserved-tool toolset joins
// the config, so the stock HttpTools path constructs it like every
// other MCP toolset. The image env then fills the OpenAI reasoning
// effort the CRD cannot express. While the knob is off this function
// changes nothing, so the stock builder gets the stock config.
func applyConfigDeltas(gate gating, agentConfig *adk.AgentConfig, logger logr.Logger) {
	if !gate.enabled() {
		return
	}
	withReservedToolset(agentConfig, gate.runtimeURL)
	logger.Info("Wired the appa reserved-tool toolset", "url", gate.runtimeURL)
	withReasoningEffort(agentConfig, os.Getenv(reasoningEffortEnv))
}

// appendAppaPlugin registers AppaPluginKagent after the stock plugins:
// delta 2. Order is load-bearing. ADK stops a callback chain at the
// first non-nil answer, and no stock plugin answers a gated callback.
// So a plugin appended last never short-circuits a gate. While the
// knob is off the plugin list stays the stock list. The inventory is
// the one the config guard built from the rendered config.
func appendAppaPlugin(gate gating, runnerConfig *adkrunner.Config, inventory appakagentadk.Inventory, logger logr.Logger) error {
	if !gate.enabled() {
		return nil
	}
	appaPlugin, err := appakagentadk.New(appakagentadk.Config{
		RuntimeURL: gate.runtimeURL,
		Inventory:  inventory,
	})
	if err != nil {
		return fmt.Errorf("failed to create AppaPluginKagent: %w", err)
	}
	adkPlugin, err := appaPlugin.ADKPlugin()
	if err != nil {
		return fmt.Errorf("failed to wire AppaPluginKagent into the ADK plugin surface: %w", err)
	}
	runnerConfig.PluginConfig.Plugins = append(runnerConfig.PluginConfig.Plugins, adkPlugin)
	logger.Info("Registered AppaPluginKagent", "runtimeURL", gate.runtimeURL, "inventory", inventory.Len())
	return nil
}

func main() {
	logLevel := flag.String("log-level", cmp.Or(os.Getenv("LOG_LEVEL"), "info"), "Set the logging level (debug, info, warn, error)")
	host := flag.String("host", "", "Set the host address to bind to (default: empty, binds to all interfaces)")
	portFlag := flag.String("port", "", "Set the port to listen on (overrides PORT environment variable)")
	filepathFlag := flag.String("filepath", "", "Set the config directory path (overrides CONFIG_DIR environment variable)")
	flag.Parse()

	logger, zapLogger := setupLogger(*logLevel)
	defer func() {
		_ = zapLogger.Sync()
	}()

	// The one appa choice. This image makes it here, and every delta
	// below asks the same value.
	gate, err := gatingFromEnv()
	if err != nil {
		logger.Error(err, "Refusing to start")
		os.Exit(1)
	}
	logStartupMode(gate, logger, zapLogger)

	port := *portFlag
	if port == "" {
		port = os.Getenv("PORT")
	}

	configDir := *filepathFlag
	if configDir == "" {
		configDir = os.Getenv("CONFIG_DIR")
	}
	if configDir == "" {
		configDir = "/config"
	}

	kagentURL := os.Getenv("KAGENT_URL")

	if err := config.MaterializeFromEnv(configDir); err != nil {
		logger.Error(err, "Failed to materialize agent config from environment", "configDir", configDir)
		os.Exit(1)
	}

	agentConfig, inventory, agentCard := loadAgentConfigs(gate, configDir, logger)
	logger.Info("Loaded agent config", "configDir", configDir)
	logger.Info("Agent configuration",
		"model", agentConfig.Model.GetType(),
		"stream", agentConfig.GetStream(),
		"httpTools", len(agentConfig.HttpTools),
		"sseTools", len(agentConfig.SseTools),
		"remoteAgents", len(agentConfig.RemoteAgents))

	applyConfigDeltas(gate, agentConfig, logger)

	kagentName := os.Getenv("KAGENT_NAME")
	kagentNamespace := os.Getenv("KAGENT_NAMESPACE")

	// Derive app name from env or agent card.
	appName := deriveAppName(kagentName, kagentNamespace, agentCard, logger)

	// Fall back to appName / "default" so traces always have a non-empty service identity.
	serviceNameSource := kagentName
	if serviceNameSource == "" {
		serviceNameSource = appName
	}
	serviceNamespaceSource := kagentNamespace
	if serviceNamespaceSource == "" {
		serviceNamespaceSource = "default"
	}
	serviceName := strings.ReplaceAll(serviceNameSource, "-", "_")
	serviceNamespace := strings.ReplaceAll(serviceNamespaceSource, "-", "_")
	shutdownTelemetry, telemetryEnabled, telErr := telemetry.Init(context.Background(), serviceName, serviceNamespace)
	if telErr != nil {
		logger.Error(telErr, "Failed to initialize ADK telemetry providers; continuing without telemetry export")
	} else if telemetryEnabled {
		defer func() {
			shutdownCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			defer cancel()
			if err := shutdownTelemetry(shutdownCtx); err != nil {
				logger.Error(err, "Failed to shutdown telemetry providers cleanly")
			}
		}()
		logger.Info("ADK telemetry initialized")
	} else {
		logger.Info("ADK telemetry disabled (set OTEL_TRACING_ENABLED or OTEL_LOGGING_ENABLED to true)")
	}

	// Create authenticated HTTP client when kagent persistence is enabled.
	// This client is shared between the executor's session service and
	// app.New's task store, avoiding duplicate token services.
	var httpClient *http.Client
	var tokenService *auth.KAgentTokenService
	if kagentURL != "" {
		tokenService = auth.NewKAgentTokenService(appName)
		if err := tokenService.Start(context.Background()); err != nil {
			logger.Error(err, "Failed to start token service")
		} else {
			logger.Info("Token service started")
		}
		defer tokenService.Stop()
		httpClient = auth.NewHTTPClientWithToken(tokenService)
	}

	// The executor needs a session service for its BeforeExecute callback
	// (session creation/lookup). This must be created before the executor.
	sessionService, err := session.NewService(agentConfig.SessionDBURL, kagentURL, httpClient)
	if err != nil {
		logger.Error(err, "Failed to open local session store", "url", agentConfig.SessionDBURL)
		os.Exit(1)
	}
	switch sessionService.(type) {
	case *session.LocalSessionService:
		logger.Info("Using local durable-dir session store", "url", agentConfig.SessionDBURL)
	case *session.KAgentSessionService:
		logger.Info("Using KAgent session service", "url", kagentURL)
	default:
		logger.Info("No KAGENT_URL set, using in-memory session and no task persistence")
	}

	ctx := logr.NewContext(context.Background(), logger)

	// Build memory service if configured.
	var memoryService *kagentmemory.KagentMemoryService
	if agentConfig.Memory != nil && kagentURL != "" {
		memSvc, err := kagentmemory.New(kagentmemory.Config{
			AgentName:       appName,
			APIURL:          kagentURL,
			HTTPClient:      httpClient,
			TTLDays:         agentConfig.Memory.TTLDays,
			EmbeddingConfig: agentConfig.Memory.Embedding,
		})
		if err != nil {
			logger.Error(err, "Failed to create memory service")
			os.Exit(1)
		}
		memoryService = memSvc
		logger.Info("Memory service enabled", "appName", appName)
	}

	runnerConfig, err := runnerpkg.CreateRunnerConfig(ctx, agentConfig, sessionService, appName, memoryService, kagentURL, httpClient)
	if err != nil {
		logger.Error(err, "Failed to create Google ADK Runner config")
		os.Exit(1)
	}

	// appa delta 4: the lineage headers a delegated call carries land
	// in session state, as on the python runtime, so a child classifies
	// as a child. The runner's session service and the executor's are
	// the same decorated one.
	runnerConfig.SessionService = withLineageHeaders(gate, runnerConfig.SessionService)
	executorSessionService := withLineageHeaders(gate, sessionService)

	if err := appendAppaPlugin(gate, &runnerConfig, inventory, logger); err != nil {
		logger.Error(err, "Failed to register AppaPluginKagent")
		os.Exit(1)
	}

	stream := agentConfig.GetStream()
	executor := withReviewShape(gate, a2a.NewKAgentExecutor(a2a.KAgentExecutorConfig{
		RunnerConfig:   runnerConfig,
		SessionService: executorSessionService,
		Stream:         stream,
		AppName:        appName,
		Logger:         logger,
	}))

	// Build the agent card.
	if agentCard == nil {
		agentCard = &a2atype.AgentCard{
			Name:        "go-adk-agent",
			Description: "Go-based Agent Development Kit",
			Version:     "0.2.0",
		}
	}
	agentCard.Capabilities = a2atype.AgentCapabilities{
		Streaming:              stream,
		StateTransitionHistory: true,
	}

	// Delegate server, task store, and remaining infrastructure to app.New.
	// Passing HTTPClient prevents app.New from creating a second token service.
	kagentApp, err := app.New(app.AppConfig{
		AgentCard:       *agentCard,
		Host:            *host,
		Port:            port,
		KAgentURL:       kagentURL,
		AppName:         appName,
		ShutdownTimeout: 5 * time.Second,
		Logger:          logger,
		HTTPClient:      httpClient,
		Agent:           runnerConfig.Agent,
	}, executor)
	if err != nil {
		logger.Error(err, "Failed to create app")
		os.Exit(1)
	}

	if err := kagentApp.Run(); err != nil {
		logger.Error(err, "Server error")
		os.Exit(1)
	}
}

// loadAgentConfigs loads the rendered config and the agent card. It
// ends the process on a config this image will not run.
//
// An ungated agent gets the stock loader, so this image loads exactly
// what the stock image loads. A gated agent gets delta 6 as well: the
// guard refuses the raw config.json before anything runs it. The env
// delivery is on disk by now, so one raw read covers both the mounted
// file and KAGENT_CONFIG_JSON. The guard decodes the config the runtime
// runs from the bytes it checked, and nothing reads the file again. The
// stock validation then runs on the decoded config, and the stock
// loader reads the agent card from disk.
// loadAgentConfigs reads the rendered config and card. In the gated
// mode the config passes the guard, and the inventory it returns spells
// every tool the gate admits; the stock mode builds none.
func loadAgentConfigs(gate gating, configDir string, logger logr.Logger) (*adk.AgentConfig, appakagentadk.Inventory, *a2atype.AgentCard) {
	if !gate.enabled() {
		agentConfig, agentCard, err := config.LoadAgentConfigs(configDir)
		if err != nil {
			logger.Error(err, "Failed to load agent config (model configuration is required)", "configDir", configDir)
			os.Exit(1)
		}
		return agentConfig, appakagentadk.Inventory{}, agentCard
	}
	raw, err := os.ReadFile(filepath.Join(configDir, "config.json"))
	if err != nil {
		logger.Error(err, "Failed to read agent config", "configDir", configDir)
		os.Exit(1)
	}
	agentConfig, inventory, err := decodeGuarded(raw, os.Getenv(skillsFolderEnv))
	var refusal *configRefusal
	if errors.As(err, &refusal) {
		logger.Error(err, "Refusing to start", "configDir", configDir)
		os.Exit(1)
	}
	if err != nil {
		logger.Error(err, "Failed to load agent config", "configDir", configDir)
		os.Exit(1)
	}
	if err := config.ValidateAgentConfigUsage(agentConfig); err != nil {
		logger.Error(err, "Invalid agent config (model configuration is required)", "configDir", configDir)
		os.Exit(1)
	}
	agentCard, err := config.LoadAgentCard(filepath.Join(configDir, "agent-card.json"))
	if err != nil {
		logger.Error(err, "Failed to load agent card", "configDir", configDir)
		os.Exit(1)
	}
	return agentConfig, inventory, agentCard
}

func deriveAppName(kagentName, kagentNamespace string, agentCard *a2atype.AgentCard, logger logr.Logger) string {
	if kagentNamespace != "" && kagentName != "" {
		namespace := strings.ReplaceAll(kagentNamespace, "-", "_")
		name := strings.ReplaceAll(kagentName, "-", "_")
		appName := namespace + "__NS__" + name
		logger.Info("Built app_name from environment variables",
			"KAGENT_NAMESPACE", kagentNamespace,
			"KAGENT_NAME", kagentName,
			"app_name", appName)
		return appName
	}

	if agentCard != nil && agentCard.Name != "" {
		logger.Info("Using agent card name as app_name", "app_name", agentCard.Name)
		return agentCard.Name
	}

	logger.Info("Using default app_name", "app_name", "go-adk-agent")
	return "go-adk-agent"
}

// withReviewShape wraps the stock executor with delta 5 while the knob
// is on. While the knob is off it returns the stock executor, so the
// task history is the stock history.
func withReviewShape(gate gating, executor a2asrv.AgentExecutor) a2asrv.AgentExecutor {
	if !gate.enabled() {
		return executor
	}
	return reviewShapedExecutor{executor}
}

// reviewShapedExecutor keeps the task history python-shaped while a
// person rules on a remedy. adk-go yields the reviewed control call's
// pending response before the confirmation event; the python ADK yields
// the confirmation first and kagent's python executor stops converting
// there, so a python task never shows that call as completed. The
// kagent dashboard renders the approval card only for a call without a
// response. So this executor drops the plugin's own pending-review
// response part on its way to the A2A queue; ADK's session keeps the
// event, as it does on python.
type reviewShapedExecutor struct {
	a2asrv.AgentExecutor
}

func (e reviewShapedExecutor) Execute(ctx context.Context, reqCtx *a2asrv.RequestContext, queue eventqueue.Queue) error {
	return e.AgentExecutor.Execute(ctx, reqCtx, reviewShapedQueue{queue})
}

type reviewShapedQueue struct {
	eventqueue.Queue
}

func (q reviewShapedQueue) Write(ctx context.Context, event a2atype.Event) error {
	update, ok := event.(*a2atype.TaskStatusUpdateEvent)
	if !ok || update.Status.Message == nil {
		return q.Queue.Write(ctx, event)
	}
	before := len(update.Status.Message.Parts)
	update.Status.Message.Parts = withoutPendingReview(update.Status.Message.Parts)
	if before > 0 && len(update.Status.Message.Parts) == 0 && !update.Final {
		return nil // the update carried nothing but the pending response
	}
	return q.Queue.Write(ctx, event)
}

func withoutPendingReview(parts a2atype.ContentParts) a2atype.ContentParts {
	kept := make(a2atype.ContentParts, 0, len(parts))
	for _, part := range parts {
		if !isPendingReview(part) {
			kept = append(kept, part)
		}
	}
	return kept
}

func isPendingReview(part a2atype.Part) bool {
	var data map[string]any
	switch p := part.(type) {
	case a2atype.DataPart:
		data = p.Data
	case *a2atype.DataPart:
		data = p.Data
	default:
		return false
	}
	name, _ := data["name"].(string)
	response, _ := data["response"].(map[string]any)
	return appakagentadk.IsPendingReview(name, response)
}
