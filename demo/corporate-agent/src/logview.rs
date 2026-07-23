//! A pretty terminal log of an agent run, implemented as a [`rig`](rig) hook.
//!
//! [`PrettyLog`] observes the run via [`AgentHook::on_event`] and prints, in
//! order: the system preamble and prompt sent to the model each turn, every
//! tool call with its arguments, every tool result (coloured by outcome), and
//! the assistant's text plus token usage per turn. It never steers — every
//! branch returns [`Flow::Continue`].
//!
//! Colour is raw ANSI, gated on `enabled` (a TTY check plus `NO_COLOR` / the
//! `--quiet` flag), so no colour crate is needed.

use std::io::IsTerminal;

use rig::agent::{AgentHook, Flow, HookContext, StepEvent};
use rig::completion::CompletionModel;
use rig::message::{AssistantContent, Message, UserContent};

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// The maximum number of lines shown for a single tool result before it is
/// truncated (with a note). Keeps a large file read from flooding the log.
const MAX_RESULT_LINES: usize = 20;

/// Observes an agent run and prints a readable, coloured trace of it.
pub struct PrettyLog {
    enabled: bool,
    preamble: String,
}

impl PrettyLog {
    /// `preamble` is the system prompt the binary configured on the agent
    /// (hooks are not handed it by the runtime, so we echo the copy we hold).
    /// `colour` requests colour; it still yields to a non-TTY stdout and `NO_COLOR`.
    pub fn new(preamble: impl Into<String>, colour: bool) -> Self {
        let enabled = colour && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self {
            enabled,
            preamble: preamble.into(),
        }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.enabled {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }
}

impl<M> AgentHook<M> for PrettyLog
where
    M: CompletionModel,
{
    async fn on_event(&self, _ctx: &HookContext, event: StepEvent<'_, M>) -> Flow {
        match event {
            StepEvent::CompletionCall { prompt, turn, .. } => {
                println!("\n{}", self.paint(BOLD, &format!("──── turn {turn} → model ────")));
                if turn == 1 && !self.preamble.trim().is_empty() {
                    println!("{}", self.paint(DIM, "system:"));
                    println!("{}", self.paint(DIM, &indent(&self.preamble)));
                }
                if let Some(text) = message_text(prompt) {
                    let label = self.paint(BLUE, "prompt:");
                    println!("{label}\n{}", indent(&text));
                }
            }
            StepEvent::ToolCall { tool_name, args, .. } => {
                let head = self.paint(YELLOW, &format!("⚙ tool call: {tool_name}"));
                println!("{head}");
                println!("{}", indent(&pretty_json(args)));
            }
            StepEvent::ToolResult {
                tool_name,
                result,
                outcome,
                ..
            } => {
                let ok = matches!(outcome, rig::tool::ToolOutcome::Success);
                let colour = if ok { GREEN } else { RED };
                let head = self.paint(colour, &format!("← {tool_name} [{}]", outcome.as_str()));
                println!("{head}");
                println!("{}", indent(&truncate_lines(result)));
            }
            StepEvent::ModelTurnFinished { content, usage, .. } => {
                let text = assistant_text(content);
                if !text.trim().is_empty() {
                    println!("{}", self.paint(CYAN, "assistant:"));
                    println!("{}", indent(&text));
                }
                let usage_line = format!("tokens: {} in / {} out", usage.input_tokens, usage.output_tokens);
                println!("{}", self.paint(DIM, &usage_line));
            }
            _ => {}
        }
        Flow::Continue
    }
}

/// Extract the plain text of a user/system message, if any.
fn message_text(message: &Message) -> Option<String> {
    match message {
        Message::System { content } => Some(content.clone()),
        Message::User { content } => {
            let text: String = content
                .iter()
                .filter_map(|c| match c {
                    UserContent::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Message::Assistant { .. } => None,
    }
}

/// Concatenate the assistant's text parts for a turn (tool calls are logged
/// separately via `ToolCall`).
fn assistant_text(content: &rig::OneOrMany<AssistantContent>) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pretty-print a JSON argument string; fall back to the raw string.
fn pretty_json(raw: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

fn truncate_lines(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= MAX_RESULT_LINES {
        return s.trim_end().to_string();
    }
    let shown = lines[..MAX_RESULT_LINES].join("\n");
    format!("{shown}\n… ({} more lines)", lines.len() - MAX_RESULT_LINES)
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
}
