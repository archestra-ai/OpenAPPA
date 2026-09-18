//! The family children a proposed call's arguments name, by Claude Code's own
//! file spellings.

use appa_runtime_api::{Actor, ProposedCall, TrajectoryId};

/// The family children a call's arguments name by Claude Code's own file spellings: a
/// background subagent's output file (`tasks/<agent>.output`) and a persisted subagent
/// transcript (`subagents/agent-<agent>.jsonl`). Every string leaf of the arguments is scanned,
/// so a path inside a shell command is caught as a `Read` path is. The default spellings only:
/// a renamed copy, a symlink, or a relative path the shell resolves is not.
///
/// The wire asks this at a proposed call and nowhere else, so the scan runs once per call and
/// never over a result's payload.
pub(crate) fn names_children(actor: &Actor, call: &ProposedCall) -> Vec<TrajectoryId> {
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(call.arguments.get()) else {
        return Vec::new();
    };
    let mut agents = Vec::new();
    collect_agent_files(&arguments, &mut agents);
    agents.sort();
    agents.dedup();
    agents
        .into_iter()
        .map(|agent| TrajectoryId(format!("{}:{agent}", actor.root.0)))
        .collect()
}

pub(crate) fn collect_agent_files(value: &serde_json::Value, agents: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            agents.extend(agent_file_ids(text, "tasks/", ".output"));
            agents.extend(agent_file_ids(text, "subagents/agent-", ".jsonl"));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_agent_files(item, agents)),
        serde_json::Value::Object(fields) => fields.values().for_each(|field| collect_agent_files(field, agents)),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Every `<prefix><id><suffix>` in `text` that is a whole path token: the id is one Claude
/// Code mints (letters, digits, `-` and `_`), and neither end of the match continues into a
/// longer name. So `mytasks/a1.output` and `tasks/a1.output.bak` name no child — they are
/// neither documented file — while a path inside a shell command or a quoted argument still
/// is one, because a separator, a quote or the end of the string ends the token.
pub(crate) fn agent_file_ids(text: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    // A name character continues the file name a match would have to be all of. `.`
    // separates one extension from the next, so it belongs to the name, not to its edge.
    let is_name_char = move |c: char| is_id_char(c) || c == '.';
    text.match_indices(prefix)
        .filter_map(|(at, _)| {
            if text[..at].chars().next_back().is_some_and(is_name_char) {
                return None;
            }
            let rest = &text[at + prefix.len()..];
            let id: String = rest.chars().take_while(|c| is_id_char(*c)).collect();
            let tail = rest.strip_prefix(id.as_str())?.strip_prefix(suffix)?;
            (!id.is_empty() && !tail.starts_with(is_name_char)).then_some(id)
        })
        .collect()
}
