//! A hosted document's contracts read without serving them: the direction each edit moves
//! the calls it gates, and the verdict the contracts alone give one call. Then the structured
//! fields a served decision carries beside its feedback.

use std::sync::Arc;
use std::time::Duration;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::Runtime;
use appa_runtime::config::{Config, HostDefaults};
use appa_runtime::contracts::{ChangeKind, ContractCall, ContractVerdict, Direction, HostedContracts};
use appa_runtime::hooks;
use appa_runtime_api::{
    Actor, DecisionGap, DecisionLabel, DecisionNarrowing, HookDecision, HookEvent, ProposedCall, TrajectoryId,
};

const BASE: &str = r#"
[policy]
version = 2
trust_chain = ["untrusted", "vendor", "trusted"]

# An explicit mandate: one left to default widens with every mark and effect the policy names.
[[policy.annotator]]
name = "classifier"
audiences = []
marks = []
effects = []

[[policy.tool]]
name = "mcp/shell/run"
requires = { trust = "vendor" }
delta = {}

[[policy.tool]]
name = "mcp/mail/send"
requires = { audience = { contains = ["$to"] } }
delta = {}
parameters = { type = "object", properties = { to = { type = "string" } }, required = ["to"] }

[[policy.tool]]
name = "mcp/deploy/apply"
requires = { effects = { contains = ["backup.completed"] } }
delta = {}

[[policy.tool]]
name = "mcp/web/fetch"
delta = { trust = "untrusted" }

[[policy.tool]]
name = "mcp/files/read"
annotator = "classifier"
requires = { trust = "vendor" }

[[policy.tool]]
name = "mcp/review/publish"
requires = { attention = ["release-review"] }
delta = {}

[[policy.authority]]
name = "release-manager"
permits = { attention = ["release-review"] }

[externals]
timeout_ms = 2000
max_body_bytes = 65536

[externals.annotators.classifier]
url = "https://classifier.example/annotate"

[externals.authorities.release-manager]
builtin = "hitl"
"#;

fn defaults() -> HostDefaults {
    HostDefaults::new(Duration::from_millis(2000), 65_536)
}

fn compiled(document: &str) -> HostedContracts {
    HostedContracts::compile(document, defaults(), appa_adapter_claude_code::adapter())
        .unwrap_or_else(|error| panic!("the document compiles: {error}"))
}

/// `BASE` with each `(from, to)` replaced once.
fn edited(edits: &[(&str, &str)]) -> String {
    edits.iter().fold(BASE.to_string(), |document, (from, to)| {
        assert_eq!(document.matches(from).count(), 1, "the edit names one place: {from}");
        document.replacen(from, to, 1)
    })
}

const RUN_FLOOR: &str = r#"name = "mcp/shell/run"
requires = { trust = "vendor" }"#;
const MAIL_REQUIRES: &str = r#"requires = { audience = { contains = ["$to"] } }"#;
const DEPLOY_REQUIRES: &str = r#"requires = { effects = { contains = ["backup.completed"] } }"#;
const PUBLISH_REQUIRES: &str = r#"requires = { attention = ["release-review"] }"#;

/// One edit, and the direction every tool it touches moves in.
struct Case {
    name: &'static str,
    edits: &'static [(&'static str, &'static str)],
    expected: &'static [(&'static str, Direction)],
}

