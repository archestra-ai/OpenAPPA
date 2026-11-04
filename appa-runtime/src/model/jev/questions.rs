//! What the `jev` annotator asks about one tool call: the [label guide](crate::label_guide)'s
//! four questions, each framed with the guide's rule for it and its worked examples.
//! Effects and attention marks are not asked.

use serde::Serialize;

use crate::consult::AnnotationDeclaration;
use crate::label_guide::{
    CALL_RULES, DELTA_AUDIENCE_CRITERIA, DELTA_AUDIENCE_RULE, DELTA_TRUST_CRITERIA, DELTA_TRUST_RULE,
    DeltaAudienceCriteria, DeltaTrustCriteria, EXAMPLES, FRAMING, Leaf, REQUIRES_AUDIENCE_CRITERIA,
    REQUIRES_AUDIENCE_RULE, REQUIRES_TRUST_CRITERIA, REQUIRES_TRUST_RULE, RequiresAudienceCriteria,
    RequiresTrustCriteria,
};

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

/// The deployer's text the declaration carries, one paragraph each: the tool hint, the
/// deployment hint, then each context provider's hint.
fn deployer_paragraphs(declaration: &AnnotationDeclaration) -> Vec<String> {
    let tool = declaration
        .hint
        .as_deref()
        .filter(|hint| !hint.is_empty())
        .map(|hint| format!("The deployer's instruction for this tool: {hint}"));
    let deployment = declaration
        .deployment_hint
        .as_ref()
        .map(|hint| format!("The deployer's statement about this deployment: {}", hint.as_str()));
    let providers = declaration.context_hints.iter().map(|(provider, hint)| {
        format!(
            "The deployer's instruction on reading the `{}` entry in `context`: {}",
            provider.as_str(),
            hint.as_str()
        )
    });
    tool.into_iter().chain(deployment).chain(providers).collect()
}

/// The framing, the call rules and the question, the deployer's text the declaration
/// carries, then the worked examples for this question.
fn rendered(leaf: Leaf, deployer: &[String]) -> String {
    let [question, rule] = instructions(leaf);
    let mut parts = [FRAMING, CALL_RULES, question, rule].map(str::to_string).to_vec();
    parts.extend_from_slice(deployer);
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
    fn of(leaf: Leaf, kind: QuestionType, criteria: &'static C, deployer: &[String]) -> Asked<C> {
        Asked {
            kind,
            instructions: rendered(leaf, deployer),
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
    pub(crate) fn new(declaration: &AnnotationDeclaration) -> Questions {
        use QuestionType::{Choice, Noul};
        let deployer = deployer_paragraphs(declaration);
        Questions {
            delta_audience: Asked::of(Leaf::DeltaAudience, Choice, &DELTA_AUDIENCE_CRITERIA, &deployer),
            delta_trust: Asked::of(Leaf::DeltaTrust, Choice, &DELTA_TRUST_CRITERIA, &deployer),
            requires_audience: Asked::of(Leaf::RequiresAudience, Choice, &REQUIRES_AUDIENCE_CRITERIA, &deployer),
            requires_trusted: Asked::of(Leaf::RequiresTrust, Noul, &REQUIRES_TRUST_CRITERIA, &deployer),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use appa_engine::authority::ContextHint;
    use appa_engine::names::ContextProviderName;
    use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
    use serde_json::value::RawValue;

    use super::Questions;
    use crate::consult::AnnotationDeclaration;

    fn declaration(hint: Option<&str>) -> AnnotationDeclaration {
        AnnotationDeclaration {
            hint: hint.map(str::to_string),
            deployment_hint: None,
            context_hints: BTreeMap::new(),
            inputs: vec![],
            trust_ranks: vec![],
            audiences: Default::default(),
            attention_marks: vec![],
            effects: vec![],
        }
    }

    fn instructions(declaration: &AnnotationDeclaration) -> Vec<String> {
        entries(&serde_json::to_string(&Questions::new(declaration)).expect("serializes"))
            .iter()
            .map(|(_, question)| string(&entries(question.get())[1].1))
            .collect()
    }

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
            let questions = entries(&serde_json::to_string(&Questions::new(&declaration(hint))).expect("serializes"));
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

    /// Every question carries each deployer hint the declaration holds, in a paragraph of its
    /// own; a declaration without the two layers asks exactly what it asked before them.
    #[test]
    fn every_question_carries_the_deployment_and_provider_hints() {
        let deployment = "Hosts under corp.example are the organization's own.";
        let github = "A private repository of the acme org is internal.";
        let hinted = AnnotationDeclaration {
            deployment_hint: Some(ContextHint::new(deployment).expect("a short hint")),
            context_hints: BTreeMap::from([(
                ContextProviderName::new("github"),
                ContextHint::new(github).expect("a short hint"),
            )]),
            ..declaration(Some("the tool hint"))
        };
        let plain = declaration(Some("the tool hint"));
        for (with, without) in instructions(&hinted).iter().zip(instructions(&plain)) {
            let paragraphs: Vec<&str> = with.split("\n\n").collect();
            assert!(paragraphs.iter().any(|paragraph| paragraph.ends_with(deployment)));
            assert!(paragraphs.iter().any(|paragraph| paragraph.ends_with(github)));
            assert!(!without.contains(deployment) && !without.contains(github));
            let stripped: Vec<&str> = paragraphs
                .into_iter()
                .filter(|paragraph| !paragraph.ends_with(deployment) && !paragraph.ends_with(github))
                .collect();
            assert_eq!(stripped.join("\n\n"), without);
        }
    }
}
