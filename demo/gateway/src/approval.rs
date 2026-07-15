//! The approval record: the string the approver returns and the proxy harvests.

use std::collections::BTreeSet;
use std::fmt;

use baton_core::{ToolName, UserId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Granted,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub verdict: Verdict,
    pub tool: ToolName,
    pub recipients: BTreeSet<UserId>,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    tool: String,
    recipients: Vec<String>,
}

impl ApprovalRecord {
    pub fn new(verdict: Verdict, tool: ToolName, recipients: BTreeSet<UserId>) -> Self {
        Self {
            verdict,
            tool,
            recipients,
        }
    }

    /// Parse the machine-readable first line of an approval tool result:
    /// `GRANTED <json>` / `DENIED <json>`, where `<json>` is
    /// `{"tool":…,"recipients":[…]}`. Following lines are human prose and
    /// ignored. Returns `None` for anything that does not match — an unparseable
    /// result is treated as no approval at all (fail closed).
    pub fn parse(content: &str) -> Option<Self> {
        let line = content.lines().next()?.trim();
        let (word, json) = line.split_once(' ')?;
        let verdict = match word {
            "GRANTED" => Verdict::Granted,
            "DENIED" => Verdict::Denied,
            _ => return None,
        };
        let payload: Payload = serde_json::from_str(json).ok()?;
        Some(Self {
            verdict,
            tool: ToolName::new(payload.tool),
            recipients: payload.recipients.into_iter().map(UserId::new).collect(),
        })
    }
}

impl fmt::Display for ApprovalRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (word, prose) = match self.verdict {
            Verdict::Granted => ("GRANTED", "Approved. Retry the original tool call now, unchanged."),
            Verdict::Denied => (
                "DENIED",
                "Denied. Do not retry this call; choose another approach or stop.",
            ),
        };
        let payload = Payload {
            tool: self.tool.as_str().to_string(),
            recipients: self.recipients.iter().map(|r| r.as_str().to_string()).collect(),
        };
        let json = serde_json::to_string(&payload).map_err(|_| fmt::Error)?;
        writeln!(f, "{word} {json}")?;
        write!(f, "{prose}")
    }
}
