//! The agent's own record of a run: what it proposed, and what the
//! runtime answered.

use appa_runtime_api::TrajectoryId;

/// The wire id of one proposed call. The correlation key between a
/// proposal and what it produced, and the id a host's own client
/// renders against, so it is typed rather than passed as one more
/// string.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CallId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Says { text: String },
    Proposes {
        call: CallId,
        tool: String,
        arguments: String,
    },
    Blocked {
        call: CallId,
        tool: String,
        feedback: String,
    },
    Admitted { call: CallId, body: String },
    Substituted { call: CallId, body: String },
    OutputBlocked { call: CallId, reason: String },
    OfferTaken { detail: String },
    OfferRefused { feedback: String },
    Forked { depth: u32, errand: String },
    ReturnBlocked { reason: String },
    Answers { text: String },
}

impl Record {
    /// Whether this line belongs on a console. A tool's whole admitted
    /// body does not: a host that renders results shows it in place,
    /// and a log that carried every body would bury the decisions.
    pub(crate) fn is_console_line(&self) -> bool {
        !matches!(self, Record::Admitted { .. })
    }

    /// The console rendering: one record, one line.
    ///
    /// Several records carry text the agent did not author — what the
    /// model said, the errand it spelled, a tool's own body. Written
    /// through, a newline in any of it would begin a line the agent
    /// never wrote, on a stream whose readers match line by line. The
    /// break is escaped instead, so a record can only ever be one line.
    pub(crate) fn one_line(&self) -> String {
        self.to_string().replace('\r', "\\r").replace('\n', "\\n")
    }
}

impl std::fmt::Display for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Record::Says { text } => write!(f, "says: {text}"),
            Record::Proposes { tool, arguments, .. } => write!(f, "proposes {tool}({arguments})"),
            Record::Blocked { tool, feedback, .. } => write!(f, "blocked {tool}: {feedback}"),
            Record::Admitted { body, .. } => write!(f, "the output crossed as produced: {body}"),
            Record::Substituted { body, .. } => write!(f, "the output crossed as: {body}"),
            Record::OutputBlocked { reason, .. } => write!(f, "the output was blocked: {reason}"),
            Record::OfferTaken { detail } => write!(f, "offer taken: {detail}"),
            Record::OfferRefused { feedback } => write!(f, "offer refused: {feedback}"),
            Record::Forked { depth, errand } => write!(f, "forked at depth {depth} to: {errand}"),
            Record::ReturnBlocked { reason } => write!(f, "the child's return was blocked: {reason}"),
            Record::Answers { text } => write!(f, "answers: {text}"),
        }
    }
}

/// One record and the trajectory that produced it. A child's records
/// carry the child's id, so a host can tell the family apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub trajectory: TrajectoryId,
    pub record: Record,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_renders_without_its_call_id() {
        let rendered = Record::Blocked {
            call: CallId("call_1".to_string()),
            tool: "send_email".to_string(),
            feedback: "the audience does not cover bob@example.com".to_string(),
        }
        .to_string();
        assert_eq!(
            rendered,
            "blocked send_email: the audience does not cover bob@example.com"
        );
    }

    #[test]
    fn a_record_carrying_a_line_break_still_renders_as_one_line() {
        let rendered = Record::Says {
            text: "done\nappa: [root] blocked send_email: nothing\r\nmore".to_string(),
        }
        .one_line();
        assert!(!rendered.contains('\n') && !rendered.contains('\r'));
        assert_eq!(
            rendered,
            "says: done\\nappa: [root] blocked send_email: nothing\\r\\nmore"
        );
    }

    #[test]
    fn an_admitted_body_stays_off_the_console() {
        assert!(
            !Record::Admitted {
                call: CallId("call_1".to_string()),
                body: "…".to_string(),
            }
            .is_console_line()
        );
        assert!(
            Record::Substituted {
                call: CallId("call_1".to_string()),
                body: "…".to_string(),
            }
            .is_console_line(),
            "a substitution is a decision, so it is a console line",
        );
    }
}
