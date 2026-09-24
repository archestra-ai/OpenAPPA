//! The label guide: what earns a tool call each audience and trust label, and worked
//! examples. Four questions cover the two label dimensions of a tool contract: what the
//! call's result contributes (`delta.audience`, `delta.trust`) and what the trajectory must
//! satisfy before the call runs (`requires.audience`, `requires.trust`). The `jev` builtin
//! asks them as its criteria; a model builtin reads them as a section of its prompt.
//!
//! The criteria order is part of the prompt: reordering it moves answers. Every criteria
//! table is a struct, so its field order is its wire order.

use serde::Serialize;

use crate::consult::AnnotationDeclaration;

/// What the call's result may be read by, safest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResultAudience {
    #[serde(rename = "self")]
    Self_,
    Internal,
    Public,
}

/// Who wrote the call's result, safest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResultTrust {
    Suspicious,
    Trusted,
}

/// Who the call delivers data to, safest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RequiredAudience {
    Public,
    Internal,
    None,
}

impl ResultAudience {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            ResultAudience::Self_ => "self",
            ResultAudience::Internal => "internal",
            ResultAudience::Public => "public",
        }
    }
}

impl ResultTrust {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            ResultTrust::Suspicious => "suspicious",
            ResultTrust::Trusted => "trusted",
        }
    }
}

impl RequiredAudience {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            RequiredAudience::Public => "public",
            RequiredAudience::Internal => "internal",
            RequiredAudience::None => "none",
        }
    }
}

/// One answer to each of the four questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Labels {
    pub(crate) result_audience: ResultAudience,
    pub(crate) result_trust: ResultTrust,
    pub(crate) required_audience: RequiredAudience,
    pub(crate) requires_trusted: bool,
}

impl Labels {
    /// The answers whose annotation is the identity: nothing narrows, nothing is required.
    const NEUTRAL: Labels = Labels {
        result_audience: ResultAudience::Public,
        result_trust: ResultTrust::Trusted,
        required_audience: RequiredAudience::None,
        requires_trusted: false,
    };
}

/// The labels in the policy's own spelling, refused where the mandate does not admit them.
pub(crate) fn annotation(labels: &Labels, declaration: &AnnotationDeclaration) -> Result<serde_json::Value, String> {
    let (Some(lowest), Some(highest)) = (declaration.trust_ranks.first(), declaration.trust_ranks.last()) else {
        return Err("jev needs a lowest and a highest trust rank".to_string());
    };
    let admitted = |audience: &str| match declaration.audiences.entries().any(|entry| entry == audience) {
        true => Ok(serde_json::json!([audience])),
        false => Err(format!(
            "jev answered the audience {audience:?}, which the mandate does not admit"
        )),
    };
    let mut delta = serde_json::Map::new();
    match labels.result_audience {
        ResultAudience::Public => {}
        ResultAudience::Internal => {
            delta.insert("audience".to_string(), admitted("internal")?);
        }
        ResultAudience::Self_ => {
            delta.insert("audience".to_string(), admitted("self")?);
        }
    }
    match labels.result_trust {
        ResultTrust::Suspicious => {
            delta.insert("trust".to_string(), serde_json::json!(lowest));
        }
        ResultTrust::Trusted => {}
    }
    let mut requires = serde_json::Map::new();
    requires.insert("history".to_string(), serde_json::json!([]));
    requires.insert("attention".to_string(), serde_json::json!([]));
    match labels.required_audience {
        RequiredAudience::None => {}
        RequiredAudience::Public => {
            requires.insert("audience".to_string(), serde_json::json!({"contains": "public"}));
        }
        RequiredAudience::Internal => {
            requires.insert(
                "audience".to_string(),
                serde_json::json!({"contains": admitted("internal")?}),
            );
        }
    }
    if labels.requires_trusted {
        requires.insert("trust".to_string(), serde_json::json!(highest));
    }
    Ok(serde_json::json!({"delta": delta, "requires": requires, "emits": []}))
}

#[derive(Serialize)]
pub(crate) struct DeltaAudienceCriteria {
    pub(crate) public: &'static str,
    pub(crate) internal: &'static str,
    #[serde(rename = "self")]
    pub(crate) self_: &'static str,
}

