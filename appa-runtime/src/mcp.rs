//! Runtime-owned MCP tools, provided identically to every harness: `execute_remedy_plan`,
//! the engine's reserved tool; the `appa_*` management tools; and `yell`, which reports
//! that APPA is in the way.
//!
//! Served over streamable HTTP from process start. `execute_remedy_plan` and `yell` carry
//! no trajectory, so both take it from the hook that preceded the call — the one place the
//! harness names it. A request no hook vouched for is refused. For a remedy the engine then
//! judges from the log whether the offer still stands: an unknown, unpursued, cross-turn,
//! or terminal id is refused. `appa_match_batteries` is read-only: it intersects
//! host-observed names with the runtime's current catalog.
//!
//! The constrained file launcher instead serves file tools and remedy execution over
//! private stdio. That server receives its trajectory from host process arguments, not
//! hooks or model tool arguments. It checks and admits file calls before returning results.
//!
//! `yell` is advertised only where the deployment turned agent reporting on. A build that
//! does not advertise it does not route it either, so a client holding a stale tool list
//! gets the same answer as a client that invented the name.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::InputResponses;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::yell::Harness;

use crate::api::{ExecuteRemedyPlanArgs, PermitKey, RemedyReply, Runtime};
use crate::batteries::{BatteriesResponse, BundledBattery};
use crate::elicit::{Elicitation, Review};
use crate::yell::YellArgs;

/// The tools this runtime serves, and the router that decides which of them exist for this
/// session. Built per session, so a `/reload` that turns agent reporting on or off settles
/// the next client rather than the next restart.
#[derive(Clone)]
pub struct RuntimeToolService {
    runtime: Arc<Runtime>,
    battery_state: Arc<RwLock<BatteryState>>,
}

#[derive(Clone)]
pub struct RuntimeTools {
    runtime: Arc<Runtime>,
    harness: Harness,
    tool_router: ToolRouter<Self>,
    file_actor: Option<appa_runtime_api::Actor>,
}

#[derive(Debug, Clone)]
pub struct BatteryState {
    pub catalog: BatteriesResponse,
    pub included: BTreeSet<String>,
    pub serving_tools: BTreeSet<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct MatchBatteriesArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
    /// The observed server or host source these names came from.
    pub source: String,
    /// Exact configured MCP URL from the observed kagent connection. Required
    /// for MCP coverage checks; omit for namespace/delegations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Exact tool wire names observed from the host's live inventory.
    pub tools: Vec<String>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct PolicyKeyArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
    /// The serving policy key observed before this approved operation.
    pub expected_policy_key: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct IncludeBatteryArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
    /// One available battery name returned by appa_match_batteries.
    pub battery: String,
    /// The serving policy key observed before this approved operation.
    pub expected_policy_key: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct UpdatePolicyArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
    /// The complete proposed root policy.
    pub policy: String,
    /// The serving policy key observed before this approved operation.
    pub expected_policy_key: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct RuntimeStateArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
}

trait ManagementArguments: serde::Serialize {
    fn actor(&self) -> &str;
}

impl ManagementArguments for MatchBatteriesArgs {
    fn actor(&self) -> &str {
        &self.actor
    }
}

impl ManagementArguments for PolicyKeyArgs {
    fn actor(&self) -> &str {
        &self.actor
    }
}

impl ManagementArguments for IncludeBatteryArgs {
    fn actor(&self) -> &str {
        &self.actor
    }
}

impl ManagementArguments for UpdatePolicyArgs {
    fn actor(&self) -> &str {
        &self.actor
    }
}

impl ManagementArguments for RuntimeStateArgs {
    fn actor(&self) -> &str {
        &self.actor
    }
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
struct BatteryMatch {
    battery: String,
    included: bool,
    tools: Vec<ToolMatch>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
struct ToolMatch {
    observed: String,
    declarations: Vec<String>,
    match_kind: &'static str,
}

/// One control call's answer in this transport's shape.
fn remedy_result(reply: RemedyReply) -> CallToolResult {
    let content = vec![ContentBlock::text(reply.text)];
    match reply.is_error {
        true => CallToolResult::error(content),
        false => CallToolResult::success(content),
    }
}

/// One `execute_remedy_plan` request. A review that must first be asked ends the call
/// before anything executes: the vouch is read, not spent, so it stands for the retry
/// that carries the answer.
async fn execute_remedy(
    runtime: &Runtime,
    args: ExecuteRemedyPlanArgs,
    responses: Option<rmcp::model::InputResponses>,
    request: RequestContext<RoleServer>,
    expected_actor: Option<&appa_runtime_api::Actor>,
) -> CallToolResponse {
    let pending = || runtime.pending_hitl_review(&args.offer_id);
    match Elicitation::open(request, responses, runtime.review_timeout(), pending) {
        Review::Ask(review) => review.into(),
        Review::Proceed(elicitation) => remedy_result(
            runtime
                .execute_remedy_plan(args, elicitation.as_ref(), expected_actor)
                .await,
        )
        .into(),
    }
}

#[tool_router]
impl RuntimeTools {
    /// The tools this deployment serves right now, reporting under `harness`: a served
    /// adapter's name, or the name an embedding host files its reports under. `yell` is
    /// dropped from the router where the deployment has not turned agent reporting on,
    /// which is both how it stops being advertised and how a call to it stops being routed.
    pub fn new(runtime: Arc<Runtime>, harness: Harness) -> RuntimeTools {
        let mut tool_router = Self::tool_router();
        if !runtime.agent_yell() {
            tool_router.remove_route(YELL);
        }
        if !runtime.file_tracking_enabled() {
            for tool in crate::api::files::TOOLS {
                tool_router.remove_route(tool);
            }
        }
        if !runtime.file_process_enabled() {
            tool_router.remove_route("appa_process_files");
        }
        RuntimeTools {
            runtime,
            harness,
            tool_router,
            file_actor: None,
        }
    }

    #[tool(description = "Execute one remedy plan by the offer id that blocking \
                       feedback surfaced. The id must be quoted exactly. A plan that \
                       declares a subagent's return takes `label`, the lowest label this \
                       session accepts from the return; a plan that attests it also takes \
                       `return_schema`. After execution succeeds, propose the call again with \
                       exactly the arguments the reply gives, or receive the admitted output.")]
    pub async fn execute_remedy_plan(
        &self,
        Parameters(args): Parameters<ExecuteRemedyPlanArgs>,
        InputResponses(responses): InputResponses,
        request: RequestContext<RoleServer>,
    ) -> CallToolResponse {
        execute_remedy(&self.runtime, args, responses, request, self.file_actor.as_ref()).await
    }

