//! The runtime's own MCP endpoint: `execute_remedy_plan`, the engine's reserved tool, and
//! `yell`, which reports that APPA is in the way. Both are runtime-provided and identical
//! for every harness.
//!
//! Served over streamable HTTP from process start. Neither request carries a trajectory, so
//! both take it from the hook that preceded the call — the one place the harness names it.
//! A request no hook vouched for is refused. For a remedy the engine then judges from the
//! log whether the offer still stands: an unknown, unpursued, cross-turn, or terminal id is
//! refused.
//!
//! `yell` is advertised only where the deployment turned agent reporting on. A build that
//! does not advertise it does not route it either, so a client holding a stale tool list
//! gets the same answer as a client that invented the name.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::api::{LabelSpelling, OfferId, PermitKey, RemedyArguments, RemedyOutcome, Runtime};
use crate::elicit::Elicitation;
use crate::runtime_cli::Adapter;
use crate::yell::YellArgs;

/// The tools this runtime serves, and the router that decides which of them exist for this
/// session. Built per session, so a `/reload` that turns agent reporting on or off settles
/// the next client rather than the next restart.
#[derive(Clone)]
pub struct RuntimeTools {
    runtime: Arc<Runtime>,
    harness: Adapter,
    tool_router: ToolRouter<Self>,
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

#[tool_router]
impl RuntimeTools {
    /// The tools this deployment serves right now. `yell` is dropped from the router where
    /// the deployment has not turned agent reporting on, which is both how it stops being
    /// advertised and how a call to it stops being routed.
    pub fn new(runtime: Arc<Runtime>, harness: Adapter) -> RuntimeTools {
        let mut tool_router = Self::tool_router();
        if !runtime.agent_yell() {
            tool_router.remove_route(YELL);
        }
        RuntimeTools {
            runtime,
            harness,
            tool_router,
        }
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
        let quoted = OfferId(args.offer_id.clone());
        let arguments = RemedyArguments::from(args);
        // Requires the vouched trajectory from the preceding hook.
        let Ok((acting, ruling)) = self.runtime.take_vouched(&PermitKey::offer(&quoted)) else {
            return render(RemedyOutcome::Refused {
                detail: "no live offer with this id exists".to_string(),
            });
        };
        let elicitation = Elicitation::new(request, self.runtime.review_timeout());
        let started = std::time::Instant::now();
        let outcome = self
            .runtime
            .remedy(&acting, quoted.clone(), arguments, Some(&elicitation), ruling)
            .await;
        // Recorded from the typed outcome, before `render` turns it into the text the model
        // reads: a remedy that takes a minute and then declines is the shape of "APPA is in
        // the way", and neither the duration nor the offer it quoted is in the trajectory.
        self.runtime.record(
            // The family, never the acting trajectory: see `Session::timed_consult`.
            Some(&acting.root),
            crate::events::RuntimeEvent::Control {
                call: crate::events::ControlCall::Remedy {
                    offer: quoted.0,
                    dispatch: None,
                },
                outcome: (&outcome).into(),
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
        );
        render(outcome)
    }

    #[tool(description = "Report to the OpenAPPA team that APPA is broken, confusing, or in \
                       the way. Say in `message` what you were trying to do and what APPA \
                       did. Set `with_trajectory` to include this session's APPA decisions \
                       — its rulings, remedies and label changes, never your prompts, tool \
                       arguments or tool outputs — or leave it false to report on the \
                       policy alone.")]
    pub(crate) async fn yell(&self, Parameters(args): Parameters<YellArgs>) -> CallToolResult {
        match crate::yell::agent::yell(&self.runtime, self.harness, &args).await {
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

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RuntimeTools {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info.name = "appa-runtime".to_string();
        info.server_info.version = RUNTIME_VERSION.to_string();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "When blocking feedback names an offer id, call execute_remedy_plan \
             with that exact id to pursue the offered remedy."
                .to_string(),
        );
        info
    }
}

/// Advertised runtime version for MCP clients.
const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");

const SESSION_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// MCP service served at `/mcp`.
pub fn service(runtime: Arc<Runtime>, harness: Adapter) -> StreamableHttpService<RuntimeTools, LocalSessionManager> {
    service_with_allowed_hosts(runtime, &[], harness)
}

pub fn service_with_allowed_hosts(
    runtime: Arc<Runtime>,
    allowed_hosts: &[String],
    harness: Adapter,
) -> StreamableHttpService<RuntimeTools, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(runtime.review_timeout() + SESSION_GRACE);
    let config = if allowed_hosts.is_empty() {
        StreamableHttpServerConfig::default()
    } else {
        StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts.iter().cloned())
    };
    StreamableHttpService::new(
        move || Ok(RuntimeTools::new(Arc::clone(&runtime), harness)),
        Arc::new(sessions),
        config,
    )
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
        let service = RuntimeTools::new(
            std::sync::Arc::new(
                Runtime::open(
                    config(),
                    tempfile::tempdir()
                        .expect("a temp dir is creatable")
                        .path()
                        .join("appa.db"),
                    None,
                )
                .expect("the deployment opens"),
            ),
            Adapter::ClaudeCode,
        );
        assert_eq!(service.get_info().server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn the_mcp_service_accepts_a_kubernetes_service_host() {
        let directory = tempfile::tempdir().expect("a temp dir is creatable");
        let service = service_with_allowed_hosts(
            std::sync::Arc::new(
                Runtime::open(config(), directory.path().join("appa.db"), None).expect("the deployment opens"),
            ),
            &["appa-runtime.appa.svc.cluster.local:18787".to_string()],
            Adapter::ClaudeCode,
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
        RuntimeTools::new(std::sync::Arc::new(runtime), Adapter::ClaudeCode)
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
