//! The `execute_remedy_plan` MCP endpoint — the engine's reserved tool,
//! runtime-provided and identical for every harness.

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};

use crate::api::{OfferId, RemedyDecision, Runtime};

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
    pub async fn execute_remedy_plan(&self, Parameters(args): Parameters<ExecuteRemedyPlanArgs>) -> CallToolResult {
        let offer = OfferId(args.offer_id);
        let trajectory = match self.runtime.offer_trajectory(&offer) {
            Ok(Some(trajectory)) => trajectory,
            Ok(None) => return refusal("no live offer with this id exists"),
            Err(error) => return refusal(&error.to_string()),
        };
        let mut session = match self.runtime.session(&trajectory) {
            Ok(session) => session,
            Err(error) => return refusal(&error.to_string()),
        };
        match session.on_remedy(offer).await {
            Ok(RemedyDecision::Authorized { call }) => CallToolResult::success(vec![ContentBlock::text(format!(
                "Authorized. Propose the {} call again, byte-for-byte identical; \
                     it will run without a new check.",
                call.tool,
            ))]),
            Ok(RemedyDecision::Returned { value }) => CallToolResult::success(vec![ContentBlock::text(value)]),
            Ok(RemedyDecision::Staged { feedback })
            | Ok(RemedyDecision::Declined { feedback })
            | Ok(RemedyDecision::NoAnswer { feedback }) => CallToolResult::success(vec![ContentBlock::text(feedback)]),
            Err(error) => refusal(&error.to_string()),
        }
    }
}

fn refusal(detail: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(detail.to_string())])
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

/// The tower service `main` nests at `/mcp`, serving from process
/// start.
pub fn service(runtime: Arc<Runtime>) -> StreamableHttpService<RemedyService, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(RemedyService::new(Arc::clone(&runtime))),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ProposedCall, ToolCallDecision, testing};
    use crate::config::Config;
    use rmcp::handler::server::wrapper::Parameters;

    fn config() -> Config {
        let text = r#"
            [policy]
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
        let runtime = Arc::new(testing::runtime(config(), dir.path().join("appa.db")));
        let service = RemedyService::new(runtime);
        let result = service
            .execute_remedy_plan(Parameters(ExecuteRemedyPlanArgs {
                offer_id: "never-surfaced".to_string(),
            }))
            .await;
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn a_surfaced_offer_executes_and_returns_the_value() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Arc::new(testing::runtime(config(), dir.path().join("appa.db")));
        let mut session = runtime
            .create_session(crate::api::TrajectoryId("cc:mcp-test".to_string()))
            .expect("a fresh id opens");
        testing::enqueue_deny(&runtime, "blocked; execute_remedy_plan(offer-mcp)", &["offer-mcp"]);
        let denied = session
            .on_tool_call(ProposedCall {
                tool: "Bash".to_string(),
                arguments: serde_json::json!({"command": "ls"}),
            })
            .await
            .expect("the deny is delivered");
        assert!(matches!(denied, ToolCallDecision::Deny { .. }));

        testing::enqueue_value(&runtime, "the cleaned result");
        let service = RemedyService::new(Arc::clone(&runtime));
        let result = service
            .execute_remedy_plan(Parameters(ExecuteRemedyPlanArgs {
                offer_id: "offer-mcp".to_string(),
            }))
            .await;
        assert_ne!(result.is_error, Some(true));
        let text = format!("{:?}", result.content);
        assert!(
            text.contains("the cleaned result"),
            "the value must be the tool response"
        );
    }
}