    #[tool(
        description = "Read a UTF-8 file in the tracked workspace. APPA checks its source Label before reading content."
    )]
    pub async fn appa_read_file(&self, Parameters(args): Parameters<crate::api::files::ReadArgs>) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_read_file",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(
        description = "Atomically replace a UTF-8 file in the tracked workspace with content derived from this trajectory."
    )]
    pub async fn appa_write_file(&self, Parameters(args): Parameters<crate::api::files::WriteArgs>) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_write_file",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(
        description = "Replace exactly one occurrence of old_string in a UTF-8 file. APPA checks the file Label before matching, including failed matches."
    )]
    pub async fn appa_edit_file(&self, Parameters(args): Parameters<crate::api::files::EditArgs>) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_edit_file",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(
        description = "Copy a tracked regular file without returning its contents. The destination retains the source Label. Replaces an existing destination."
    )]
    pub async fn appa_copy_file(
        &self,
        Parameters(args): Parameters<crate::api::files::FileTransferArgs>,
    ) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_copy_file",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(
        description = "Move a tracked regular file within the workspace without returning its contents. The destination retains the source Label. Replaces an existing destination."
    )]
    pub async fn appa_move_file(
        &self,
        Parameters(args): Parameters<crate::api::files::FileTransferArgs>,
    ) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_move_file",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(
        description = "Run a shell command in an isolated workspace with only declared input snapshots and a trusted system toolchain. Read inputs at inputs/<path> and write output/result. The runtime publishes that one regular file at output_path after teardown. All outputs and errors carry every input Label. No network or native-workspace access."
    )]
    pub async fn appa_process_files(
        &self,
        Parameters(args): Parameters<crate::api::files::ProcessArgs>,
    ) -> CallToolResult {
        file_result(
            &self.runtime,
            self.file_actor.as_ref(),
            "appa_process_files",
            serde_json::to_value(args).expect("file arguments serialize"),
        )
        .await
    }

    #[tool(description = "Report to the OpenAPPA developers when APPA is malfunctioning, \
                       contradictory, or its blocking feedback or remedies are \
                       confusing and leave no clear way forward. Say in `message` what you were \
                       trying to do, what APPA did or demanded, and why you are stuck. This \
                       sends diagnostic telemetry and will not bypass the policy or grant \
                       tool permissions. Set `with_trajectory` to include this session's APPA \
                       decisions — its rulings, remedies and label changes, never your prompts, \
                       tool arguments or tool outputs — or leave it false to report on the \
                       policy alone.")]
    pub(crate) async fn yell(&self, Parameters(args): Parameters<YellArgs>) -> CallToolResult {
        match crate::yell::agent::yell(&self.runtime, self.harness.clone(), &args).await {
            crate::yell::agent::Outcome::Sent(receipt) => {
                let already = match receipt.duplicate {
                    true => " (already had this one)",
                    false => "",
                };
                CallToolResult::success(vec![ContentBlock::text(format!(
                    "[appa] Reported{already}. Receipt {}.",
                    receipt.receipt_id
                ))])
            }
            crate::yell::agent::Outcome::Refused(refusal) => {
                CallToolResult::error(vec![ContentBlock::text(format!("[appa] {refusal}"))])
            }
            crate::yell::agent::Outcome::Unvouched(crate::api::Unvouched::Nobody) => {
                CallToolResult::error(vec![ContentBlock::text(
                    "[appa] This call was not seen by a hook, so there is no session to report on. \
                     Propose it as an ordinary tool call.",
                )])
            }
            // Retrying the identical call cannot work, so the answer says what to change.
            crate::yell::agent::Outcome::Unvouched(crate::api::Unvouched::Ambiguous) => {
                CallToolResult::error(vec![ContentBlock::text(
                    "[appa] Another session on this machine is reporting the same thing word for \
                     word, so this call does not say which session it is about. Call again with a \
                     message that describes your own session.",
                )])
            }
            crate::yell::agent::Outcome::Oversize => CallToolResult::error(vec![ContentBlock::text(
                "[appa] The report is too large to send even with the session left out.",
            )]),
            crate::yell::agent::Outcome::Undeliverable(failure) => {
                CallToolResult::error(vec![ContentBlock::text(format!("[appa] Not reported: {failure}"))])
            }
        }
    }
}

/// The advertised name of the reporting tool, and the route removed where the deployment
/// leaves agent reporting off.
const YELL: &str = "yell";

#[tool_router]
impl RuntimeToolService {
    pub fn new(runtime: Arc<Runtime>) -> RuntimeToolService {
        RuntimeToolService::with_batteries(
            runtime,
            Arc::new(RwLock::new(BatteryState {
                catalog: BatteriesResponse { batteries: Vec::new() },
                included: BTreeSet::new(),
                serving_tools: BTreeSet::new(),
            })),
        )
    }

    pub fn with_batteries(runtime: Arc<Runtime>, battery_state: Arc<RwLock<BatteryState>>) -> RuntimeToolService {
        RuntimeToolService { runtime, battery_state }
    }

    #[tool(description = "Execute one remedy plan by the offer id that blocking \
                       feedback surfaced. The id must be quoted exactly. A plan that \
                       declares a subagent's return takes `label`, the lowest label this \
                       session accepts from the return; a plan that attests it also takes \
                       `return_schema`. After execution succeeds, propose the call again with \
                       exactly the arguments the reply gives, or receive the admitted output.")]
    pub async fn execute_remedy_plan(
        &self,
        Parameters(args): Parameters<ExecuteRemedyPlanArgs>,
        InputResponses(responses): InputResponses,
        request: RequestContext<RoleServer>,
    ) -> CallToolResponse {
        execute_remedy(&self.runtime, args, responses, request, None).await
    }

    #[tool(
        description = "Match observed native tool names against the battery catalog. Matches are suggestions, not permission. Supply the exact configured MCP endpoint to check coverage against the serving policy and obtain its server identity for an approved battery binding. Without an endpoint MCP coverage is unknown. Use source namespace/delegations for agent names."
    )]
    pub async fn appa_match_batteries(&self, Parameters(args): Parameters<MatchBatteriesArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_match_batteries", &args) {
            return management_refused();
        }
        let state = self
            .battery_state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observed = observed_for_source(&args.source, &args.tools);
        let matches = match_batteries(&state.catalog.batteries, &state.included, &observed);
        let (server, coverage) = match source_coverage(&self.runtime, &args.source, args.endpoint.as_deref(), &observed)
        {
            Ok(result) => result,
            Err(error) => return CallToolResult::error(vec![ContentBlock::text(error)]),
        };
        let unconfigured_tools: Vec<_> = coverage
            .tools
            .iter()
            .filter(|check| matches!(check.status, crate::tool_validation::ToolStatus::Invalid { .. }))
            .map(|check| &check.tool)
            .collect();
        let body = serde_json::to_string(&serde_json::json!({
            "source": args.source,
            "matches": matches,
            "serving_tools": &state.serving_tools,
            "unconfigured_tools": unconfigured_tools,
            "server": server,
            "coverage": coverage,
        }))
        .expect("battery matches contain only serializable values");
        CallToolResult::success(vec![ContentBlock::text(body)])
    }

    #[tool(
        description = "Read the runtime's serving policy, policy key, included batteries, refresh state, and Kubernetes policy identity. This operation is read-only."
    )]
    pub async fn appa_get_runtime_state(&self, Parameters(args): Parameters<RuntimeStateArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_get_runtime_state", &args) {
            return management_refused();
        }
        management_result(crate::management::run::<PolicyKeyArgs>("appa-guide-runtime-state", None).await)
    }

    #[tool(
        description = "Include one available battery in the complete current root policy, update the runtime-owned ConfigMap, wait for its mounted file, and reload atomically. Requires an allowed APPA ToolCall and human approval."
    )]
    pub async fn appa_include_battery(&self, Parameters(args): Parameters<IncludeBatteryArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_include_battery", &args) {
            return management_refused();
        }
        management_result(crate::management::run("appa-guide-include-battery", Some(&args)).await)
    }

    #[tool(
        description = "Validate and publish one complete root policy while retaining every existing table in order, then wait for the ConfigMap and reload atomically. Approved field values may change. Requires an allowed APPA ToolCall and human approval."
    )]
    pub async fn appa_update_policy(&self, Parameters(args): Parameters<UpdatePolicyArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_update_policy", &args) {
            return management_refused();
        }
        management_result(crate::management::run("appa-guide-update-policy", Some(&args)).await)
    }

    #[tool(
        description = "Reload the complete policy currently mounted for this runtime. Requires an allowed APPA ToolCall and human approval."
    )]
    pub async fn appa_reload_policy(&self, Parameters(args): Parameters<PolicyKeyArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_reload_policy", &args) {
            return management_refused();
        }
        management_result(crate::management::run("appa-guide-reload-policy", Some(&args)).await)
    }

    #[tool(
        description = "Fetch the latest verified stable OpenAPPA battery release, validate it against serving policy, publish it, reload, and commit or roll back as one operation. Requires an allowed APPA ToolCall and human approval."
    )]
    pub async fn appa_refresh_batteries(&self, Parameters(args): Parameters<PolicyKeyArgs>) -> CallToolResult {
        if !take_management_vouch(&self.runtime, "appa_refresh_batteries", &args) {
            return management_refused();
        }
        management_result(crate::management::run("appa-guide-refresh-batteries", Some(&args)).await)
    }
}

