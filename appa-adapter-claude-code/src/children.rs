//! The family children a proposed call's arguments name. A call whose
//! arguments name a family child's output file or transcript is refused by
//! the runtime: the default spellings only, a heuristic rather than a
//! guarantee.

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

#[cfg(test)]
mod tests {
    use crate::fixtures::*;
    use appa_runtime_api::TrajectoryId;
    #[test]
    fn a_calls_arguments_name_the_family_children_they_spell() {
        assert!(named_children("Agent", serde_json::json!({"prompt": "list files"})).is_empty());
        assert_eq!(
            named_children(
                "Bash",
                serde_json::json!({"command": "cat tasks/a1.output; grep x subagents/agent-a2.jsonl tasks/a1.output"}),
            ),
            vec![
                TrajectoryId("cc:s1:a1".to_string()),
                TrajectoryId("cc:s1:a2".to_string())
            ],
        );
        assert_eq!(
            named_children(
                "Read",
                serde_json::json!({"file_path": "/home/u/.claude/subagents/agent-b7.jsonl", "meta": [{"p": "tasks/x-1.output"}]}),
            ),
            vec![
                TrajectoryId("cc:s1:b7".to_string()),
                TrajectoryId("cc:s1:x-1".to_string())
            ],
        );
        assert!(
            named_children("Read", serde_json::json!({"file_path": "tasks/a1.txt"})).is_empty(),
            "another suffix names no child"
        );
    }

    /// The scan reads one whole path token. A name that only contains the spelling —
    /// another directory ending in `tasks`, a copy under another extension — is a
    /// different file, and a call touching it names no child.
    #[test]
    fn a_path_that_merely_contains_the_spelling_names_no_child() {
        for path in [
            "mytasks/a1.output",
            "tasks/a1.output.bak",
            "notes/tasks/a1.outputs",
            "mysubagents/agent-a1.jsonl",
            "subagents/agent-a1.jsonl.gz",
            "backup-subagents/agent-a1.jsonl",
        ] {
            assert!(
                named_children("Read", serde_json::json!({ "file_path": path })).is_empty(),
                "{path} names neither documented child file"
            );
            assert!(
                named_children("Bash", serde_json::json!({ "command": format!("cat {path}") })).is_empty(),
                "{path} names neither documented child file inside a command"
            );
        }
    }

    /// The boundary keeps the scan working where the path is one argument of a shell
    /// command: quotes, separators and the ends of the string all end the token.
    #[test]
    fn a_child_file_is_found_wherever_a_path_token_ends() {
        for command in [
            "cat tasks/a1.output",
            "cat \"tasks/a1.output\"",
            "cat 'tasks/a1.output'",
            "cat ./tasks/a1.output",
            "cat ../run/tasks/a1.output",
            "cat /home/u/tasks/a1.output | wc -l",
            "cat $(ls tasks/a1.output)",
            "cp tasks/a1.output,tasks/a1.output.bak",
        ] {
            assert_eq!(
                named_children("Bash", serde_json::json!({ "command": command })),
                vec![TrajectoryId("cc:s1:a1".to_string())],
                "{command}"
            );
        }
    }
}
