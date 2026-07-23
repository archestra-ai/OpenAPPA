//! The `corp-systems` MCP server: mock internal corporate systems exposed as
//! tools over MCP.
//!
//! Thirteen tools — `search_`/`read_`/`create_` for each of `hr`, `finance`,
//! `task_tracker`, `public_forum`, plus the outbound `send_email` sink. Each is a
//! three-line `#[tool]` method delegating to the [`crate::systems`] primitives;
//! the semantics live there, once. `send_email` has no `read`/`search` mate —
//! the `email/` folder is a write-only side-effect the injection demo watches.

use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::systems::{self, System};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// Keyword or phrase to match against file names and contents.
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadArgs {
    /// The exact file name to read, e.g. `alice-chen.md`.
    pub file: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateArgs {
    /// The file name to create, e.g. `TASK-103.md`.
    pub file: String,
    /// The full contents to write.
    pub content: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendEmailArgs {
    /// Recipient email address.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Message body.
    pub body: String,
}

/// The server state: the data root plus the generated tool router.
#[derive(Clone)]
pub struct CorpSystems {
    root: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl CorpSystems {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            tool_router: Self::tool_router(),
        }
    }

    fn do_search(&self, system: System, args: SearchArgs) -> CallToolResult {
        match systems::search(&self.root, system, &args.query) {
            Ok(hits) if hits.is_empty() => CallToolResult::success(vec![Content::text(format!(
                "no matches for {:?} in the {system} system",
                args.query
            ))]),
            Ok(hits) => {
                let mut out = format!("{} match(es) in the {system} system:\n", hits.len());
                for hit in hits {
                    out.push_str(&format!("- {} — {}\n", hit.file, hit.snippet));
                }
                CallToolResult::success(vec![Content::text(out)])
            }
            Err(e) => error_result(format!("search failed: {e}")),
        }
    }

    fn do_read(&self, system: System, args: ReadArgs) -> CallToolResult {
        match systems::read(&self.root, system, &args.file) {
            Ok(body) => CallToolResult::success(vec![Content::text(body)]),
            Err(e) => error_result(e.to_string()),
        }
    }

    fn do_create(&self, system: System, args: CreateArgs) -> CallToolResult {
        match systems::create(&self.root, system, &args.file, &args.content) {
            Ok(()) => CallToolResult::success(vec![Content::text(format!(
                "created {} in the {system} system",
                args.file
            ))]),
            Err(e) => error_result(e.to_string()),
        }
    }
}

/// A model-visible error result: delivered as text (so the model can read and
/// recover from it) but flagged `is_error` so the transport reports it faithfully.
fn error_result(message: String) -> CallToolResult {
    CallToolResult::error(vec![Content::text(message)])
}

#[tool_router]
impl CorpSystems {
    #[tool(description = "Search the HR system (employee records, org roster, policies) by keyword.")]
    fn search_hr(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        self.do_search(System::Hr, a)
    }
    #[tool(description = "Read a file from the HR system by exact file name.")]
    fn read_hr(&self, Parameters(a): Parameters<ReadArgs>) -> CallToolResult {
        self.do_read(System::Hr, a)
    }
    #[tool(description = "Create a new file in the HR system.")]
    fn create_hr(&self, Parameters(a): Parameters<CreateArgs>) -> CallToolResult {
        self.do_create(System::Hr, a)
    }

    #[tool(description = "Search the finance system (invoices, budgets, expense policy) by keyword.")]
    fn search_finance(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        self.do_search(System::Finance, a)
    }
    #[tool(description = "Read a file from the finance system by exact file name.")]
    fn read_finance(&self, Parameters(a): Parameters<ReadArgs>) -> CallToolResult {
        self.do_read(System::Finance, a)
    }
    #[tool(description = "Create a new file in the finance system.")]
    fn create_finance(&self, Parameters(a): Parameters<CreateArgs>) -> CallToolResult {
        self.do_create(System::Finance, a)
    }

    #[tool(description = "Search the task tracker (tickets, tasks, assignments) by keyword.")]
    fn search_task_tracker(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        self.do_search(System::TaskTracker, a)
    }
    #[tool(description = "Read a task from the task tracker by exact file name.")]
    fn read_task_tracker(&self, Parameters(a): Parameters<ReadArgs>) -> CallToolResult {
        self.do_read(System::TaskTracker, a)
    }
    #[tool(description = "Create a new task in the task tracker.")]
    fn create_task_tracker(&self, Parameters(a): Parameters<CreateArgs>) -> CallToolResult {
        self.do_create(System::TaskTracker, a)
    }

    #[tool(description = "Search the public forum (external, untrusted user-posted content) by keyword.")]
    fn search_public_forum(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        self.do_search(System::PublicForum, a)
    }
    #[tool(description = "Read a public forum post by exact file name.")]
    fn read_public_forum(&self, Parameters(a): Parameters<ReadArgs>) -> CallToolResult {
        self.do_read(System::PublicForum, a)
    }
    #[tool(description = "Publish a new post to the public forum.")]
    fn create_public_forum(&self, Parameters(a): Parameters<CreateArgs>) -> CallToolResult {
        self.do_create(System::PublicForum, a)
    }

    #[tool(description = "Send an outbound email. Delivers the message to the given recipient.")]
    fn send_email(&self, Parameters(a): Parameters<SendEmailArgs>) -> CallToolResult {
        match systems::send_email(&self.root, &a.to, &a.subject, &a.body) {
            Ok(file) => CallToolResult::success(vec![Content::text(format!(
                "email sent to {} (subject: {:?}); archived as {file}",
                a.to, a.subject
            ))]),
            Err(e) => error_result(format!("send_email failed: {e}")),
        }
    }
}

// Dispatch through the stored router (built once in `new`) rather than the
// macro default of rebuilding it via `Self::tool_router()` on every call.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for CorpSystems {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Mock internal corporate systems: hr, finance, task_tracker and a public_forum, each with \
             search/read/create tools, plus send_email for outbound mail. Files are plain markdown documents.",
        )
    }
}