async fn file_result(
    runtime: &Runtime,
    actor: Option<&appa_runtime_api::Actor>,
    tool: &str,
    arguments: serde_json::Value,
) -> CallToolResult {
    let result = match actor {
        Some(actor) => runtime.execute_bound_file(actor, tool, arguments).await,
        None => runtime.execute_file(tool, arguments).await,
    };
    match result {
        Ok(crate::api::files::FileReply::Value(value)) => CallToolResult::success(vec![ContentBlock::text(value)]),
        Ok(crate::api::files::FileReply::Failure(message)) => CallToolResult::error(vec![ContentBlock::text(message)]),
        Err(error) => {
            tracing::warn!(%error, "runtime-owned file operation could not admit an observation");
            CallToolResult::error(vec![ContentBlock::text(
                "file operation was not admitted; no file observation is returned",
            )])
        }
    }
}

fn take_management_vouch<T: ManagementArguments>(runtime: &Runtime, tool: &str, arguments: &T) -> bool {
    let arguments_value = serde_json::to_value(arguments).expect("management arguments are plain data");
    runtime
        .take_vouched(&PermitKey::call(tool, &arguments_value))
        .is_ok_and(|(actor, _)| {
            let actual = &crate::api::acting_trajectory(&actor).0;
            actual == arguments.actor()
                || actual
                    .strip_prefix("kagent:")
                    .is_some_and(|trajectory| trajectory == arguments.actor())
        })
}

fn management_result(result: Result<String, String>) -> CallToolResult {
    match result {
        Ok(output) => CallToolResult::success(vec![ContentBlock::text(output)]),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
    }
}

fn management_refused() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        "this management call has no one-shot APPA vouch; propose it through the gated Agent",
    )])
}

fn match_batteries(catalog: &[BundledBattery], included: &BTreeSet<String>, observed: &[String]) -> Vec<BatteryMatch> {
    let observed: BTreeSet<&str> = observed
        .iter()
        .map(String::as_str)
        .filter(|name| !name.is_empty())
        .collect();
    catalog
        .iter()
        .filter_map(|battery| {
            let tools: Vec<ToolMatch> = observed
                .iter()
                .filter_map(|observed| match_tool(&battery.tools, observed))
                .collect();
            (!tools.is_empty()).then(|| BatteryMatch {
                battery: battery.name.clone(),
                included: included.contains(&battery.name),
                tools,
            })
        })
        .collect()
}

fn match_tool(declarations: &[String], observed: &str) -> Option<ToolMatch> {
    let exact: Vec<String> = declarations
        .iter()
        .filter(|declaration| selector_name(declaration) == observed)
        .cloned()
        .collect();
    if !exact.is_empty() {
        return Some(ToolMatch {
            observed: observed.to_string(),
            declarations: exact,
            match_kind: "exact",
        });
    }
    let suffix: Vec<String> = declarations
        .iter()
        .filter(|declaration| {
            selector_name(declaration)
                .rsplit_once('/')
                .or_else(|| selector_name(declaration).rsplit_once("__"))
                .is_some_and(|(_, tail)| tail == observed)
        })
        .cloned()
        .collect();
    (!suffix.is_empty()).then(|| ToolMatch {
        observed: observed.to_string(),
        declarations: suffix,
        match_kind: "suffix",
    })
}

fn selector_name(declaration: &str) -> &str {
    declaration.split_once('(').map_or(declaration, |(name, _)| name)
}

