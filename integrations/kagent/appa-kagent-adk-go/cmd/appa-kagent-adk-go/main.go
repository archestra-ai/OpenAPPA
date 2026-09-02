// The appa-kagent-adk-go runtime main.
//
// This binary replaces the stock kagent go runtime image. It replays
// the stock runtime construction — adk/cmd/main.go at kagent tag
// go/v0.10.0-rc4 — through the exported go/adk packages only (no
// kagent internal/ imports), and adds exactly six deltas:
//
//  1. It refuses to start without APPA_RUNTIME_URL.
//  2. It appends the reserved-tool toolset to the rendered config: a
//     streamable-HTTP MCP toolset at $APPA_RUNTIME_URL/mcp serving
//     execute_remedy_plan, through the same HttpTools path kagent uses
//     for CRD MCP tools.
//  3. It registers AppaPluginKagent after the stock plugins in
//     runner.PluginConfig — the registration point kagent itself uses
//     (go/adk/pkg/runner/adapter.go).
//  4. It fills the OpenAI model's reasoning_effort from
//     APPA_KAGENT_OPENAI_REASONING_EFFORT when the rendered config
//     leaves it unset. The v1alpha2 ModelConfig enum has no "none",
//     which some OpenAI models require for function tools on chat
//     completions; a value the CRD set wins.
//  5. It lands the inbound lineage headers in session state on every
//     session Get and Create, so a delegated entry classifies as a
//     child — the python executor persists them as a header_update
//     event; the go controller session service folds no state back.
//  6. It drops the plugin's own pending-review response from the A2A
//     task while a person rules on a remedy, so the task history stays
//     python-shaped and the kagent dashboard renders the approval card.
//
// Everything else keeps the stock contract: --host/--port/--filepath
// args, config.json + agent-card.json under the config dir (or the
// KAGENT_CONFIG_JSON / KAGENT_AGENT_CARD_JSON env delivery), A2A on
// the given port, and readiness at /.well-known/agent-card.json.
package main

import (
	"cmp"
	"context"
	"flag"
	"fmt"
	"net/http"
	"os"
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

// appaRuntimeURL reads APPA_RUNTIME_URL. Without it the image cannot
// gate anything, so the runtime refuses to start rather than run open.
func appaRuntimeURL() (string, error) {
	url := strings.TrimSpace(os.Getenv("APPA_RUNTIME_URL"))
	if url == "" {
		return "", fmt.Errorf("APPA_RUNTIME_URL is not set: the appa-kagent-adk-go runtime refuses to start ungated")
	}
	return url, nil
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

// withReservedToolset appends the engine's remedy-execution toolset to
// the rendered config: the reserved execute_remedy_plan tool over
// streamable HTTP at $APPA_RUNTIME_URL/mcp, built by the same stock
// HttpTools path as every CRD MCP toolset.
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

// spawnToolNames lists the agent-as-tool wire names of the rendered
// config: the remote agents, under the names the stock builder
// dispatches them by. Entries the stock builder skips (no URL) are
// skipped here too.
func spawnToolNames(agentConfig *adk.AgentConfig) []string {
	var names []string
	for _, remoteAgent := range agentConfig.RemoteAgents {
		if remoteAgent.Url == "" {
			continue
		}
		names = append(names, remoteAgent.Name)
	}
	return names
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

	runtimeURL, err := appaRuntimeURL()
	if err != nil {
		logger.Error(err, "Refusing to start")
		os.Exit(1)
	}

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

	agentConfig, agentCard, err := config.LoadAgentConfigs(configDir)
	if err != nil {
		logger.Error(err, "Failed to load agent config (model configuration is required)", "configDir", configDir)
		os.Exit(1)
	}
	logger.Info("Loaded agent config", "configDir", configDir)
	logger.Info("Agent configuration",
		"model", agentConfig.Model.GetType(),
		"stream", agentConfig.GetStream(),
		"httpTools", len(agentConfig.HttpTools),
		"sseTools", len(agentConfig.SseTools),
		"remoteAgents", len(agentConfig.RemoteAgents))

	// appa delta: the reserved-tool toolset joins the config before the
	// stock builder runs, so it is constructed exactly like every other
	// MCP toolset.
	withReservedToolset(agentConfig, runtimeURL)
	logger.Info("Wired the appa reserved-tool toolset", "url", runtimeURL)

	// appa delta: the image env fills the OpenAI reasoning effort the
	// CRD cannot express; a CRD-set value wins.
	withReasoningEffort(agentConfig, os.Getenv(reasoningEffortEnv))

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

	// appa delta: the lineage headers a delegated call carries land in
	// session state, as on the python runtime, so a child classifies as
	// a child. The runner's service and the executor's are the same
	// decorated one.
	runnerConfig.SessionService = lineageSessionService{runnerConfig.SessionService}
	var executorSessionService adksession.Service
	if sessionService != nil {
		executorSessionService = lineageSessionService{sessionService}
	}

	// appa delta: AppaPluginKagent joins the plugin list after the
	// stock plugins. Order is load-bearing — ADK stops a callback chain
	// at the first non-nil answer, and no stock plugin answers a gated
	// callback, so appending last never lets one short-circuit a gate.
	appaPlugin, err := appakagentadk.New(appakagentadk.Config{
		RuntimeURL: runtimeURL,
		SpawnTools: spawnToolNames(agentConfig),
	})
	if err != nil {
		logger.Error(err, "Failed to create AppaPluginKagent")
		os.Exit(1)
	}
	adkPlugin, err := appaPlugin.ADKPlugin()
	if err != nil {
		logger.Error(err, "Failed to wire AppaPluginKagent into the ADK plugin surface")
		os.Exit(1)
	}
	runnerConfig.PluginConfig.Plugins = append(runnerConfig.PluginConfig.Plugins, adkPlugin)
	logger.Info("Registered AppaPluginKagent", "runtimeURL", runtimeURL, "spawnTools", len(spawnToolNames(agentConfig)))

	stream := agentConfig.GetStream()
	executor := a2a.NewKAgentExecutor(a2a.KAgentExecutorConfig{
		RunnerConfig:   runnerConfig,
		SessionService: executorSessionService,
		Stream:         stream,
		AppName:        appName,
		Logger:         logger,
	})

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
	}, reviewShapedExecutor{executor})
	if err != nil {
		logger.Error(err, "Failed to create app")
		os.Exit(1)
	}

	if err := kagentApp.Run(); err != nil {
		logger.Error(err, "Server error")
		os.Exit(1)
	}
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
