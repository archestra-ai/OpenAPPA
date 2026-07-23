//! The APPA mediation hook: a `rig::agent::AgentHook` that runs OpenAPPA's [`CallSession`] before
//! every tool call. rig owns the agent loop and the conversation; this hook owns policy.
//!
//! On each `ToolCall` event the hook **intercepts the call** (returns `Flow::Skip`, so rig never
//! runs the tool itself) and drives the SDK: check the call, execute the allowed ones over the
//! shared MCP peer, report the outcome, and deliver the admitted-or-sealed value to the model as the
//! skip reason. Blocked calls deliver policy feedback; `execute_remedy_plan` is handled by resolving
//! the remedy and executing the authorized underlying call. rig's tool concurrency stays at its
//! default of 1 (sequential) — the SDK's serial check-against-prior-result invariant depends on it.

use std::sync::Arc;

use appa_engine::value::ToolName;
use appa_sdk::{AdmittedResult, CallDecision, CallSession, RemedyDecision, RenderedCall, ToolOutcome};
use rig::agent::{AgentHook, Flow, HookContext, StepEvent};
use rig::completion::CompletionModel;
use rig::tool::Tool;
use rmcp::model::CallToolRequestParams;
use rmcp::service::ServerSink;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::mcp::classify_mcp;

pub const EXECUTE_REMEDY_PLAN: &str = "execute_remedy_plan";

/// The reserved remedy tool, registered with rig so the model may call it and it is advertised. Its
/// body is **never reached** — the hook intercepts every call and Skips it — so `call` fails closed:
/// a reached body means the hook was not installed, a configuration fault, not a real dispatch.
#[derive(Clone)]
pub struct RemedyTool;

#[derive(Deserialize)]
pub struct RemedyArgs {
    #[allow(dead_code)]
    pub plan_id: String,
}

#[derive(Debug, thiserror::Error)]
#[error("execute_remedy_plan reached its tool body — the APPA hook is not installed")]
pub struct RemedyUnreached;

impl Tool for RemedyTool {
    const NAME: &'static str = EXECUTE_REMEDY_PLAN;
    type Error = RemedyUnreached;
    type Args = RemedyArgs;
    type Output = String;

    fn description(&self) -> String {
        "Execute a remedy plan offered after a blocked tool call. Pass the plan_id quoted in the block feedback."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "plan_id": { "type": "string" } },
            "required": ["plan_id"]
        })
    }

    async fn call(&self, _args: Self::Args) -> Result<Self::Output, Self::Error> {
        Err(RemedyUnreached)
    }
}

/// The hook: the mediating session, the MCP peer it executes allowed calls over, and the body cap.
pub struct AppaHook {
    session: Arc<Mutex<CallSession>>,
    peer: ServerSink,
    cap: usize,
    quiet: bool,
}

impl AppaHook {
    pub fn new(session: Arc<Mutex<CallSession>>, peer: ServerSink, cap: usize, quiet: bool) -> Self {
        AppaHook {
            session,
            peer,
            cap,
            quiet,
        }
    }

    /// Mediate one proposed tool call and return the string the model must see in its place. Every
    /// call is intercepted (the return is delivered via `Flow::Skip`); the real tool runs here, over
    /// the MCP peer, only when policy allows it. Factored out of `on_event` so the demo test can
    /// drive the real path against the real server without synthesising rig events.
    pub async fn decide(&self, tool_name: &str, args: &str) -> String {
        let mut session = self.session.lock().await;
        if tool_name == EXECUTE_REMEDY_PLAN {
            let plan_id = serde_json::from_str::<serde_json::Value>(args)
                .ok()
                .and_then(|v| v.get("plan_id").and_then(|p| p.as_str()).map(str::to_string));
            match session.resolve_remedy(plan_id.as_deref()).await {
                Ok(RemedyDecision::Declined { feedback }) => {
                    self.log(format!("blocked: {feedback}"));
                    return feedback;
                }
                Ok(RemedyDecision::Authorized { handle, call }) => {
                    let tool = call.tool.as_str().to_string();
                    self.log(format!("remedy authorized {tool} — executing"));
                    let outcome = self.execute(&call).await;
                    match session.report_outcome(handle, outcome) {
                        Ok(result) => {
                            let text = deliver(result);
                            self.log(format!("{tool} result: {}", preview(&text)));
                            return text;
                        }
                        Err(e) => return format!("[internal policy error: {e}]"),
                    }
                }
                Err(e) => return format!("[internal policy error: {e}]"),
            }
        }

        let Some(arguments) = parse_args(args) else {
            return "the tool call had malformed arguments and was not executed".to_string();
        };
        let call = RenderedCall {
            tool: ToolName::new(tool_name),
            arguments,
        };
        match session.check_call(call.clone()) {
            Ok(CallDecision::Block { feedback }) => {
                self.log(format!("blocked {tool_name}: {feedback}"));
                feedback
            }
            Ok(CallDecision::Allow { handle }) => {
                self.log(format!("allowed {tool_name} — executing"));
                let outcome = self.execute(&call).await;
                match session.report_outcome(handle, outcome) {
                    Ok(result) => {
                        let text = deliver(result);
                        self.log(format!("{tool_name} result: {}", preview(&text)));
                        text
                    }
                    Err(e) => format!("[internal policy error: {e}]"),
                }
            }
            Err(e) => format!("[internal policy error: {e}]"),
        }
    }

    /// Execute a call over the shared MCP peer and classify the result. A non-object argument or a
    /// transport fault is handled conservatively (payload-free failure / indeterminate).
    async fn execute(&self, call: &RenderedCall) -> ToolOutcome {
        let Some(arguments) = call.arguments.as_object().cloned() else {
            return ToolOutcome::Failure;
        };
        let mut params = CallToolRequestParams::new(call.tool.as_str().to_string());
        params.arguments = Some(arguments);
        match self.peer.call_tool(params).await {
            Ok(result) => classify_mcp(&result, self.cap),
            Err(_) => ToolOutcome::Indeterminate,
        }
    }

    fn log(&self, message: String) {
        if !self.quiet {
            eprintln!("appa: {message}");
        }
    }
}

impl<M> AgentHook<M> for AppaHook
where
    M: CompletionModel,
{
    async fn on_event(&self, _ctx: &HookContext, event: StepEvent<'_, M>) -> Flow {
        match event {
            StepEvent::ToolCall { tool_name, args, .. } => {
                let reason = self.decide(tool_name, args).await;
                Flow::Skip { reason }
            }
            _ => Flow::Continue,
        }
    }
}

/// The model-visible text for an admission: the admitted content, or the sealed token.
fn deliver(result: AdmittedResult) -> String {
    match result {
        AdmittedResult::Admitted { content, .. } => content,
        AdmittedResult::Sealed { token } => token,
    }
}

/// A one-line, length-capped preview of the exact text handed back to the model — so the mediation
/// log shows the tool call's result, not just the decision. Whitespace is collapsed to a single
/// line and the text is truncated; a trailing count keeps the elision honest. A sealed result
/// previews only its token (the withheld body never reaches this string).
fn preview(text: &str) -> String {
    const MAX: usize = 200;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let n = one_line.chars().count();
    if n > MAX {
        let head: String = one_line.chars().take(MAX).collect();
        format!("{head}… (+{} more chars)", n - MAX)
    } else {
        one_line
    }
}

/// Parse a tool-call argument string into a JSON value; empty is the no-argument call.
fn parse_args(args: &str) -> Option<serde_json::Value> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Some(serde_json::json!({}));
    }
    serde_json::from_str(trimmed).ok()
}