fn source_coverage(
    runtime: &Runtime,
    source: &str,
    endpoint: Option<&str>,
    observed: &[String],
) -> Result<(Option<String>, crate::tool_validation::ValidationReport), String> {
    use crate::tool_validation::{ToolCheck, ToolStatus, ValidationReport};
    use appa_runtime_api::inventory::{InventorySource, InventoryStatus, ObservedTool, ToolInventory};
    use sha2::{Digest, Sha256};

    let mut inventory = ToolInventory::default();
    let server = if let Some(namespace) = source.strip_suffix("/delegations") {
        if endpoint.is_some() {
            return Err("delegations do not have an MCP endpoint".into());
        }
        for name in observed {
            let (encoded_namespace, agent) = name
                .split_once("__NS__")
                .ok_or("delegation names must identify their namespace and agent")?;
            if encoded_namespace.replace('_', "-") != namespace {
                return Err("a delegation belongs to a different source namespace".into());
            }
            inventory.tools.push(ObservedTool {
                name: name.clone(),
                tool: format!("agent:{namespace}/{}", agent.replace('_', "-")),
            });
        }
        None
    } else if let Some(endpoint) = endpoint {
        let url = url::Url::parse(endpoint).map_err(|_| "endpoint must be an HTTP(S) URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("endpoint must be an HTTP(S) URL without userinfo or a fragment".into());
        }
        let hex: String = Sha256::digest(endpoint.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let server = format!("server-{hex}");
        inventory.tools = observed
            .iter()
            .map(|name| ObservedTool {
                name: name.clone(),
                tool: format!("mcp:{server}/{name}"),
            })
            .collect();
        inventory.sources.push(InventorySource {
            server: server.clone(),
            status: InventoryStatus::Partial,
            dynamic: true,
            detail: None,
        });
        Some(server)
    } else {
        return Ok((
            None,
            ValidationReport {
                tools: observed
                    .iter()
                    .map(|name| ToolCheck {
                        tool: name.clone(),
                        status: ToolStatus::Unknown {
                            reason: "supply the configured MCP endpoint to check source coverage".into(),
                        },
                    })
                    .collect(),
                tools_may_change: true,
                ..ValidationReport::default()
            },
        ));
    };
    let mut report = runtime
        .preflight_inventory(None, appa_adapter_kagent::adapter(), &inventory)
        .map_err(|error| error.to_string())?;
    if !report.errors.is_empty() {
        return Err(report.errors.join("; "));
    }
    // Other sources' unresolved declarations say nothing about this observation.
    report.tools.retain(|check| observed.contains(&check.tool));
    Ok((server, report))
}

fn observed_for_source(source: &str, observed: &[String]) -> Vec<String> {
    let Some(namespace) = source.strip_suffix("/delegations") else {
        return observed.to_vec();
    };
    observed
        .iter()
        .filter(|name| !name.is_empty())
        .map(|name| {
            if name.contains("__NS__") {
                name.clone()
            } else {
                format!("{}__NS__{}", namespace.replace('-', "_"), name.replace('-', "_"))
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[tool_handler]
impl ServerHandler for RuntimeToolService {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.server_info.name = "appa-runtime".to_string();
        info.server_info.version = RUNTIME_VERSION.to_string();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "When blocking feedback names an offer id, call execute_remedy_plan \
             with that exact id to pursue the offered remedy. Use \
             appa_match_batteries to intersect host-observed tool names with \
             the current battery catalog."
                .to_string(),
        );
        info
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RuntimeTools {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.server_info.name = "appa-runtime".to_string();
        info.server_info.version = RUNTIME_VERSION.to_string();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions =
            Some("When blocking feedback names an offer id, call execute_remedy_plan with that exact id.".to_string());
        info
    }
}

/// Advertised runtime version for MCP clients.
const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

const SESSION_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// Private stdio transport. The actor comes from the host's process arguments, never MCP.
pub(crate) async fn serve_files(runtime: Arc<Runtime>, actor: appa_runtime_api::Actor) -> Result<(), String> {
    use rmcp::ServiceExt;
    let mut tools = RuntimeTools::new(runtime, Harness::ClaudeCode);
    tools.file_actor = Some(actor);
    tools.tool_router.remove_route(YELL);
    let service = tools
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| error.to_string())?;
    service.waiting().await.map_err(|error| error.to_string())?;
    Ok(())
}

/// MCP service served at `/mcp`.
pub fn service(runtime: Arc<Runtime>, harness: Harness) -> StreamableHttpService<RuntimeTools, LocalSessionManager> {
    service_with_allowed_hosts(runtime, &[], harness)
}

pub fn service_with_allowed_hosts(
    runtime: Arc<Runtime>,
    allowed_hosts: &[String],
    harness: Harness,
) -> StreamableHttpService<RuntimeTools, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(runtime.review_timeout() + SESSION_GRACE);
    let config = server_config(allowed_hosts);
    StreamableHttpService::new(
        move || Ok(RuntimeTools::new(Arc::clone(&runtime), harness.clone())),
        Arc::new(sessions),
        config,
    )
}

pub fn guide_service_with_allowed_hosts(
    runtime: Arc<Runtime>,
    battery_state: Arc<RwLock<BatteryState>>,
    allowed_hosts: &[String],
) -> StreamableHttpService<RuntimeToolService, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(runtime.review_timeout() + SESSION_GRACE);
    let config = server_config(allowed_hosts);
    StreamableHttpService::new(
        move || {
            Ok(RuntimeToolService::with_batteries(
                Arc::clone(&runtime),
                Arc::clone(&battery_state),
            ))
        },
        Arc::new(sessions),
        config,
    )
}

fn server_config(allowed_hosts: &[String]) -> StreamableHttpServerConfig {
    if allowed_hosts.is_empty() {
        StreamableHttpServerConfig::default()
    } else {
        StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::raw;
    use crate::api::{
        EmbeddedPresentationOptions, OfferId, ProposedCall, RemedyOutcome, RemedyRefusal, Runtime, ToolCallDecision,
    };
    use crate::config::Config;
    use appa_runtime_api::HookDecision;

    #[test]
    fn the_mcp_server_advertises_the_release_version_the_cli_advertises() {
        let runtime = std::sync::Arc::new(
            Runtime::open(
                config(),
                tempfile::tempdir()
                    .expect("a temp dir is creatable")
                    .path()
                    .join("appa.db"),
                None,
            )
            .expect("the deployment opens"),
        );
        let service = RuntimeTools::new(Arc::clone(&runtime), Harness::ClaudeCode);
        assert_eq!(service.get_info().server_info.version, env!("CARGO_PKG_VERSION"));
        // The instance's router, not the static one: `RuntimeTools` decides per session
        // whether `yell` exists, and this fixture leaves agent reporting off.
        let remedy_tools: Vec<String> = service
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(remedy_tools, ["execute_remedy_plan"]);

        let guide = RuntimeToolService::new(runtime);
        assert_eq!(guide.get_info().server_info.version, env!("CARGO_PKG_VERSION"));
        let mut tools: Vec<String> = RuntimeToolService::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        tools.sort();
        assert_eq!(
            tools,
            [
                "appa_get_runtime_state",
                "appa_include_battery",
                "appa_match_batteries",
                "appa_refresh_batteries",
                "appa_reload_policy",
                "appa_update_policy",
                "execute_remedy_plan",
            ]
        );
        let guide_schema =
            serde_json::to_string(&RuntimeToolService::tool_router().list_all()).expect("guide tool schemas serialize");
        assert!(
            guide_schema.contains("_appa_actor"),
            "the MCP client must preserve the adapter-owned actor binding"
        );
    }

    #[tokio::test]
    async fn the_mcp_service_accepts_a_kubernetes_service_host() {
        let directory = tempfile::tempdir().expect("a temp dir is creatable");
        let service = service_with_allowed_hosts(
            std::sync::Arc::new(
                Runtime::open(config(), directory.path().join("appa.db"), None).expect("the deployment opens"),
            ),
            &["appa-runtime.appa.svc.cluster.local:18787".to_string()],
            Harness::ClaudeCode,
        );

        let response = service
            .handle(
                axum::http::Request::builder()
                    .uri("/mcp")
                    .header("host", "appa-runtime.appa.svc.cluster.local:18787")
                    .body(axum::body::Body::empty())
                    .expect("the MCP request is valid"),
            )
            .await;

        assert!(
            response.status() != axum::http::StatusCode::FORBIDDEN,
            "the Kubernetes Service Host header is not rejected by rmcp"
        );

        let refused = service
            .handle(
                axum::http::Request::builder()
                    .uri("/mcp")
                    .header("host", "attacker.example:18787")
                    .body(axum::body::Body::empty())
                    .expect("the hostile MCP request is syntactically valid"),
            )
            .await;
        assert_eq!(refused.status(), axum::http::StatusCode::FORBIDDEN);
    }

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 2
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    /// The tools a deployment with this reporting posture serves.
    fn tools_with(agent_yell: bool) -> RuntimeTools {
        let text = format!(
            r#"
            [policy]
            version = 2
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
            [reporting]
            agent_yell = {agent_yell}
        "#
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        let config = Config::load(&path).expect("the fixture validates");
        let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
        RuntimeTools::new(std::sync::Arc::new(runtime), Harness::ClaudeCode)
    }

    /// A deployment that has not turned agent reporting on does not advertise the tool, and
    /// a client that calls it anyway — from a stale list, or by guessing — finds no route.
    /// The remedy tool is unaffected either way: it is the engine's, not the deployment's.
    #[test]
    fn agent_reporting_off_serves_no_yell_at_all() {
        let off = tools_with(false);
        assert!(!off.tool_router.has_route(YELL));
        assert!(!off.tool_router.list_all().iter().any(|tool| tool.name == YELL));
        assert!(off.tool_router.has_route("execute_remedy_plan"));

        let on = tools_with(true);
        assert!(on.tool_router.has_route(YELL));
        assert!(on.tool_router.list_all().iter().any(|tool| tool.name == YELL));
        assert!(on.tool_router.has_route("execute_remedy_plan"));
    }

    #[test]
    fn battery_matching_is_exact_and_deterministic() {
        let catalog = vec![BundledBattery {
            name: "github".to_string(),
            tools: vec![
                "mcp__github__get_file_contents".to_string(),
                "get_file_contents".to_string(),
                "mcp__github__issue_write(repo:public)".to_string(),
            ],
        }];

        assert_eq!(
            match_batteries(
                &catalog,
                &BTreeSet::from(["github".to_string()]),
                &[
                    "issue_write".to_string(),
                    "unrelated".to_string(),
                    "get_file_contents".to_string(),
                    "issue_write".to_string(),
                ],
            ),
            vec![BatteryMatch {
                battery: "github".to_string(),
                included: true,
                tools: vec![
                    ToolMatch {
                        observed: "get_file_contents".to_string(),
                        declarations: vec!["get_file_contents".to_string()],
                        match_kind: "exact",
                    },
                    ToolMatch {
                        observed: "issue_write".to_string(),
                        declarations: vec!["mcp__github__issue_write(repo:public)".to_string()],
                        match_kind: "suffix",
                    },
                ],
            }],
        );
        assert!(!match_batteries(&catalog, &BTreeSet::new(), &["get_file_contents".to_string()])[0].included);
        assert_eq!(
            observed_for_source(
                "team-alpha/delegations",
                &[
                    "log-analyst".to_string(),
                    "team_alpha__NS__log_analyst".to_string(),
                    "release-manager".to_string(),
                ],
            ),
            ["team_alpha__NS__log_analyst", "team_alpha__NS__release_manager"],
        );
    }

    #[test]
    fn shipped_battery_matches_native_names_without_claiming_coverage() {
        let catalog = crate::batteries::snapshot(&[
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace/batteries")
        ]);
        let names = vec!["get_file_contents".into(), "issue_write".into()];
        let matches = match_batteries(&catalog.batteries, &BTreeSet::new(), &names);
        let github = matches.iter().find(|item| item.battery == "github").unwrap();
        assert!(!github.included);
        assert_eq!(github.tools.len(), 2);
        assert!(github.tools.iter().all(|item| item.match_kind == "suffix"));
        assert!(
            github
                .tools
                .iter()
                .all(|item| item.declarations.iter().all(|name| name.starts_with("mcp/github/")))
        );

        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).unwrap();
        let (_, report) = source_coverage(&runtime, "kagent/github", None, &names).unwrap();
        assert!(
            report
                .tools
                .iter()
                .all(|check| matches!(check.status, crate::tool_validation::ToolStatus::Unknown { .. }))
        );
        let (_, report) =
            source_coverage(&runtime, "kagent/github", Some("https://github.example/mcp"), &names).unwrap();
        assert!(
            report
                .tools
                .iter()
                .all(|check| matches!(check.status, crate::tool_validation::ToolStatus::Invalid { .. }))
        );
    }

    #[test]
    fn coverage_uses_policy_bindings_and_explicit_delegation_contracts() {
        use crate::tool_validation::ToolStatus;
        let endpoint = "https://github.example/mcp";
        let server = "server-8f9bc50cf907a42a97f5dd5a81e428c594a34d0713e83ac279a5be860a4db652";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appa.toml");
        std::fs::write(
            &path,
            format!(
                r#"
            [server_aliases]
            github = ["{server}"]
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
            [policy]
            version = 2
            [[policy.tool]]
            name = "mcp/github/get_file_contents"
            [[policy.tool]]
            name = "list_pods"
            [[policy.tool]]
            name = "agent/kagent/log-analyst"
        "#
            ),
        )
        .unwrap();
        let runtime = Runtime::open(Config::load(&path).unwrap(), dir.path().join("appa.db"), None).unwrap();
        let names = vec!["get_file_contents".into(), "list_pods".into()];
        let (identity, covered) = source_coverage(&runtime, "kagent/github", Some(endpoint), &names).unwrap();
        assert_eq!(identity.as_deref(), Some(server));
        assert!(
            covered
                .tools
                .iter()
                .filter(|check| names.contains(&check.tool))
                .all(|check| check.status == ToolStatus::Valid)
        );
        let (_, other) = source_coverage(&runtime, "kagent/other", Some("https://other.example/mcp"), &names).unwrap();
        assert!(
            other
                .tools
                .iter()
                .any(|check| check.tool == "get_file_contents" && matches!(check.status, ToolStatus::Invalid { .. }))
        );
        assert!(
            other
                .tools
                .iter()
                .any(|check| check.tool == "list_pods" && check.status == ToolStatus::Valid)
        );
        let agents = observed_for_source("kagent/delegations", &["log-analyst".into(), "release-manager".into()]);
        let (_, report) = source_coverage(&runtime, "kagent/delegations", None, &agents).unwrap();
        assert!(
            report
                .tools
                .iter()
                .any(|check| check.tool == "kagent__NS__log_analyst" && check.status == ToolStatus::Valid)
        );
        assert!(
            report
                .tools
                .iter()
                .any(|check| check.tool == "kagent__NS__release_manager"
                    && matches!(check.status, ToolStatus::Invalid { .. }))
        );
        assert!(source_coverage(&runtime, "other/delegations", None, &agents).is_err());
    }

    #[tokio::test]
    async fn a_direct_management_call_without_a_hook_vouch_is_refused() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let runtime = Arc::new(Runtime::open(config(), dir.path().join("appa.db"), None).expect("the runtime opens"));
        let result = RuntimeToolService::new(runtime)
            .appa_include_battery(Parameters(IncludeBatteryArgs {
                actor: "direct".to_string(),
                battery: "github".to_string(),
                expected_policy_key: "unvouched".to_string(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(format!("{:?}", result.content).contains("one-shot APPA vouch"));

        let runtime =
            Arc::new(Runtime::open(config(), dir.path().join("second.db"), None).expect("the second runtime opens"));
        let service = RuntimeToolService::new(runtime);
        assert_eq!(
            service
                .appa_get_runtime_state(Parameters(RuntimeStateArgs {
                    actor: "direct".to_string(),
                }))
                .await
                .is_error,
            Some(true)
        );
        assert_eq!(
            service
                .appa_match_batteries(Parameters(MatchBatteriesArgs {
                    actor: "direct".to_string(),
                    source: "server".to_string(),
                    endpoint: None,
                    tools: vec!["tool".to_string()],
                }))
                .await
                .is_error,
            Some(true)
        );
    }

    #[test]
    fn a_kagent_actor_can_consume_only_its_prefixed_management_vouch() {
        let dir = tempfile::tempdir().expect("a temp directory");
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the runtime opens");
        let root = crate::api::TrajectoryId("kagent:s1".to_string());
        runtime.create_session(root.clone()).expect("a fresh id opens");
        let actor = crate::api::Actor { root, child: None };
        let args = IncludeBatteryArgs {
            actor: "s1".to_string(),
            battery: "github".to_string(),
            expected_policy_key: "key".to_string(),
        };
        let call = ProposedCall {
            tool: "appa_include_battery".to_string(),
            arguments: serde_json::value::to_raw_value(&args).expect("arguments serialize"),
            cwd: None,
        };
        runtime.vouch(&crate::api::call_key(&call).expect("a management call"), &actor, None);
        assert!(take_management_vouch(&runtime, "appa_include_battery", &args));
    }

    #[tokio::test]
    async fn an_unknown_offer_gets_one_typed_refusal() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the fixture deployment opens");
        let outcome = runtime
            .execute_remedy(&acting("cc:mcp-test"), OfferId("never-surfaced".to_string()))
            .await;
        assert_eq!(
            outcome,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::UnknownOffer,
            },
        );
        assert_eq!(remedy_result(runtime.render_remedy(outcome)).is_error, Some(true));
    }

    /// One offer executes at a time, and what says so is a record: an execution that never
    /// returned — a runtime killed mid-review — keeps the offer refused until its claim's
    /// bound passes, and the offer answers again after it.
    #[tokio::test]
    async fn one_offer_is_claimed_once_at_a_time() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let (runtime, root, quoted) = blocked_offer(&dir).await;
        let actor = acting(root.0.as_str());
        let already = RemedyOutcome::Refused {
            reason: RemedyRefusal::AlreadyExecuting,
        };

        let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(600);
        runtime.claim_until(&actor, &quoted, ahead);
        assert_eq!(
            runtime.execute_remedy(&actor, quoted.clone()).await,
            already,
            "a claim that still stands refuses the second execution"
        );

        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        runtime.claim_until(&actor, &quoted, past);
        let outcome = runtime.execute_remedy(&actor, quoted).await;
        assert_ne!(outcome, already, "a claim past its bound holds nothing: {outcome:?}");
        assert!(
            matches!(outcome, RemedyOutcome::Authorized { .. }),
            "the offer is executed: {outcome:?}"
        );
    }

    /// A family with one offer standing: the narrowing acceptance a blocked proposal
    /// surfaced, which the model can execute unaided.
    async fn blocked_offer(dir: &tempfile::TempDir) -> (Runtime, crate::api::TrajectoryId, OfferId) {
        let policy = r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "host/claude-code/Read"
            delta = { audience = ["hr"] }

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, policy).expect("the fixture writes");
        let runtime = Runtime::open_served(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )
        .expect("the served deployment opens");
        let root = crate::api::TrajectoryId("cc:claimed".to_string());
        let session = runtime.create_session(root.clone()).expect("a fresh id opens");
        let denied = session
            .on_tool_call(
                ProposedCall {
                    tool: "host/claude-code/Read".to_string(),
                    arguments: raw(serde_json::json!({})),
                    cwd: None,
                },
                false,
            )
            .await
            .expect("the block is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }), "{denied:?}");
        let offer = runtime
            .minted_offers(&root, &root)
            .into_iter()
            .next()
            .expect("the block surfaced an offer");
        (runtime, root, offer)
    }

    /// An execution ends when the act that claimed the offer ends, and a client that walks
    /// away ends it between two awaits, reaching no exit of its own. The claim's bound is
    /// the hard-crash backstop, so a dropped request must not hold the offer for it.
    #[tokio::test]
    async fn a_dropped_execution_frees_the_offer_it_claimed() {
        // An authority that accepts the connection and never answers, so the execution is
        // still waiting when the caller goes away.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback port binds");
        let url = format!("http://{}/authority", listener.local_addr().expect("the address reads"));
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((connection, _)) = listener.accept().await {
                held.push(connection);
            }
        });
        let (runtime, root, quoted, _dir) = blocked_under(&url, "cc:mcp-dropped", 4000).await;
        let actor = acting(root.0.as_str());
        let already = RemedyOutcome::Refused {
            reason: RemedyRefusal::AlreadyExecuting,
        };

        let mut executing = Box::pin(runtime.execute_remedy(&actor, quoted.clone()));
        let waiting = tokio::time::timeout(std::time::Duration::from_millis(500), &mut executing).await;
        assert!(waiting.is_err(), "the execution is still waiting on its authority");
        assert_eq!(
            runtime.execute_remedy(&actor, quoted.clone()).await,
            already,
            "while it runs, a second call naming the same offer is refused"
        );

        drop(executing);
        let outcome = runtime.execute_remedy(&actor, quoted).await;
        assert_ne!(
            outcome, already,
            "the abandoned execution released its claim: {outcome:?}"
        );
    }

    /// The one served-deployment fixture: a policy that narrows on the tool it names, so
    /// the first proposal blocks with an acceptance the model can execute unaided.
    async fn authorized_under(adapter: appa_runtime_api::Adapter, root: &str, tool: &str) -> String {
        let policy = format!(
            r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "{tool}"
            delta = {{ audience = ["hr"] }}

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
            "#
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, policy).expect("the fixture writes");
        let config = Config::load(&path).expect("the fixture validates");
        let runtime = Runtime::open_served(config, dir.path().join("appa.db"), None, adapter)
            .expect("the served deployment opens");

        let root = crate::api::TrajectoryId(root.to_string());
        let session = runtime.create_session(root.clone()).expect("a fresh id opens");
        let denied = session
            .on_tool_call(
                ProposedCall {
                    tool: tool.to_string(),
                    arguments: raw(serde_json::json!({})),
                    cwd: None,
                },
                false,
            )
            .await
            .expect("the block is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }));
        let offer = runtime
            .minted_offers(&root, &root)
            .into_iter()
            .next()
            .expect("the block surfaced an offer");
        let outcome = runtime.execute_remedy(&acting(root.0.as_str()), offer).await;
        assert!(
            matches!(outcome, RemedyOutcome::Authorized { .. }),
            "accepting the narrowing releases the call: {outcome:?}"
        );
        let rendered = remedy_result(runtime.render_remedy(outcome));
        rendered
            .content
            .first()
            .and_then(|block| block.as_text())
            .map(|text| text.text.clone())
            .expect("the answer is one text block")
    }

    /// The canonical identity keys the released call, and no host advertises it. What the
    /// model is told to call again is its own harness's spelling.
    #[tokio::test]
    async fn an_authorized_remedy_names_the_tool_the_host_dispatches() {
        for (adapter, root, tool, dispatched) in [
            (
                appa_adapter_claude_code::adapter(),
                "cc:mcp-served",
                "host/claude-code/Read",
                "Read",
            ),
            (
                appa_adapter_claude_code::adapter(),
                "cc:mcp-served-mcp",
                "mcp/github/create_issue",
                "mcp__github__create_issue",
            ),
            (
                appa_adapter_kagent::adapter(),
                "kagent:mcp-served",
                "mcp/k8s/get_pods",
                "mcp:k8s/get_pods",
            ),
            (
                appa_adapter_kagent::adapter(),
                "kagent:mcp-served-builtin",
                "host/kagent/memory_persist",
                "builtin:memory_persist",
            ),
        ] {
            let text = authorized_under(adapter, root, tool).await;
            assert!(
                text.contains(dispatched) && !text.contains(tool),
                "the model is told to call {dispatched}, not {tool}: {text}"
            );
        }
    }

    /// An embedded host names its own tools, so what the runtime recorded is already what
    /// its model calls and nothing is spelled away.
    #[test]
    fn an_embedded_deployment_addresses_the_model_by_the_recorded_name() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the deployment opens");
        for recorded in ["wire", "host/claude-code/Read", "mcp/k8s/get_pods"] {
            assert_eq!(runtime.model_spelling(recorded), recorded);
        }
    }

    fn acting(trajectory: &str) -> crate::api::Actor {
        crate::api::Actor {
            root: crate::api::TrajectoryId(trajectory.to_string()),
            child: None,
        }
    }

    async fn blocked_deployment() -> (
        crate::api::Runtime,
        crate::api::TrajectoryId,
        OfferId,
        tempfile::TempDir,
    ) {
        // Nothing listens on port 1, so the authority answers nothing at once.
        blocked_under("http://127.0.0.1:1/authority", "cc:mcp-test", 1000).await
    }

    /// The same family, under an authority this deployment consults at `url` with `timeout`
    /// to bound it.
    async fn blocked_under(
        url: &str,
        root: &str,
        timeout_ms: u64,
    ) -> (
        crate::api::Runtime,
        crate::api::TrajectoryId,
        OfferId,
        tempfile::TempDir,
    ) {
        let policy = format!(
            r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "wire"
            parameters = {{ type = "object", properties = {{ amount = {{ type = "integer" }} }} }}
            requires = {{ attention = ["irreversible"] }}
            delta = {{}}

            [[policy.authority]]
            name = "approver"
            [policy.authority.permits]
            attention = ["irreversible"]

            [externals]
            timeout_ms = {timeout_ms}
            max_body_bytes = 4096

            [externals.authorities.approver]
            url = "{url}"
        "#
        );
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, policy).expect("the fixture writes");
        let config = Config::load(&path).expect("the fixture validates");
        let runtime =
            crate::api::Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

        let root = crate::api::TrajectoryId(root.to_string());
        let session = runtime.create_session(root.clone()).expect("a fresh id opens");
        let denied = session
            .on_tool_call(
                ProposedCall {
                    tool: "wire".to_string(),
                    arguments: raw(serde_json::json!({"amount": 500})),
                    cwd: None,
                },
                false,
            )
            .await
            .expect("the block is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }));

        let offer = runtime
            .minted_offers(&root, &root)
            .into_iter()
            .next()
            .expect("the block surfaced an offer");
        (runtime, root, offer, dir)
    }

    #[tokio::test]
    async fn a_surfaced_offer_executes_for_its_own_trajectory_and_no_other() {
        let (runtime, root, offer, _dir) = blocked_deployment().await;
        assert!(
            !offer.0.contains(root.0.as_str()),
            "the quoted id carries no trajectory: {}",
            offer.0,
        );
        assert_eq!(
            runtime.execute_remedy(&acting("cc:mcp-stranger"), offer.clone()).await,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::UnknownOffer,
            },
            "an offer executes only for the trajectory that pursues it",
        );
        assert!(matches!(
            runtime.execute_remedy(&acting(root.0.as_str()), offer).await,
            RemedyOutcome::Declined { .. } | RemedyOutcome::NoAnswer { .. },
        ));
    }

    #[tokio::test]
    async fn a_control_act_is_vouched_for_only_by_the_trajectory_that_pursues_it() {
        let (runtime, root, quoted, _dir) = blocked_deployment().await;

        let stranger = crate::hooks::handle(&runtime, control_act(&acting("cc:mcp-stranger"), &quoted)).await;
        assert!(
            matches!(stranger, HookDecision::DenyCall { .. }),
            "another trajectory's quote is refused where the harness names it: {stranger:?}",
        );
        assert!(
            runtime.take_vouched(&PermitKey::offer(&quoted)).is_err(),
            "a refused control act vouches for nobody"
        );

        for _ in 0..2 {
            let admitted = crate::hooks::handle(&runtime, control_act(&acting(root.0.as_str()), &quoted)).await;
            assert!(matches!(admitted, HookDecision::PassControl), "got {admitted:?}");
        }
        assert_eq!(
            runtime.take_vouched(&PermitKey::offer(&quoted)),
            Ok((acting(root.0.as_str()), None)),
            "a repeated hook is one caller"
        );
        assert!(runtime.take_vouched(&PermitKey::offer(&quoted)).is_err());

        let admitted = crate::hooks::handle(&runtime, control_act(&acting(root.0.as_str()), &quoted)).await;
        assert!(matches!(admitted, HookDecision::PassControl));
        let _ = runtime.execute_remedy(&acting(root.0.as_str()), quoted.clone()).await;
        assert!(
            runtime.take_vouched(&PermitKey::offer(&quoted)).is_err(),
            "executing the act spends its vouch on every transport"
        );
    }

    #[tokio::test]
    async fn an_embedded_remedy_keeps_the_vouch_and_actor_checks_typed() {
        let (runtime, root, quoted, _dir) = blocked_deployment().await;
        let owner = acting(root.0.as_str());
        let stranger = acting("cc:mcp-embedded-stranger");
        let args = ExecuteRemedyPlanArgs {
            offer_id: quoted.0.clone(),
            label: None,
            return_schema: None,
        };

        assert!(matches!(
            crate::hooks::handle(&runtime, control_act(&owner, &quoted)).await,
            HookDecision::PassControl
        ));
        assert_eq!(
            runtime.execute_embedded_remedy(&stranger, args).await,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::ActorMismatch,
            },
            "a stranger is refused before the owner's vouch is spent"
        );
        let outcome = runtime
            .execute_embedded_remedy(
                &owner,
                ExecuteRemedyPlanArgs {
                    offer_id: quoted.0,
                    label: None,
                    return_schema: None,
                },
            )
            .await;
        assert!(matches!(
            outcome,
            RemedyOutcome::Declined { .. } | RemedyOutcome::NoAnswer { .. }
        ));
    }

    #[tokio::test]
    async fn malformed_embedded_offer_ids_refuse_without_spending_a_valid_vouch() {
        let (runtime, root, quoted, _dir) = blocked_deployment().await;
        let actor = acting(root.0.as_str());
        assert!(matches!(
            crate::hooks::handle(&runtime, control_act(&actor, &quoted)).await,
            HookDecision::PassControl
        ));

        assert_eq!(
            runtime
                .execute_embedded_remedy(
                    &actor,
                    ExecuteRemedyPlanArgs {
                        offer_id: "not-an-offer".to_owned(),
                        label: None,
                        return_schema: None,
                    },
                )
                .await,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::InvalidOfferId(crate::api::OfferIdRefusal::WrongLength),
            }
        );

        let outcome = runtime
            .execute_embedded_remedy(
                &actor,
                ExecuteRemedyPlanArgs {
                    offer_id: quoted.0,
                    label: None,
                    return_schema: None,
                },
            )
            .await;
        assert!(matches!(
            outcome,
            RemedyOutcome::Declined { .. } | RemedyOutcome::NoAnswer { .. }
        ));
    }

    #[tokio::test]
    async fn a_fresh_embedded_invocation_does_not_replay_a_spent_offer() {
        let dir = tempfile::tempdir().expect("a test directory");
        let path = dir.path().join("appa.toml");
        std::fs::write(
            &path,
            r#"
[policy]
version = 2
[[policy.tool]]
name = "read"
delta = { trust = "suspicious" }
[policy.deployment]
context_control = true
[externals]
timeout_ms = 1000
max_body_bytes = 4096
"#,
        )
        .expect("write the fixture");
        let runtime = Runtime::open(
            Config::load(&path).expect("valid policy"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("open the runtime");
        let actor = acting("cc:embedded-fresh-offer");
        let session = runtime.create_session(actor.root.clone()).expect("create the root");
        assert!(matches!(
            session
                .on_tool_call(
                    ProposedCall {
                        tool: "read".to_owned(),
                        arguments: raw(serde_json::json!({})),
                        cwd: None,
                    },
                    false
                )
                .await
                .expect("propose the read"),
            ToolCallDecision::Deny { .. }
        ));
        let offer = runtime
            .minted_offers(&actor.root, &actor.root)
            .into_iter()
            .next()
            .expect("an offered narrowing");
        let args = || ExecuteRemedyPlanArgs {
            offer_id: offer.0.clone(),
            label: None,
            return_schema: None,
        };
        assert!(matches!(
            crate::hooks::handle(&runtime, control_act(&actor, &offer)).await,
            HookDecision::PassControl
        ));
        assert!(matches!(
            runtime.execute_embedded_remedy(&actor, args()).await,
            RemedyOutcome::Authorized { .. }
        ));
        assert!(matches!(
            crate::hooks::handle(&runtime, control_act(&actor, &offer)).await,
            HookDecision::PassControl
        ));
        assert_eq!(
            runtime.execute_embedded_remedy(&actor, args()).await,
            RemedyOutcome::Refused {
                reason: RemedyRefusal::UnknownOffer
            }
        );
    }

    #[tokio::test]
    async fn embedded_call_denials_expose_offers_without_reading_feedback() {
        let (runtime, root, _quoted, _dir) = blocked_deployment().await;
        let actor = acting(root.0.as_str());
        let outcome = crate::hooks::handle_embedded(
            &runtime,
            appa_runtime_api::HookEvent::ToolCall {
                actor,
                call: ProposedCall {
                    tool: "wire".to_string(),
                    arguments: raw(serde_json::json!({"amount": 501})),
                    cwd: None,
                },
                call_id: None,
                spawn: false,
                ruling: None,
            },
        )
        .await;
        let HookDecision::DenyCall { offers, .. } = &outcome.decision else {
            panic!("the policy must block the call: {:?}", outcome.decision);
        };
        let presentation = outcome.presentation.expect("a denial carries typed offers");
        assert_eq!(presentation.offers, *offers);
        assert!(!presentation.offers.is_empty());
    }

    #[tokio::test]
    async fn embedded_presentation_options_are_scoped_to_each_hook_request() {
        let (runtime, root, _quoted, _dir) = blocked_deployment().await;
        let event = |amount| appa_runtime_api::HookEvent::ToolCall {
            actor: acting(root.0.as_str()),
            call: ProposedCall {
                tool: "wire".to_string(),
                arguments: raw(serde_json::json!({"amount": amount})),
                cwd: None,
            },
            call_id: None,
            spawn: false,
            ruling: None,
        };
        let first = crate::hooks::handle_embedded_with_options(
            &runtime,
            event(501),
            EmbeddedPresentationOptions {
                control_tool: "client_one_remedy".to_owned(),
                supports_delegation: false,
                include_display_plan: false,
            },
        )
        .await;
        let second = crate::hooks::handle_embedded_with_options(
            &runtime,
            event(502),
            EmbeddedPresentationOptions {
                control_tool: "client_two_remedy".to_owned(),
                supports_delegation: false,
                include_display_plan: false,
            },
        )
        .await;
        let feedback = |outcome: crate::api::EmbeddedHookOutcome| match outcome.decision {
            HookDecision::DenyCall { feedback, .. } => feedback,
            decision => panic!("the policy must block the call: {decision:?}"),
        };
        let first = feedback(first);
        let second = feedback(second);
        assert!(first.contains("client_one_remedy(offer_id"));
        assert!(!first.contains("client_two_remedy"));
        assert!(second.contains("client_two_remedy(offer_id"));
        assert!(!second.contains("client_one_remedy"));
    }

    #[test]
    fn rendering_a_typed_remedy_outcome_does_not_change_its_state() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the runtime opens");
        let outcome = RemedyOutcome::Refused {
            reason: RemedyRefusal::ActorMismatch,
        };
        let original = outcome.clone();
        let rendered = runtime.render_remedy(outcome.clone());
        assert_eq!(outcome, original);
        assert!(rendered.is_error);
        assert!(rendered.text.contains("different session"));
    }

    #[test]
    fn typed_offer_ids_reject_non_rendered_spellings() {
        assert!(OfferId::parse("0123456789abcdef").is_ok());
        assert!(OfferId::parse("0123456789ABCDEF").is_err());
        assert!(OfferId::parse("0123456789abcde").is_err());
    }

    /// A network caller names the offer id, so an id no minting could have produced is
    /// refused on its spelling — before the store is searched for a key that cannot be in
    /// it. The armed read failure is the evidence: a take that searched would spend it.
    #[tokio::test]
    async fn an_offer_id_this_runtime_never_renders_is_refused_without_asking_the_store() {
        let (runtime, root, quoted, _dir) = blocked_deployment().await;
        assert!(
            crate::engine::renders_offer(&quoted),
            "the id the model is shown is the shape the check admits: {}",
            quoted.0,
        );
        let admitted = crate::hooks::handle(&runtime, control_act(&acting(root.0.as_str()), &quoted)).await;
        assert!(matches!(admitted, HookDecision::PassControl), "got {admitted:?}");

        runtime.store().fail_next_reads(1);
        for spelling in [
            "",
            "not hex at all!",
            "0123456789abcde",
            "0123456789abcdef0",
            "0123456789ABCDEF",
        ] {
            assert_eq!(
                runtime.take_vouched(&PermitKey::offer(&OfferId(spelling.to_string()))),
                Err(crate::api::Unvouched::Nobody),
                "an id shaped like {spelling:?} names no offer",
            );
        }
        assert_eq!(
            runtime.take_vouched(&PermitKey::offer(&quoted)),
            Err(crate::api::Unvouched::Nobody),
            "the read that was armed to fail was still waiting for the first take that read at all",
        );
        assert_eq!(
            runtime.take_vouched(&PermitKey::offer(&quoted)),
            Ok((acting(root.0.as_str()), None)),
            "and the standing none of the refused takes reached is still there to spend",
        );
    }

    fn control_act(actor: &crate::api::Actor, quoted: &OfferId) -> appa_runtime_api::HookEvent {
        appa_runtime_api::HookEvent::ToolCall {
            actor: actor.clone(),
            call: ProposedCall {
                tool: appa_runtime_api::CONTROL_TOOL.to_string(),
                arguments: raw(serde_json::json!({ "offer_id": quoted.0 })),
                cwd: None,
            },
            call_id: None,
            spawn: false,
            ruling: None,
        }
    }
}
