//! Runtime-owned MCP tools, provided identically to every harness.
//!
//! `execute_remedy_plan` carries only the quoted id and no trajectory, so
//! the trajectory comes from the hook that preceded the call. A request no
//! hook vouched for is refused. `appa_match_batteries` is read-only: it
//! intersects host-observed names with the runtime's current catalog.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::api::{LabelSpelling, OfferId, RemedyArguments, RemedyOutcome, Runtime};
use crate::batteries::{BatteriesResponse, BundledBattery};
use crate::elicit::Elicitation;

#[derive(Clone)]
pub struct RuntimeToolService {
    runtime: Arc<Runtime>,
    battery_state: Arc<RwLock<BatteryState>>,
}

#[derive(Clone)]
pub struct RemedyService {
    runtime: Arc<Runtime>,
}

#[derive(Debug, Clone)]
pub struct BatteryState {
    pub catalog: BatteriesResponse,
    pub included: BTreeSet<String>,
    pub serving_tools: BTreeSet<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteRemedyPlanArgs {
    pub offer_id: String,
    /// For a plan that declares a subagent's return: the lowest label this session accepts
    /// from the return, in the policy's `delta` spelling. An omitted dimension keeps this
    /// session's current value.
    #[serde(default)]
    pub label: Option<LabelArgs>,
    /// For a plan that attests a subagent's return: the JSON schema the return must match.
    #[serde(default)]
    pub return_schema: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LabelArgs {
    #[serde(default)]
    pub trust: Option<String>,
    #[serde(default)]
    pub audience: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct MatchBatteriesArgs {
    #[serde(default, rename = "_appa_actor")]
    /// Injected by the appa adapter. Model-provided values are overwritten.
    pub actor: String,
    /// The observed server or host source these names came from.
    pub source: String,
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

impl From<ExecuteRemedyPlanArgs> for RemedyArguments {
    fn from(args: ExecuteRemedyPlanArgs) -> RemedyArguments {
        RemedyArguments {
            label: args.label.map(|label| LabelSpelling {
                trust: label.trust,
                audience: label.audience,
            }),
            return_schema: args.return_schema,
        }
    }
}

async fn execute_remedy(
    runtime: &Runtime,
    args: ExecuteRemedyPlanArgs,
    request: RequestContext<RoleServer>,
) -> CallToolResult {
    let quoted = OfferId(args.offer_id.clone());
    let arguments = RemedyArguments::from(args);
    let Some((acting, ruling)) = runtime.take_vouched(&quoted) else {
        return render(RemedyOutcome::Refused {
            detail: "no live offer with this id exists".to_string(),
        });
    };
    let elicitation = Elicitation::new(request, runtime.review_timeout());
    render(
        runtime
            .remedy(&acting, quoted, arguments, Some(&elicitation), ruling)
            .await,
    )
}

#[tool_router]
impl RemedyService {
    pub fn new(runtime: Arc<Runtime>) -> RemedyService {
        RemedyService { runtime }
    }

    #[tool(description = "Execute one remedy plan by the offer id that blocking \
                       feedback surfaced. The id must be quoted exactly. A plan that \
                       declares a subagent's return takes `label`, the lowest label this \
                       session accepts from the return; a plan that attests it also takes \
                       `return_schema`. After execution succeeds, re-call the original \
                       tool or receive the admitted output.")]
    pub async fn execute_remedy_plan(
        &self,
        Parameters(args): Parameters<ExecuteRemedyPlanArgs>,
        request: RequestContext<RoleServer>,
    ) -> CallToolResult {
        execute_remedy(&self.runtime, args, request).await
    }
}

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
                       `return_schema`. After execution succeeds, re-call the original \
                       tool or receive the admitted output.")]
    pub async fn execute_remedy_plan(
        &self,
        Parameters(args): Parameters<ExecuteRemedyPlanArgs>,
        request: RequestContext<RoleServer>,
    ) -> CallToolResult {
        execute_remedy(&self.runtime, args, request).await
    }

    #[tool(
        description = "Match exact tool wire names observed from one named host source against the runtime's current battery catalog and report whether each match is already included. Exact aliases win over host-qualified suffix matches. Use this result as the only source of battery suggestions."
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
        let unconfigured_tools = unconfigured_tools(&state.serving_tools, &observed);
        let body = serde_json::to_string(&serde_json::json!({
            "source": args.source,
            "matches": matches,
            "serving_tools": &state.serving_tools,
            "unconfigured_tools": unconfigured_tools,
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

fn take_management_vouch<T: ManagementArguments>(runtime: &Runtime, tool: &str, arguments: &T) -> bool {
    runtime.take_management_vouch(tool, arguments).is_some_and(|actor| {
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
                .rsplit_once("__")
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

fn unconfigured_tools(serving: &BTreeSet<String>, observed: &[String]) -> Vec<String> {
    let configured: BTreeSet<&str> = serving.iter().map(|tool| selector_name(tool)).collect();
    observed
        .iter()
        .map(String::as_str)
        .filter(|tool| !tool.is_empty() && !configured.contains(tool))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_owned)
        .collect()
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

fn render(outcome: RemedyOutcome) -> CallToolResult {
    match outcome {
        RemedyOutcome::Authorized { call } => CallToolResult::success(vec![ContentBlock::text(format!(
            "[appa] Authorized. Call the {} tool again with exactly these arguments: {}",
            call.tool,
            call.arguments.get(),
        ))]),
        RemedyOutcome::Substituted { call } => CallToolResult::success(vec![ContentBlock::text(format!(
            "[appa] Substituted. The sanitizer replaced the arguments and the call is released. \
             Call the {} tool with exactly these arguments to run it: {}",
            call.tool,
            call.arguments.get(),
        ))]),
        RemedyOutcome::Returned { value } => CallToolResult::success(vec![ContentBlock::text(value)]),
        RemedyOutcome::Declined { feedback } | RemedyOutcome::NoAnswer { feedback } => {
            CallToolResult::success(vec![ContentBlock::text(feedback)])
        }
        RemedyOutcome::Refused { detail } => CallToolResult::error(vec![ContentBlock::text(detail)]),
    }
}

#[tool_handler]
impl ServerHandler for RuntimeToolService {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
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

#[tool_handler]
impl ServerHandler for RemedyService {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
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

/// MCP service served at `/mcp`.
pub fn service(runtime: Arc<Runtime>) -> StreamableHttpService<RemedyService, LocalSessionManager> {
    service_with_allowed_hosts(runtime, &[])
}

pub fn service_with_allowed_hosts(
    runtime: Arc<Runtime>,
    allowed_hosts: &[String],
) -> StreamableHttpService<RemedyService, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(runtime.review_timeout() + SESSION_GRACE);
    let config = server_config(allowed_hosts);
    StreamableHttpService::new(
        move || Ok(RemedyService::new(Arc::clone(&runtime))),
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
    use crate::api::{ProposedCall, Runtime, ToolCallDecision};
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
        let service = RemedyService::new(Arc::clone(&runtime));
        assert_eq!(service.get_info().server_info.version, env!("CARGO_PKG_VERSION"));
        let remedy_tools: Vec<String> = RemedyService::tool_router()
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
            unconfigured_tools(
                &BTreeSet::from(["list_pods".to_string(), "read_secret(name:payments*)".to_string()]),
                &[
                    "issue_write".to_string(),
                    "read_secret".to_string(),
                    "list_pods".to_string()
                ],
            ),
            ["issue_write"],
        );
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
        let actor = crate::api::Actor {
            root: crate::api::TrajectoryId("kagent:s1".to_string()),
            child: None,
        };
        let args = IncludeBatteryArgs {
            actor: "s1".to_string(),
            battery: "github".to_string(),
            expected_policy_key: "key".to_string(),
        };
        runtime.vouch_management(
            &ProposedCall {
                tool: "appa_include_battery".to_string(),
                arguments: serde_json::value::to_raw_value(&args).expect("arguments serialize"),
            },
            &actor,
        );
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
                detail: "no live offer with this id exists".to_string(),
            },
        );
        assert_eq!(render(outcome).is_error, Some(true));
    }

    #[tokio::test]
    async fn one_offer_is_claimed_once_at_a_time() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config(), dir.path().join("appa.db"), None).expect("the fixture deployment opens");
        let offer = OfferId("offer-live".to_string());

        let claim = runtime.claim_offer(&offer).expect("a free offer is claimable");
        assert!(
            runtime.claim_offer(&offer).is_none(),
            "a second execution of one offer is refused"
        );

        drop(claim);
        assert!(
            runtime.claim_offer(&offer).is_some(),
            "the offer is claimable again once its execution ended"
        );
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
        let policy = r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "wire"
            parameters = { type = "object", properties = { amount = { type = "integer" } } }
            requires = { attention = ["irreversible"] }
            delta = {}

            [[policy.authority]]
            name = "approver"
            [policy.authority.permits]
            attention = ["irreversible"]

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096

            [externals.authorities.approver]
            url = "http://127.0.0.1:1/authority"
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, policy).expect("the fixture writes");
        let config = Config::load(&path).expect("the fixture validates");
        let runtime =
            crate::api::Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");

        let root = crate::api::TrajectoryId("cc:mcp-test".to_string());
        let session = runtime.create_session(root.clone()).expect("a fresh id opens");
        let denied = session
            .on_tool_call(
                ProposedCall {
                    tool: "wire".to_string(),
                    arguments: raw(serde_json::json!({"amount": 500})),
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
                detail: "no live offer with this id exists".to_string(),
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
            runtime.take_vouched(&quoted).is_none(),
            "a refused control act vouches for nobody"
        );

        for _ in 0..2 {
            let admitted = crate::hooks::handle(&runtime, control_act(&acting(root.0.as_str()), &quoted)).await;
            assert!(matches!(admitted, HookDecision::PassControl), "got {admitted:?}");
        }
        assert_eq!(
            runtime.take_vouched(&quoted),
            Some((acting(root.0.as_str()), None)),
            "a repeated hook is one caller"
        );
        assert!(runtime.take_vouched(&quoted).is_none());

        let admitted = crate::hooks::handle(&runtime, control_act(&acting(root.0.as_str()), &quoted)).await;
        assert!(matches!(admitted, HookDecision::PassControl));
        let _ = runtime.execute_remedy(&acting(root.0.as_str()), quoted.clone()).await;
        assert!(
            runtime.take_vouched(&quoted).is_none(),
            "executing the act spends its vouch on every transport"
        );
    }

    fn control_act(actor: &crate::api::Actor, quoted: &OfferId) -> appa_runtime_api::HookEvent {
        appa_runtime_api::HookEvent::ToolCall {
            actor: actor.clone(),
            call: ProposedCall {
                tool: "execute_remedy_plan".to_string(),
                arguments: raw(serde_json::json!({ "offer_id": quoted.0 })),
            },
            spawn: false,
            ruling: None,
        }
    }
}
