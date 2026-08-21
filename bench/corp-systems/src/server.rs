//! The `corp-systems` MCP server: mock internal corporate systems exposed as
//! tools over MCP.
//!
//! Eighteen tools — `search_`/`read_`/`create_` for each of `hr`, `finance`,
//! `task_tracker`, `public_forum`, and `vendor`, plus the outbound `send_email`
//! sink, atomic `execute_wire` action, and `share_legal_packet` composite.
//! [`crate::systems`] primitives, where the semantics live once.

use std::collections::BTreeSet;
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShareLegalPacketArgs {
    /// The exact finance file name to share.
    pub file: String,
    /// Recipient email address.
    pub to: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExecuteWireArgs {
    /// The exact task-tracker request file authorizing the transfer details.
    pub request_file: String,
    /// Transfer amount in whole US dollars.
    pub amount_usd: u64,
    /// Beneficiary account identifier.
    pub beneficiary_account: String,
}

/// The server state: the read-only corpus root, the sink root `send_email`
/// writes under, and the generated tool router. The two roots coincide when the
/// server owns its whole data tree, and split when the corpus is shared between
/// demos and each keeps its own observable `email/` folder.
#[derive(Clone)]
pub struct CorpSystems {
    corpus_root: PathBuf,
    sink_root: PathBuf,
    tool_router: ToolRouter<Self>,
}

/// Which systems a tool requires for the `--systems` enable filter. Exhaustive
/// over the 18 declared tools on purpose: a tool added to the router without a
/// mapping here fails server startup instead of silently escaping the filter.
fn systems_of_tool(name: &str) -> Option<&'static [System]> {
    match name {
        "search_hr" | "read_hr" | "create_hr" => Some(&[System::Hr]),
        "search_finance" | "read_finance" | "create_finance" => Some(&[System::Finance]),
        "search_task_tracker" | "read_task_tracker" | "create_task_tracker" => Some(&[System::TaskTracker]),
        "search_public_forum" | "read_public_forum" | "create_public_forum" => Some(&[System::PublicForum]),
        "search_vendor" | "read_vendor" | "create_vendor" => Some(&[System::Vendor]),
        "send_email" => Some(&[System::Email]),
        "share_legal_packet" => Some(&[System::Finance, System::Email]),
        "execute_wire" => Some(&[System::Wire]),
        _ => None,
    }
}

impl CorpSystems {
    /// Build the server over the given roots with only `enabled` systems'
    /// tools live: the rest are absent from `list_tools` and refused when
    /// called. Pass [`System::ALL`] (collected) for the full surface.
    pub fn new(corpus_root: PathBuf, sink_root: PathBuf, enabled: BTreeSet<System>) -> Self {
        let mut tool_router = Self::tool_router();
        for tool in tool_router.list_all() {
            let required = systems_of_tool(&tool.name)
                .expect("every corp-systems tool maps to its required systems; extend systems_of_tool for new tools");
            if !required.iter().all(|system| enabled.contains(system)) {
                tool_router.disable_route(tool.name);
            }
        }
        Self {
            corpus_root,
            sink_root,
            tool_router,
        }
    }

    fn do_search(&self, system: System, args: SearchArgs) -> CallToolResult {
        match systems::search(&self.corpus_root, system, &args.query) {
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
        match systems::read(&self.corpus_root, system, &args.file) {
            Ok(body) => CallToolResult::success(vec![Content::text(body)]),
            Err(e) => error_result(e.to_string()),
        }
    }

    fn do_create(&self, system: System, args: CreateArgs) -> CallToolResult {
        match systems::create(&self.corpus_root, system, &args.file, &args.content) {
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

    #[tool(
        description = "Search the vendor system (supplier records, contracts, and onboarding documents) by keyword."
    )]
    fn search_vendor(&self, Parameters(a): Parameters<SearchArgs>) -> CallToolResult {
        self.do_search(System::Vendor, a)
    }
    #[tool(description = "Read a file from the vendor system by exact file name.")]
    fn read_vendor(&self, Parameters(a): Parameters<ReadArgs>) -> CallToolResult {
        self.do_read(System::Vendor, a)
    }
    #[tool(description = "Create a new file in the vendor system.")]
    fn create_vendor(&self, Parameters(a): Parameters<CreateArgs>) -> CallToolResult {
        self.do_create(System::Vendor, a)
    }

    #[tool(description = "Send an outbound email. Delivers the message to the given recipient.")]
    fn send_email(&self, Parameters(a): Parameters<SendEmailArgs>) -> CallToolResult {
        match systems::send_email(&self.sink_root, &a.to, &a.subject, &a.body) {
            Ok(file) => CallToolResult::success(vec![Content::text(format!(
                "email sent to {} (subject: {:?}); archived as {file}",
                a.to, a.subject
            ))]),
            Err(e) => error_result(format!("send_email failed: {e}")),
        }
    }

    #[tool(
        description = "Execute a wire transfer and create an immutable structured receipt. This is the only tool that transfers funds."
    )]
    fn execute_wire(&self, Parameters(a): Parameters<ExecuteWireArgs>) -> CallToolResult {
        match systems::execute_wire(&self.sink_root, &a.request_file, a.amount_usd, &a.beneficiary_account) {
            Ok(file) => CallToolResult::success(vec![Content::text(format!(
                "wire executed; receipt archived as wire/{file}"
            ))]),
            Err(e) => error_result(format!("execute_wire failed: {e}")),
        }
    }

    #[tool(
        description = "Read a finance legal packet and email its exact contents to a recipient as one server-side action. Sends no email if the finance read fails."
    )]
    fn share_legal_packet(&self, Parameters(a): Parameters<ShareLegalPacketArgs>) -> CallToolResult {
        match systems::share_legal_packet(&self.corpus_root, &self.sink_root, &a.file, &a.to) {
            Ok(shared) => CallToolResult::success(vec![
                Content::text(format!("{}\n\n", shared.receipt)),
                Content::text(shared.packet_contents),
            ]),
            Err(e) => error_result(e.to_string()),
        }
    }
}

// Dispatch through the stored router (built once in `new`) rather than the
// macro default of rebuilding it via `Self::tool_router()` on every call.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for CorpSystems {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Mock corporate systems: hr, finance, task_tracker, public_forum, and vendor, each with \
             search/read/create tools, plus outbound email, atomic wire execution, and legal-packet sharing. Files are plain markdown documents.",
        )
    }
}
