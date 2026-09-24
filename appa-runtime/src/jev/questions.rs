//! What the `jev` annotator asks about one tool call: the [label guide](crate::label_guide)'s
//! four questions, each framed with its own instructions and the guide's worked examples.
//! Effects and attention marks are not asked.

use serde::Serialize;

use crate::label_guide::{
    DELTA_AUDIENCE_CRITERIA, DELTA_TRUST_CRITERIA, DeltaAudienceCriteria, DeltaTrustCriteria, EXAMPLES, Example,
    REQUIRES_AUDIENCE_CRITERIA, REQUIRES_TRUST_CRITERIA, RequiresAudienceCriteria, RequiresTrustCriteria,
};

macro_rules! frame {
    () => {
        concat!(
            "OpenAPPA is an information-flow policy engine that sits between an agent and ",
            "its tools. Before each tool call it evaluates the call against the trajectory's ",
            "security label, a pair of `audience` (who is allowed to read the data the agent ",
            "holds) and `trust` (how much that data can be relied on). The trajectory starts ",
            "at `{public, trusted}` and can only become more restrictive.\n\n",
            "You act as an APPA Annotator. For the tool call given in `tool` and `arguments`, ",
            "supply the parts of its contract that concern audience and trust. Judge the ",
            "specific arguments of this call, not the tool's general capability: the same ",
            "tool is `public` in one call and `self` in another. A tool name that appears ",
            "inside an argument value is not a destination this call reaches.\n\n",
            "Some calls address only the agent's own harness: loading a tool schema, invoking ",
            "a skill, waiting on or reading a background task, entering a worktree, or running ",
            "an OpenAPPA remedy plan by its offer id. Such a call delivers nothing to any ",
            "reader outside this machine, and the harness itself produces its result."
        )
    };
}

const DELTA_AUDIENCE_INSTRUCTIONS: &str = concat!(
    frame!(),
    "\n\n",
    "Decide `delta.audience`, in two steps.\n\n",
    "First decide whether the call's result contains data at all. If the call ",
    "returns only an acknowledgement, an id, a URL or a status, answer `public` and ",
    "ignore everything the call sent. Argument values are never part of the result: ",
    "a file's contents, a message body or a commit message travelling outward is not ",
    "what comes back.\n\n",
    "Only if the result contains data, choose the narrowest reader set present in it. ",
    "The question is who may read the bytes the call returns to the agent, never ",
    "where the call sends data."
);

const DELTA_TRUST_INSTRUCTIONS: &str = concat!(
    frame!(),
    "\n\n",
    "Decide `delta.trust`: the trust rank of the data the call RETURNS to the agent. ",
    "The chain is `suspicious` < `trusted`. Answer `suspicious` when the returned ",
    "content could have been authored or influenced by a party outside the operator's ",
    "control, because a later tool that requires `trusted` must not run on it. Answer ",
    "about the provenance of what comes back, not about how dangerous the call is to ",
    "run.\n\n",
    "Judge by who wrote the returned text, not by whether the call used the network. ",
    "What a command or its target service reports about the call itself — a push's ",
    "progress lines, the URL or id of what it created, a list of releases or check ",
    "states, an exit status, an install or build log — is `trusted`. Text a third ",
    "party wrote — a fetched page, a downloaded document, an issue or message body, ",
    "another model's answer — is `suspicious`."
);

const REQUIRES_AUDIENCE_INSTRUCTIONS: &str = concat!(
    frame!(),
    "\n\n",
    "Decide `requires.audience.contains`: which readers must already be allowed to ",
    "read the trajectory's data before this call may run, because the call delivers ",
    "data to them.\n\n",
    "Any argument value that leaves this machine is data delivered to whoever ",
    "receives it — a query string, a message body, a file's contents, a path in a ",
    "URL. Classify by who receives those bytes. A call being a read does not by ",
    "itself make it `none`: a search against a remote service delivers the query to ",
    "that service.\n\n",
    "Judge the destination the arguments actually name. If the call reaches several, ",
    "answer with the widest one."
);