pub(crate) const DELTA_AUDIENCE_CRITERIA: DeltaAudienceCriteria = DeltaAudienceCriteria {
    public: concat!(
        "The returned bytes are already readable today by someone outside the ",
        "operator's organization: a fetched public web page, published ",
        "documentation, or a file on a branch that is already pushed to a public ",
        "remote. An acknowledgement, id, URL or status with no data in it is also ",
        "`public`."
    ),
    internal: concat!(
        "The returned bytes are readable inside the operator's organization but not ",
        "outside it: a local checkout, a working tree, an unpushed branch, a build ",
        "or test run over them, a diff of uncommitted work, a private repository's ",
        "code or history, an internal chat message, a customer or business record, ",
        "an internal service's response, or a private document. This holds whatever ",
        "the upstream repository's visibility is."
    ),
    self_: concat!(
        "The returned bytes are restricted to the single identity OpenAPPA acts for ",
        "and no one else: credentials, tokens, private keys, a shell history or ",
        "environment dump, a local secret file, or personal mail — anything the ",
        "operator would not show even to a colleague."
    ),
};

#[derive(Serialize)]
pub(crate) struct DeltaTrustCriteria {
    pub(crate) trusted: &'static str,
    pub(crate) suspicious: &'static str,
}

pub(crate) const DELTA_TRUST_CRITERIA: DeltaTrustCriteria = DeltaTrustCriteria {
    trusted: concat!(
        "The returned content was authored by the operator or produced by ",
        "deterministic computation over the operator's own inputs: reading the ",
        "project's own source, running its tests, inspecting local git state, or a ",
        "first-party service under the operator's control. The operator's tools are ",
        "trusted too: what a compiler, a package manager, a build or a linter reports ",
        "is `trusted`, whatever they downloaded to do it. `trusted` describes ",
        "provenance, not factual correctness."
    ),
    suspicious: concat!(
        "The returned content could have been authored or influenced by a party ",
        "outside the operator's control, so it could carry a prompt injection: a ",
        "fetched web page or search result, a downloaded document, an inbound ",
        "email, chat message or support ticket ",
        "written by someone else, or the free-form output of another model or agent. ",
        "The agent's own harness surfaces — loading a tool schema, invoking a skill, ",
        "reading its own scratch files — are not third-party content."
    ),
};

#[derive(Serialize)]
pub(crate) struct RequiresAudienceCriteria {
    pub(crate) none: &'static str,
    pub(crate) internal: &'static str,
    pub(crate) public: &'static str,
}

pub(crate) const REQUIRES_AUDIENCE_CRITERIA: RequiresAudienceCriteria = RequiresAudienceCriteria {
    none: concat!(
        "No bytes leave this machine and no other reader receives anything: the call ",
        "reads, searches or computes locally, or writes to a local path — scratch, ",
        "temporary, or inside the project's working tree. A working-tree edit is ",
        "`none` because the bytes reach a reader only when a later call pushes or ",
        "publishes them."
    ),
    internal: concat!(
        "The receiving party is a person or service inside the operator's ",
        "organization: posting to an internal chat channel or sending a direct ",
        "message, pushing to a private repository, emailing a colleague, writing to ",
        "an internal service, database or ticket — and also a read-only query against ",
        "an internal service, because the query itself reaches it."
    ),
    public: concat!(
        "The receiving party is outside the operator's control: a public repository, ",
        "issue tracker or website, a package registry, an external email recipient, ",
        "or any third-party service — including a read-only one, because a search ",
        "query or an HTTP request reaches that destination."
    ),
};

#[derive(Serialize)]
pub(crate) struct RequiresTrustCriteria {
    #[serde(rename = "true")]
    pub(crate) true_: &'static str,
    #[serde(rename = "false")]
    pub(crate) false_: &'static str,
}

