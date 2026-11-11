//! What the host records carry about a subagent's return declared ahead of its spawn: which
//! attempt declared it, with what arguments, which spawn took it, and how it was settled.

/// What a turn end is. A boundary is the end of the actor's turn as its host reported it; a
/// housekeeping end settles what an interrupted turn left open, and is not a turn boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndKind {
    #[default]
    Boundary,
    Housekeeping,
}

impl EndKind {
    pub fn is_boundary(&self) -> bool {
        matches!(self, EndKind::Boundary)
    }
}

/// The host attempt a declaration ahead of the spawn was made under: an embedding host's own
/// operation, the MCP tool call the host's client named, or the seq of the vouch an unnamed
/// daemon call spent. One attempt declares at most once.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AheadAttempt {
    Host(String),
    McpCall(String),
    Vouch(u64),
}

impl AheadAttempt {
    /// The key an [`crate::HostObservation::AheadDeclared`] record names, so a retry finds the
    /// family that declared under this attempt.
    pub fn key(&self) -> String {
        match self {
            AheadAttempt::Host(operation) => format!("ahead-attempt:host:{operation}"),
            AheadAttempt::McpCall(call) => format!("ahead-attempt:mcp:{call}"),
            AheadAttempt::Vouch(seq) => format!("ahead-attempt:vouch:{seq}"),
        }
    }
}

/// A label as the policy dialect spells a `delta`: a trust rank name and an audience list. An
/// omitted dimension keeps the declaring trajectory's current value.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelSpelling {
    pub trust: Option<String>,
    pub audience: Option<Vec<String>>,
}

/// The arguments a declaration ahead of the spawn was made with, as its caller spelled them.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredArguments {
    pub label: Option<LabelSpelling>,
    pub return_schema: Option<serde_json::Value>,
}

/// One proposal invocation of a spawn the host gave no call id: what its reservation of a
/// declaration is keyed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct InvocationNonce(pub u128);

impl From<InvocationNonce> for String {
    fn from(nonce: InvocationNonce) -> String {
        format!("{:032x}", nonce.0)
    }
}

impl TryFrom<String> for InvocationNonce {
    type Error = std::num::ParseIntError;

    fn try_from(spelled: String) -> Result<InvocationNonce, Self::Error> {
        u128::from_str_radix(&spelled, 16).map(InvocationNonce)
    }
}

/// How a declaration the spawn took was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AheadOutcome {
    /// The spawn's block applied it through its own declaration offer.
    Applied,
    /// The spawn starts several subagents, which a declaration ahead does not cover.
    FanOut,
    /// A call between the declaration and the spawn changed the declaring session's label.
    Lapsed,
    /// No declaration the spawn's block offers fits it.
    Unfit,
    /// The declaring actor's turn ended before it was applied.
    Ended,
}

/// The vouch a daemon declaration consumed: the key its control hook vouched under.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpentVouch {
    pub key: String,
}
