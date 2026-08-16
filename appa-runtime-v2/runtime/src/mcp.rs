//! The `execute_remedy_plan` MCP endpoint — the engine's reserved tool,
//! runtime-provided and identical for every harness.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::api::{OfferId, RemedyOutcome, Runtime};
use crate::elicit::Elicitation;

#[derive(Clone)]
pub struct RemedyService {
    runtime: Arc<Runtime>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteRemedyPlanArgs {
    pub offer_id: String,
}

#[tool_router]
impl RemedyService {
    pub fn new(runtime: Arc<Runtime>) -> RemedyService {
        RemedyService { runtime }
    }

    #[tool(description = "Execute one remedy plan by the offer id that blocking \
                       feedback surfaced. The id must be quoted exactly.")]
    pub async fn execute_remedy_plan(
        &self,
        Parameters(args): Parameters<ExecuteRemedyPlanArgs>,
        request: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let elicitation = Elicitation::new(request, self.runtime.review_timeout());
        render(self.runtime.remedy(OfferId(args.offer_id), Some(&elicitation)).await)
    }
}

fn render(outcome: RemedyOutcome) -> CallToolResult {
    match outcome {
        RemedyOutcome::Authorized { tool } => CallToolResult::success(vec![ContentBlock::text(format!(
            "[appa] Authorized. Retry the original {tool} call with exactly the same arguments.",
        ))]),
        RemedyOutcome::Returned { value } => CallToolResult::success(vec![ContentBlock::text(value)]),
        RemedyOutcome::Declined { feedback } | RemedyOutcome::NoAnswer { feedback } => {
            CallToolResult::success(vec![ContentBlock::text(feedback)])
        }
        RemedyOutcome::Refused { detail } => CallToolResult::error(vec![ContentBlock::text(detail)]),
    }
}

#[tool_handler]
impl ServerHandler for RemedyService {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.server_info.name = "appa-runtime-v2".to_string();
        info.server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "When blocking feedback names an offer id, call execute_remedy_plan \
             with that exact id to pursue the offered remedy."
                .to_string(),
        );
        info
    }
}

const SESSION_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// The tower service `main` nests at `/mcp`, serving from process
/// start.
pub fn service(runtime: Arc<Runtime>) -> StreamableHttpService<RemedyService, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(runtime.review_timeout() + SESSION_GRACE);
    StreamableHttpService::new(
        move || Ok(RemedyService::new(Arc::clone(&runtime))),
        Arc::new(sessions),
        Default::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::raw;
    use crate::api::{ProposedCall, ToolCallDecision, testing};
    use crate::config::Config;

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 1
            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    #[tokio::test]
    async fn an_unknown_offer_gets_one_typed_refusal() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = testing::runtime(config(), dir.path().join("appa.db"));
        let outcome = runtime.execute_remedy(OfferId("never-surfaced".to_string())).await;
        assert_eq!(
            outcome,
            RemedyOutcome::Refused {
                detail: "no live offer with this id exists".to_string(),
            },
        );
        assert_eq!(render(outcome).is_error, Some(true));
    }

    #[tokio::test]
    async fn one_offer_executes_once_at_a_time() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = testing::runtime(config(), dir.path().join("appa.db"));
        let offer = OfferId("offer-live".to_string());

        let claim = runtime.claim_offer(&offer).expect("a free offer is claimable");
        assert!(
            runtime.claim_offer(&offer).is_none(),
            "a second execution of one offer is refused"
        );
        assert_eq!(
            runtime.execute_remedy(offer.clone()).await,
            RemedyOutcome::Refused {
                detail: "this offer is already being executed".to_string(),
            },
        );

        drop(claim);
        assert!(
            runtime.claim_offer(&offer).is_some(),
            "the offer is claimable again once its execution ended"
        );
    }

    #[tokio::test]
    async fn a_surfaced_offer_executes_and_returns_the_value() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = testing::runtime(config(), dir.path().join("appa.db"));
        let mut session = runtime
            .create_session(crate::api::TrajectoryId("cc:mcp-test".to_string()))
            .expect("a fresh id opens");
        testing::enqueue_deny(&runtime, "blocked; execute_remedy_plan(offer-mcp)", &["offer-mcp"]);
        let denied = session
            .on_tool_call(
                ProposedCall {
                    tool: "Bash".to_string(),
                    arguments: raw(serde_json::json!({"command": "ls"})),
                },
                false,
            )
            .await
            .expect("the deny is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }));

        testing::enqueue_value(&runtime, "the cleaned result");
        assert_eq!(
            runtime.execute_remedy(OfferId("offer-mcp".to_string())).await,
            RemedyOutcome::Returned {
                value: "the cleaned result".to_string(),
            },
        );
    }
}