const CASES: &[Case] = &[
    Case {
        name: "no edit",
        edits: &[],
        expected: &[],
    },
    Case {
        name: "a higher trust floor",
        edits: &[(
            RUN_FLOOR,
            "name = \"mcp/shell/run\"\nrequires = { trust = \"trusted\" }",
        )],
        expected: &[("mcp/shell/run", Direction::Tighten)],
    },
    Case {
        name: "a lower trust floor",
        edits: &[(
            RUN_FLOOR,
            "name = \"mcp/shell/run\"\nrequires = { trust = \"untrusted\" }",
        )],
        expected: &[("mcp/shell/run", Direction::Loosen)],
    },
    Case {
        name: "a dropped trust floor",
        edits: &[(RUN_FLOOR, "name = \"mcp/shell/run\"")],
        expected: &[("mcp/shell/run", Direction::Loosen)],
    },
    Case {
        name: "an added contains requirement",
        edits: &[(
            RUN_FLOOR,
            "name = \"mcp/shell/run\"\nrequires = { trust = \"vendor\", audience = { contains = [\"internal\"] } }",
        )],
        expected: &[("mcp/shell/run", Direction::Tighten)],
    },
    Case {
        name: "a dropped contains requirement",
        edits: &[(MAIL_REQUIRES, "")],
        expected: &[("mcp/mail/send", Direction::Loosen)],
    },
    Case {
        name: "an added prior and an added no_prior",
        edits: &[(
            DEPLOY_REQUIRES,
            r#"requires = { effects = { contains = ["backup.completed", "tests.passed"], excludes = ["migration.applied"] } }"#,
        )],
        expected: &[("mcp/deploy/apply", Direction::Tighten)],
    },
    Case {
        name: "a dropped prior",
        edits: &[(DEPLOY_REQUIRES, "")],
        expected: &[("mcp/deploy/apply", Direction::Loosen)],
    },
    Case {
        name: "an added attention mark",
        edits: &[(
            RUN_FLOOR,
            "name = \"mcp/shell/run\"\nrequires = { trust = \"vendor\", attention = [\"release-review\"] }",
        )],
        expected: &[("mcp/shell/run", Direction::Tighten)],
    },
    Case {
        name: "a dropped attention mark",
        edits: &[(PUBLISH_REQUIRES, "")],
        expected: &[("mcp/review/publish", Direction::Loosen)],
    },
    Case {
        name: "a raised floor beside a dropped requirement",
        edits: &[(MAIL_REQUIRES, r#"requires = { trust = "trusted" }"#)],
        expected: &[("mcp/mail/send", Direction::Mixed)],
    },
    Case {
        name: "a tightened and a loosened tool",
        edits: &[
            (
                RUN_FLOOR,
                "name = \"mcp/shell/run\"\nrequires = { trust = \"trusted\" }",
            ),
            (DEPLOY_REQUIRES, ""),
        ],
        expected: &[
            ("mcp/deploy/apply", Direction::Loosen),
            ("mcp/shell/run", Direction::Tighten),
        ],
    },
    Case {
        name: "a raised Annotator floor",
        edits: &[(
            "annotator = \"classifier\"\nrequires = { trust = \"vendor\" }",
            "annotator = \"classifier\"\nrequires = { trust = \"trusted\" }",
        )],
        expected: &[("mcp/files/read", Direction::Tighten)],
    },
    Case {
        name: "a contract for a name nothing covered",
        edits: &[(
            "[[policy.authority]]",
            "[[policy.tool]]\nname = \"mcp/shell/kill\"\nrequires = { trust = \"trusted\" }\ndelta = {}\n\n[[policy.authority]]",
        )],
        expected: &[("mcp/shell/kill", Direction::Loosen)],
    },
    Case {
        name: "a contract dropped with nothing left to cover the name",
        edits: &[(
            "[[policy.tool]]\nname = \"mcp/web/fetch\"\ndelta = { trust = \"untrusted\" }\n",
            "",
        )],
        expected: &[("mcp/web/fetch", Direction::Tighten)],
    },
    Case {
        name: "a changed delta",
        edits: &[("delta = { trust = \"untrusted\" }", "delta = { trust = \"vendor\" }")],
        expected: &[("mcp/web/fetch", Direction::Unknown)],
    },
    Case {
        name: "an argument predicate",
        edits: &[(
            "[[policy.authority]]",
            "[[policy.tool]]\nname = \"mcp/shell/run(command:git *)\"\nrequires = { trust = \"untrusted\" }\ndelta = {}\n\n[[policy.authority]]",
        )],
        expected: &[("mcp/shell/run", Direction::Unknown)],
    },
    Case {
        name: "an Annotator route replaced by a declared contract",
        edits: &[(
            "annotator = \"classifier\"\nrequires = { trust = \"vendor\" }",
            "requires = { trust = \"vendor\" }\ndelta = {}",
        )],
        expected: &[("mcp/files/read", Direction::Unknown)],
    },
    Case {
        name: "an authority's mandate",
        edits: &[(
            "permits = { attention = [\"release-review\"] }",
            "permits = { attention = [\"release-review\"], trust_below = \"trusted\" }",
        )],
        expected: &[("*", Direction::Unknown)],
    },
    Case {
        name: "a widened Annotator mandate",
        edits: &[("effects = []", "effects = [\"backup.completed\"]")],
        expected: &[("mcp/files/read", Direction::Unknown)],
    },
    Case {
        name: "an Annotator's binding",
        edits: &[(
            "url = \"https://classifier.example/annotate\"",
            "url = \"https://lenient.example/annotate\"",
        )],
        expected: &[("*", Direction::Unknown)],
    },
    Case {
        name: "a tightened tool beside a policy-wide change",
        edits: &[
            (
                RUN_FLOOR,
                "name = \"mcp/shell/run\"\nrequires = { trust = \"trusted\" }",
            ),
            (
                "trust_chain = [\"untrusted\", \"vendor\", \"trusted\"]",
                "trust_chain = [\"untrusted\", \"vendor\", \"partner\", \"trusted\"]",
            ),
        ],
        // Ranks compare only within one chain, and the classifier's mandate ranges over it.
        expected: &[
            ("*", Direction::Unknown),
            ("mcp/files/read", Direction::Unknown),
            ("mcp/shell/run", Direction::Unknown),
        ],
    },
];

#[test]
fn every_edit_moves_its_tools_in_the_direction_its_contracts_say() {
    let before = compiled(BASE);
    for case in CASES {
        let after = compiled(&edited(case.edits));
        let diff = before.diff(&after);
        let directions: Vec<(&str, Direction)> = diff.iter().map(|tool| (tool.tool.as_str(), tool.direction)).collect();
        assert_eq!(directions, case.expected, "{}: {diff:#?}", case.name);
        assert_eq!(
            after.diff(&after),
            Vec::new(),
            "{}: a document equals itself",
            case.name
        );
    }
}

/// Reversing an edit swaps tighten and loosen and keeps every other direction: no edit and
/// its undo can both tighten.
#[test]
fn an_undone_edit_moves_the_other_way() {
    let base = compiled(BASE);
    for case in CASES {
        let after = compiled(&edited(case.edits));
        let forward = base.diff(&after);
        let backward = after.diff(&base);
        let flipped: Vec<(String, Direction)> = forward
            .iter()
            .map(|tool| {
                let direction = match tool.direction {
                    Direction::Tighten => Direction::Loosen,
                    Direction::Loosen => Direction::Tighten,
                    other => other,
                };
                (tool.tool.clone(), direction)
            })
            .collect();
        let undone: Vec<(String, Direction)> = backward
            .iter()
            .map(|tool| (tool.tool.clone(), tool.direction))
            .collect();
        assert_eq!(undone, flipped, "{}", case.name);
    }
}

#[test]
fn a_diff_names_what_changed_and_flags_predicates_and_routing() {
    let base = compiled(BASE);
    let raised = base.diff(&compiled(&edited(&[(
        RUN_FLOOR,
        "name = \"mcp/shell/run\"\nrequires = { trust = \"trusted\" }",
    )])));
    let change = &raised[0].changes[0];
    assert_eq!(
        (change.kind, change.before.as_deref(), change.after.as_deref()),
        (ChangeKind::TrustFloor, Some("vendor"), Some("trusted"))
    );
    assert!(!raised[0].annotator_changed && !raised[0].argument_predicates);

    let predicate = base.diff(&compiled(&edited(&[(
        "[[policy.authority]]",
        "[[policy.tool]]\nname = \"mcp/shell/run(command:git *)\"\ndelta = {}\n\n[[policy.authority]]",
    )])));
    assert!(predicate[0].argument_predicates, "{predicate:#?}");

    let rerouted = base.diff(&compiled(&edited(&[(
        "annotator = \"classifier\"\nrequires = { trust = \"vendor\" }",
        "requires = { trust = \"vendor\" }\ndelta = {}",
    )])));
    assert!(rerouted[0].annotator_changed, "{rerouted:#?}");

    let rebound = base.diff(&compiled(&edited(&[(
        "url = \"https://classifier.example/annotate\"",
        "url = \"https://lenient.example/annotate\"",
    )])));
    assert_eq!(rebound[0].changes[0].kind, ChangeKind::Bindings);
}

fn label(trust: &str, audience: &[&[&str]]) -> DecisionLabel {
    DecisionLabel {
        trust: trust.to_string(),
        audience: audience
            .iter()
            .map(|clause| clause.iter().map(|entry| entry.to_string()).collect())
            .collect(),
    }
}

fn call(tool: &str, arguments: serde_json::Value, label: DecisionLabel, effects: &[&str]) -> ContractCall {
    ContractCall {
        tool: tool.to_string(),
        arguments: arguments.to_string(),
        label,
        effects: effects.iter().map(|effect| effect.to_string()).collect(),
    }
}

#[test]
fn the_contracts_alone_allow_deny_or_name_the_consult_a_call_needs() {
    let contracts = compiled(BASE);
    let none = serde_json::json!({});
    let mail = |to: &str| serde_json::json!({ "to": to });
    let public = || label("trusted", &[]);
    let cases: Vec<(&str, ContractCall, ContractVerdict)> = vec![
        (
            "met floor",
            call("mcp/shell/run", none.clone(), label("vendor", &[]), &[]),
            ContractVerdict::Allow,
        ),
        (
            "missed floor",
            call("mcp/shell/run", none.clone(), label("untrusted", &[]), &[]),
            ContractVerdict::Deny {
                gaps: vec![DecisionGap::TrustFloor {
                    required: "vendor".to_string(),
                    actual: "untrusted".to_string(),
                }],
            },
        ),
        (
            "public data to anyone",
            call("mcp/mail/send", mail("bob@example.com"), public(), &[]),
            ContractVerdict::Allow,
        ),
        (
            "data alice may read, to bob",
            call(
                "mcp/mail/send",
                mail("bob@example.com"),
                label("trusted", &[&["alice@example.com"]]),
                &[],
            ),
            ContractVerdict::Deny {
                gaps: vec![DecisionGap::Includes {
                    required: vec!["bob@example.com".to_string()],
                }],
            },
        ),
        (
            "committed prior",
            call("mcp/deploy/apply", none.clone(), public(), &["backup.completed"]),
            ContractVerdict::Allow,
        ),
        (
            "missing prior",
            call("mcp/deploy/apply", none.clone(), public(), &[]),
            ContractVerdict::Deny {
                gaps: vec![DecisionGap::Prior {
                    effect: "backup.completed".to_string(),
                }],
            },
        ),
        (
            "attention",
            call("mcp/review/publish", none.clone(), public(), &[]),
            ContractVerdict::Deny {
                gaps: vec![DecisionGap::Attention {
                    mark: "release-review".to_string(),
                }],
            },
        ),
        (
            "a narrowing alone",
            call("mcp/web/fetch", none.clone(), public(), &[]),
            ContractVerdict::Allow,
        ),
        (
            "annotated, floor met",
            call("mcp/files/read", none.clone(), public(), &[]),
            ContractVerdict::NeedsAnnotator {
                annotator: "classifier".to_string(),
            },
        ),
        (
            "annotated, floor missed",
            call("mcp/files/read", none.clone(), label("untrusted", &[]), &[]),
            ContractVerdict::Deny {
                gaps: vec![DecisionGap::TrustFloor {
                    required: "vendor".to_string(),
                    actual: "untrusted".to_string(),
                }],
            },
        ),
    ];
    for (name, call, expected) in cases {
        assert_eq!(contracts.check(&call), Ok(expected.clone()), "{name}");
        assert_eq!(
            contracts.check(&call),
            Ok(expected),
            "{name}: the same call, the same verdict"
        );
    }
    for (name, refused) in [
        ("no contract", call("mcp/shell/kill", none.clone(), public(), &[])),
        ("missing argument", call("mcp/mail/send", none.clone(), public(), &[])),
    ] {
        assert!(
            matches!(contracts.check(&refused), Ok(ContractVerdict::Refused { .. })),
            "{name}: {:?}",
            contracts.check(&refused)
        );
    }
    assert!(
        contracts
            .check(&call("mcp/shell/run", none, label("root", &[]), &[]))
            .is_err()
    );
}

fn actor() -> Actor {
    Actor {
        root: TrajectoryId("conversation-1".into()),
        child: None,
    }
}

fn proposed(tool: &str) -> HookEvent {
    HookEvent::ToolCall {
        call_id: None,
        actor: actor(),
        call: ProposedCall {
            tool: tool.to_string(),
            arguments: serde_json::value::RawValue::from_string("{}".into()).expect("an object"),
            cwd: None,
        },
        spawn: None,
        prompt: None,
        ruling: None,
    }
}

/// A served decision carries the label its call was checked at and, for a block, the gaps
/// and narrowing the feedback describes; read back under the same policy, the label and gaps
/// are what the contracts alone say of that call.
#[tokio::test]
async fn a_decision_carries_its_label_gaps_and_narrowing() {
    let config = Config::hosted_composed(BASE, &[], defaults(), |_| None).expect("the document composes");
    let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
    let runtime = Runtime::open_with_store_as(config, store, None, appa_adapter_claude_code::adapter())
        .expect("the runtime opens");
    let started = hooks::handle(
        &runtime,
        HookEvent::SessionStart {
            root: actor().root,
            principal: None,
            address: None,
            title: None,
            launch: None,
            start: None,
        },
    )
    .await;
    assert_eq!(started, HookDecision::Ack);
    let top = label("trusted", &[]);

    let allowed = hooks::handle_embedded(&runtime, proposed("mcp/shell/run")).await;
    assert!(
        matches!(allowed.decision, HookDecision::AllowCall { .. }),
        "{allowed:?}"
    );
    assert_eq!(allowed.label, Some(top.clone()));

    let blocked = hooks::handle_embedded(&runtime, proposed("mcp/review/publish")).await;
    let HookDecision::DenyCall { gaps, narrowing, .. } = &blocked.decision else {
        panic!("an unattended mark blocks: {blocked:?}");
    };
    let expected = vec![DecisionGap::Attention {
        mark: "release-review".to_string(),
    }];
    assert_eq!((gaps, narrowing), (&expected, &None));
    assert_eq!(blocked.label, Some(top.clone()));
    assert_eq!(
        compiled(BASE).check(&call("mcp/review/publish", serde_json::json!({}), top.clone(), &[])),
        Ok(ContractVerdict::Deny { gaps: expected })
    );

    let narrowed = hooks::handle_embedded(&runtime, proposed("mcp/web/fetch")).await;
    let HookDecision::DenyCall { gaps, narrowing, .. } = &narrowed.decision else {
        panic!("a narrowing waits for its acceptance: {narrowed:?}");
    };
    assert!(gaps.is_empty(), "{gaps:?}");
    assert_eq!(
        narrowing,
        &Some(DecisionNarrowing {
            from: top.clone(),
            to: label("untrusted", &[]),
        })
    );
}
