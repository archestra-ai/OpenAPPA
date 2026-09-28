//! What the `jev` annotator asks about one tool call: the [label guide](crate::label_guide)'s
//! four questions, each framed with the guide's rule for it and its worked examples.
//! Effects and attention marks are not asked.

use serde::Serialize;

use crate::label_guide::{
    CALL_RULES, DELTA_AUDIENCE_CRITERIA, DELTA_AUDIENCE_RULE, DELTA_TRUST_CRITERIA, DELTA_TRUST_RULE,
    DeltaAudienceCriteria, DeltaTrustCriteria, EXAMPLES, Leaf, REQUIRES_AUDIENCE_CRITERIA, REQUIRES_AUDIENCE_RULE,
    REQUIRES_TRUST_CRITERIA, REQUIRES_TRUST_RULE, RequiresAudienceCriteria, RequiresTrustCriteria,
};

const CONTEXT: &str = concat!(
    "OpenAPPA is an information-flow policy engine that sits between an agent and its tools. Before each ",
    "tool call it evaluates the call against the trajectory's security label, a pair of `audience` (who is ",
    "allowed to read the data the agent holds) and `trust` (how much that data can be relied on). The ",
    "trajectory starts at `{public, trusted}` and can only become more restrictive.\n\n",
    "You act as an APPA Annotator. For the tool call given in `tool` and `arguments`, supply the parts of ",
    "its contract that concern audience and trust."
);

/// The question as jev frames it, then the guide's rule that decides it.
fn instructions(leaf: Leaf) -> [&'static str; 2] {
    match leaf {
        Leaf::DeltaAudience => ["Decide `delta.audience`.", DELTA_AUDIENCE_RULE],
        Leaf::DeltaTrust => [
            "Decide `delta.trust`. The chain is `suspicious` < `trusted`.",
            DELTA_TRUST_RULE,
        ],
        Leaf::RequiresAudience => ["Decide `requires.audience.contains`.", REQUIRES_AUDIENCE_RULE],
        Leaf::RequiresTrust => [
            "Decide whether this call needs `requires.trust = \"trusted\"`.",
            REQUIRES_TRUST_RULE,
        ],
    }
}

/// The context, the call rules and the question, the deployer's hint when the policy
/// declares one, then the worked examples for this question.
fn rendered(leaf: Leaf, hint: Option<&str>) -> String {
    let [question, rule] = instructions(leaf);
    let mut parts = [CONTEXT, CALL_RULES, question, rule].map(str::to_string).to_vec();
    if let Some(hint) = hint.filter(|hint| !hint.is_empty()) {
        parts.push(format!("The deployer's instruction for this tool: {hint}"));
    }
    let examples = EXAMPLES.iter().map(|example| {
        let answer = leaf.name(&example.labels());
        format!("- {}\n  -> {answer}  ({})", example.call, example.why)
    });
    parts.push(
        std::iter::once("Worked examples:".to_string())
            .chain(examples)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    parts.join("\n\n")
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
    fn of(leaf: Leaf, kind: QuestionType, criteria: &'static C, hint: Option<&str>) -> Asked<C> {
        Asked {
            kind,
            instructions: rendered(leaf, hint),
            criteria,
        }
    }
}

/// The request's `questions`: the field order is the wire order.
#[derive(Serialize)]
pub(crate) struct Questions {
    delta_audience: Asked<DeltaAudienceCriteria>,
    delta_trust: Asked<DeltaTrustCriteria>,
    requires_audience: Asked<RequiresAudienceCriteria>,
    requires_trusted: Asked<RequiresTrustCriteria>,
}

impl Questions {
    pub(crate) fn new(hint: Option<&str>) -> Questions {
        use QuestionType::{Choice, Noul};
        Questions {
            delta_audience: Asked::of(Leaf::DeltaAudience, Choice, &DELTA_AUDIENCE_CRITERIA, hint),
            delta_trust: Asked::of(Leaf::DeltaTrust, Choice, &DELTA_TRUST_CRITERIA, hint),
            requires_audience: Asked::of(Leaf::RequiresAudience, Choice, &REQUIRES_AUDIENCE_CRITERIA, hint),
            requires_trusted: Asked::of(Leaf::RequiresTrust, Noul, &REQUIRES_TRUST_CRITERIA, hint),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
    use serde_json::value::RawValue;

    use super::Questions;

    /// A JSON object's entries in the order the serializer wrote them.
    struct Entries(Vec<(String, Box<RawValue>)>);

    impl<'de> Deserialize<'de> for Entries {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Entries, D::Error> {
            struct InOrder;
            impl<'de> Visitor<'de> for InOrder {
                type Value = Entries;
                fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    formatter.write_str("an object")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                    let mut entries = vec![];
                    while let Some(entry) = map.next_entry()? {
                        entries.push(entry);
                    }
                    Ok(Entries(entries))
                }
            }
            deserializer.deserialize_map(InOrder)
        }
    }

    fn entries(json: &str) -> Vec<(String, Box<RawValue>)> {
        serde_json::from_str::<Entries>(json).expect("an object").0
    }

    fn keys(entries: &[(String, Box<RawValue>)]) -> Vec<&str> {
        entries.iter().map(|(key, _)| key.as_str()).collect()
    }

    fn string(raw: &RawValue) -> String {
        serde_json::from_str(raw.get()).expect("a string")
    }

    #[test]
    fn the_request_asks_four_typed_questions_with_criteria_in_declared_order() {
        let expected = [
            ("delta_audience", "choice", &["public", "internal", "self"][..]),
            ("delta_trust", "choice", &["trusted", "suspicious"]),
            ("requires_audience", "choice", &["none", "internal", "public"]),
            ("requires_trusted", "noul", &["true", "false"]),
        ];
        for hint in [None, Some("the deployment's own host is build.corp")] {
            let questions = entries(&serde_json::to_string(&Questions::new(hint)).expect("serializes"));
            assert_eq!(keys(&questions), expected.map(|(key, _, _)| key));
            for ((_, question), (key, kind, criteria)) in questions.iter().zip(expected) {
                let fields = entries(question.get());
                assert_eq!(keys(&fields), ["type", "instructions", "criteria"], "{key}");
                assert_eq!(string(&fields[0].1), kind, "{key}");
                assert!(!string(&fields[1].1).is_empty(), "{key}");
                let criteria_entries = entries(fields[2].1.get());
                assert_eq!(keys(&criteria_entries), criteria, "{key}");
                assert!(
                    criteria_entries.iter().all(|(_, text)| !string(text).is_empty()),
                    "{key}"
                );
            }
        }
    }
}