pub(crate) const REQUIRES_TRUST_CRITERIA: RequiresTrustCriteria = RequiresTrustCriteria {
    true_: concat!(
        "The call carries data the trajectory holds out of the operator's control: it ",
        "sends it to a person, even a colleague, writes it where people can read it, even ",
        "an internal repository, ticket or channel, delivers or publishes it to a service ",
        "outside the operator's organization, or executes or installs content that the ",
        "trajectory supplied. A URL or query sent to a third party counts, since it can ",
        "carry data out. Pushing commits, filing a report, installing a package and ",
        "running a downloaded script all count. An injected instruction that fires this ",
        "call moves data or code outward."
    ),
    false_: concat!(
        "The call's effect stays on the operator's machine — even if it overwrites or ",
        "deletes local files — or it only reads from an internal service the operator ",
        "controls: a read or lookup whose arguments only name what to read (an id, a ",
        "channel, a path) counts here. Re-running it ",
        "after an injection produces nothing but wasted work."
    ),
};

/// One worked example: the call as the prompt shows it, each question's answer, and why.
pub(crate) struct Example {
    pub(crate) call: &'static str,
    pub(crate) delta_audience: ResultAudience,
    pub(crate) delta_trust: ResultTrust,
    pub(crate) requires_audience: RequiredAudience,
    pub(crate) requires_trusted: bool,
    pub(crate) why: &'static str,
}

impl Example {
    pub(crate) fn labels(&self) -> Labels {
        Labels {
            result_audience: self.delta_audience,
            result_trust: self.delta_trust,
            required_audience: self.requires_audience,
            requires_trusted: self.requires_trusted,
        }
    }
}

/// Written from the contract examples in the docs, not from observed calls. Each `call` is
/// the example's `{"tool", "arguments"}` object as the prompt has always spelled it.
pub(crate) const EXAMPLES: [Example; 14] = [
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "grep -rn 'fn resolve' src/ | head -20", "description": "Find the resolver"}}"#,
        delta_audience: ResultAudience::Internal,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "reads the working tree; nothing leaves the machine",
    },
    Example {
        call: r#"{"tool": "WebFetch", "arguments": {"url": "https://docs.example.org/guide", "prompt": "summarise the retry policy"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Suspicious,
        requires_audience: RequiredAudience::Public,
        requires_trusted: true,
        why: "a public page comes back, and the URL can carry data to a third party",
    },
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "cat ~/.aws/credentials", "description": "Show the profile"}}"#,
        delta_audience: ResultAudience::Self_,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "returns secrets, but reads them locally",
    },
    Example {
        call: r#"{"tool": "send_email", "arguments": {"recipient": "colleague@corp.example", "body": "The migration finished."}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::Internal,
        requires_trusted: true,
        why: "only an acknowledgement comes back; the body is delivered to a colleague",
    },
    Example {
        call: r##"{"tool": "Write", "arguments": {"file_path": "/tmp/report.md", "content": "# Findings\n\nThe cache is cold on boot."}}"##,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "an acknowledgement comes back, and the write stays local",
    },
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "git push origin HEAD:main", "description": "Publish the branch"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::Public,
        requires_trusted: true,
        why: "the commits reach a public remote",
    },
    Example {
        call: r#"{"tool": "get_ticket_from_crm", "arguments": {"ticket_id": "T-4471"}}"#,
        delta_audience: ResultAudience::Internal,
        delta_trust: ResultTrust::Suspicious,
        requires_audience: RequiredAudience::Internal,
        requires_trusted: false,
        why: "a customer record comes back, written by someone outside the operator's control; the ticket id reaches an internal service",
    },
    Example {
        call: r#"{"tool": "slack_read_channel", "arguments": {"channel_id": "C0123", "limit": 50}}"#,
        delta_audience: ResultAudience::Internal,
        delta_trust: ResultTrust::Suspicious,
        requires_audience: RequiredAudience::Internal,
        requires_trusted: false,
        why: "an internal chat log comes back; the read only names a channel inside the workspace",
    },
    Example {
        call: r#"{"tool": "slack_send_message", "arguments": {"channel_id": "C0123", "text": "Build is green"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::Internal,
        requires_trusted: true,
        why: "the text reaches people in the workspace",
    },
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "gh pr create --title 'Fix the retry loop' --body-file notes.md", "description": "Open the pull request"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::Public,
        requires_trusted: true,
        why: "the service answers with the new pull request's URL; the title and body reach a hosted repository",
    },
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "npm install left-pad 2>&1 | tail -3", "description": "Add the dependency"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::Public,
        requires_trusted: true,
        why: "a registry receives the request and content it serves is installed; the package manager's own log comes back",
    },
    Example {
        call: r#"{"tool": "ToolSearch", "arguments": {"query": "select:Read,Edit", "max_results": 5}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "the harness returns its own tool schemas; nothing leaves the machine",
    },
    Example {
        call: r#"{"tool": "execute_remedy_plan", "arguments": {"offer_id": "5f1c02a9d3e84b77"}}"#,
        delta_audience: ResultAudience::Public,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "the harness answers with a status; the offer id reaches no outside reader",
    },
    Example {
        call: r#"{"tool": "Bash", "arguments": {"command": "cargo test -p engine 2>&1 | tail -20", "description": "Run the suite"}}"#,
        delta_audience: ResultAudience::Internal,
        delta_trust: ResultTrust::Trusted,
        requires_audience: RequiredAudience::None,
        requires_trusted: false,
        why: "a test run over the operator's own source",
    },
];