const REQUIRES_TRUST_INSTRUCTIONS: &str = concat!(
    frame!(),
    "\n\n",
    "Decide whether this call needs `requires.trust = \"trusted\"`: whether OpenAPPA ",
    "must refuse it once the trajectory has read attacker-influenceable data.\n\n",
    "This is an information-flow question, not a damage question. Do not answer yes ",
    "merely because a call is destructive or hard to undo — that is an ",
    "attention/review requirement, which is a different field."
);

/// The four questions, in the order the request asks them.
#[derive(Clone, Copy)]
enum Question {
    DeltaAudience,
    DeltaTrust,
    RequiresAudience,
    RequiresTrusted,
}

impl Question {
    fn instructions(self) -> &'static str {
        match self {
            Question::DeltaAudience => DELTA_AUDIENCE_INSTRUCTIONS,
            Question::DeltaTrust => DELTA_TRUST_INSTRUCTIONS,
            Question::RequiresAudience => REQUIRES_AUDIENCE_INSTRUCTIONS,
            Question::RequiresTrusted => REQUIRES_TRUST_INSTRUCTIONS,
        }
    }

    fn answer(self, example: &Example) -> &'static str {
        match self {
            Question::DeltaAudience => example.delta_audience.name(),
            Question::DeltaTrust => example.delta_trust.name(),
            Question::RequiresAudience => example.requires_audience.name(),
            Question::RequiresTrusted => match example.requires_trusted {
                true => "true",
                false => "false",
            },
        }
    }

    /// The instruction, the deployer's hint when the policy declares one, then the worked
    /// examples for this question.
    fn rendered(self, hint: Option<&str>) -> String {
        let mut parts = vec![self.instructions().to_string()];
        if let Some(hint) = hint.filter(|hint| !hint.is_empty()) {
            parts.push(format!("The deployer's instruction for this tool: {hint}"));
        }
        let examples = EXAMPLES
            .iter()
            .map(|example| format!("- {}\n  -> {}  ({})", example.call, self.answer(example), example.why));
        parts.push(
            std::iter::once("Worked examples:".to_string())
                .chain(examples)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        parts.join("\n\n")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum QuestionType {
    Choice,
    Noul,
}

#[derive(Serialize)]
struct Asked<C: 'static> {
    #[serde(rename = "type")]
    kind: QuestionType,
    instructions: String,
    criteria: &'static C,
}

impl<C> Asked<C> {
    fn of(question: Question, kind: QuestionType, criteria: &'static C, hint: Option<&str>) -> Asked<C> {
        Asked {
            kind,
            instructions: question.rendered(hint),
            criteria,
        }
    }
}

/// The request's `questions`, in wire order.
#[derive(Serialize)]
pub(crate) struct Questions {
    delta_audience: Asked<DeltaAudienceCriteria>,
    delta_trust: Asked<DeltaTrustCriteria>,
    requires_audience: Asked<RequiresAudienceCriteria>,
    requires_trusted: Asked<RequiresTrustCriteria>,
}

impl Questions {
    pub(crate) fn new(hint: Option<&str>) -> Questions {
        Questions {
            delta_audience: Asked::of(
                Question::DeltaAudience,
                QuestionType::Choice,
                &DELTA_AUDIENCE_CRITERIA,
                hint,
            ),
            delta_trust: Asked::of(Question::DeltaTrust, QuestionType::Choice, &DELTA_TRUST_CRITERIA, hint),
            requires_audience: Asked::of(
                Question::RequiresAudience,
                QuestionType::Choice,
                &REQUIRES_AUDIENCE_CRITERIA,
                hint,
            ),
            requires_trusted: Asked::of(
                Question::RequiresTrusted,
                QuestionType::Noul,
                &REQUIRES_TRUST_CRITERIA,
                hint,
            ),
        }
    }
}