/// Each worked example with the annotation `declaration` gives it; an example whose labels
/// the mandate does not admit is left out.
fn annotated_examples(
    declaration: &AnnotationDeclaration,
) -> impl Iterator<Item = (&'static Example, serde_json::Value)> {
    EXAMPLES
        .iter()
        .filter_map(|example| Some((example, annotation(&example.labels(), declaration).ok()?)))
}

/// The guide as a model annotator reads it: each criterion under the leaf it settles, then
/// each worked example with the annotation this declaration gives it. A criterion or
/// example whose labels the mandate does not admit is left out; `None` when nothing is left.
pub(crate) fn for_model(declaration: &AnnotationDeclaration) -> Option<String> {
    let delta_audience = &DELTA_AUDIENCE_CRITERIA;
    let delta_trust = &DELTA_TRUST_CRITERIA;
    let requires_audience = &REQUIRES_AUDIENCE_CRITERIA;
    let requires_trust = &REQUIRES_TRUST_CRITERIA;
    let with_result_audience = |result_audience| Labels {
        result_audience,
        ..Labels::NEUTRAL
    };
    let with_result_trust = |result_trust| Labels {
        result_trust,
        ..Labels::NEUTRAL
    };
    let with_required_audience = |required_audience| Labels {
        required_audience,
        ..Labels::NEUTRAL
    };
    let with_requires_trusted = |requires_trusted| Labels {
        requires_trusted,
        ..Labels::NEUTRAL
    };
    let leaves = [
        (
            "delta",
            "audience",
            vec![
                (with_result_audience(ResultAudience::Public), delta_audience.public),
                (with_result_audience(ResultAudience::Internal), delta_audience.internal),
                (with_result_audience(ResultAudience::Self_), delta_audience.self_),
            ],
        ),
        (
            "delta",
            "trust",
            vec![
                (with_result_trust(ResultTrust::Trusted), delta_trust.trusted),
                (with_result_trust(ResultTrust::Suspicious), delta_trust.suspicious),
            ],
        ),
        (
            "requires",
            "audience",
            vec![
                (with_required_audience(RequiredAudience::None), requires_audience.none),
                (
                    with_required_audience(RequiredAudience::Internal),
                    requires_audience.internal,
                ),
                (
                    with_required_audience(RequiredAudience::Public),
                    requires_audience.public,
                ),
            ],
        ),
        (
            "requires",
            "trust",
            vec![
                (with_requires_trusted(true), requires_trust.true_),
                (with_requires_trusted(false), requires_trust.false_),
            ],
        ),
    ];
    let criteria = leaves.into_iter().filter_map(|(part, leaf, criteria)| {
        let bullets = criteria
            .into_iter()
            .filter_map(|(labels, criterion)| {
                let answer = annotation(&labels, declaration).ok()?;
                Some(match answer[part].get(leaf) {
                    None => format!("- omit it: {criterion}"),
                    Some(value) => format!("- `{value}`: {criterion}"),
                })
            })
            .collect::<Vec<_>>();
        (!bullets.is_empty()).then(|| format!("`{part}.{leaf}`:\n{}", bullets.join("\n")))
    });
    let examples = annotated_examples(declaration)
        .map(|(example, answer)| format!("- {}\n  -> {answer}  ({})", example.call, example.why));
    let criteria = criteria.collect::<Vec<_>>();
    let examples = examples.collect::<Vec<_>>();
    if criteria.is_empty() && examples.is_empty() {
        return None;
    }
    let mut parts = vec![
        concat!(
            "Label guide. It says what earns each trust and audience leaf of your answer. It does not ",
            "decide `emits`, `requires.history` or `requires.attention`: an example's empty lists there ",
            "are not a ruling. The deployer's `hint`, when present, overrides this guide."
        )
        .to_string(),
    ];
    parts.extend(criteria);
    if !examples.is_empty() {
        parts.push(format!(
            "Worked examples, each a call and the annotation it gets under your declaration:\n{}",
            examples.join("\n")
        ));
    }
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use appa_engine::registry::AudienceVocabulary;

    use super::*;
    use crate::consult::AnnotationAnswer;

    fn declaration(ranks: &[&str], audiences: &[&str]) -> AnnotationDeclaration {
        AnnotationDeclaration {
            hint: None,
            inputs: vec![],
            established: vec![],
            trust_ranks: ranks.iter().map(|rank| rank.to_string()).collect(),
            audiences: AudienceVocabulary::parse_entries(
                &audiences.iter().map(|entry| entry.to_string()).collect::<Vec<_>>(),
            )
            .expect("a fixture vocabulary parses"),
            attention_marks: vec![],
            effects: vec![],
        }
    }

    #[test]
    fn every_example_decodes_as_an_answer_under_the_declaration_it_was_rendered_for() {
        let declaration = declaration(&["tainted", "vetted", "signed"], &["self", "internal", "@eng"]);
        let annotated = annotated_examples(&declaration).collect::<Vec<_>>();
        assert_eq!(annotated.len(), EXAMPLES.len());
        for (example, answer) in annotated {
            let decoded = AnnotationAnswer::from_wire(&answer, &declaration)
                .unwrap_or_else(|| panic!("{} renders {answer}, which does not decode", example.call));
            let expected_delta_trust = match example.delta_trust {
                ResultTrust::Suspicious => Some("tainted".to_string()),
                ResultTrust::Trusted => None,
            };
            let expected_required_trust = example.requires_trusted.then(|| "signed".to_string());
            assert_eq!(decoded.delta_trust, expected_delta_trust, "{}", example.call);
            assert_eq!(decoded.required_trust, expected_required_trust, "{}", example.call);
            assert_eq!(
                decoded.delta_audience.is_some(),
                example.delta_audience != ResultAudience::Public,
                "{}",
                example.call
            );
            assert_eq!(
                decoded.required_audience.is_some(),
                example.requires_audience != RequiredAudience::None,
                "{}",
                example.call
            );
        }
    }

    #[test]
    fn an_example_the_mandate_does_not_admit_is_left_out() {
        let needs = |example: &Example, audience: &str| {
            example.delta_audience.name() == audience
                || (audience == "internal" && example.requires_audience == RequiredAudience::Internal)
        };
        for audiences in [&["internal"][..], &["self"], &[]] {
            let declaration = declaration(&["suspicious", "trusted"], audiences);
            let kept = annotated_examples(&declaration)
                .map(|(example, _)| example.call)
                .collect::<Vec<_>>();
            let admitted = EXAMPLES
                .iter()
                .filter(|example| {
                    ["self", "internal"]
                        .iter()
                        .all(|audience| audiences.contains(audience) || !needs(example, audience))
                })
                .map(|example| example.call)
                .collect::<Vec<_>>();
            assert_eq!(kept, admitted, "{audiences:?}");
            assert!(kept.len() < EXAMPLES.len(), "{audiences:?} leaves an example out");
        }
        assert!(for_model(&declaration(&[], &["self", "internal"])).is_none());
    }
}
