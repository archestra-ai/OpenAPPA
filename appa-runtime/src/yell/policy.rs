//! The policy a trajectory ran under, stripped for export.
//!
//! A report carries the decisions and the rules they were made under, because either alone is
//! unreadable: a denial means nothing without the clause that produced it. The rules are the
//! composed `[policy]` table as the loader holds it, walked against the same deny-by-default
//! machinery the facts use — see [`super::strip`] — against the inventory below.
//!
//! # Why this is a second inventory and not the same one
//!
//! The facts are the *engine's* serde and their inventory is written against it. This is the
//! deployment's own TOML, whose schema is `appa_policy`'s `Raw*` structs, and the two share
//! neither keys nor shapes. Only the walk is shared, and with it the property that matters:
//! a key the inventory does not name loses its value and says so.
//!
//! Every `Raw*` struct is `#[serde(deny_unknown_fields)]`, so a loaded policy holds no key
//! outside this schema. That makes drift here mean one thing — the schema grew and this
//! inventory did not — rather than "a deployment wrote something unexpected".
//!
//! Two of the six free-form `toml::Value` fields the schema does hold — the inline
//! `implementation` on a tool, authority, sanitizer or annotator, and `url`/`command` on
//! `[identity]` — are refused during conversion (`ConfigError::ForbiddenInlineBinding`), so a
//! loaded policy carries none. They are deliberately *not* named below: one appearing in a
//! pinned document means the loader's contract changed, and drift is the only thing that
//! would say so.
//!
//! # What the two modes differ on
//!
//! Baseline is the deployment reading its own policy back, so it spells the names it chose;
//! pseudonymization replaces them. What differs between the modes is naming, and only naming.
//!
//! Prose is in neither. A `description` and a `hint` are the deployer's own sentences, and a
//! sentence is bounded by nothing: it holds a path, an endpoint, the name of a credential
//! variable, or a colleague, as readily as it holds the reason a rule exists. The structure
//! is what explains a decision, and the structure is all of it that leaves.

use serde_json::Value;

use super::strip::{Rule, Stripped, Table, strip};
use super::tokens::{Class, Mode, Tokens};

const TRUST: Rule = Rule::Token(Class::Trust);
const EFFECT: Rule = Rule::Token(Class::Effect);
const MARK: Rule = Rule::Token(Class::Mark);
/// One entry of an audience list as a policy writes it. Four unlike things share the slot;
/// [`Rule::AudienceToken`] carries each as what it is.
const READER: Rule = Rule::AudienceToken;
const NUMBER: Rule = Rule::Keep;

// ---------------------------------------------------------------- labels

/// `[boundary]` and `[[tool]].delta`: a trust rank and a list of audience tokens.
static LABEL_WRITE: Table = Table {
    name: "policy.delta",
    entries: &[("trust", TRUST), ("audience", Rule::Each(&READER))],
};

static REQUIRES_AUDIENCE: Table = Table {
    name: "policy.requires.audience",
    entries: &[("contains", Rule::Each(&READER)), ("within", Rule::Each(&READER))],
};

static HISTORY: Table = Table {
    name: "policy.requires.effects",
    entries: &[("contains", Rule::Each(&EFFECT)), ("excludes", Rule::Each(&EFFECT))],
};

static REQUIRES: Table = Table {
    name: "policy.requires",
    entries: &[
        ("trust", TRUST),
        ("audience", Rule::Table(&REQUIRES_AUDIENCE)),
        ("effects", Rule::Table(&HISTORY)),
        ("attention", Rule::Each(&MARK)),
    ],
};

// ---------------------------------------------------------------- declarations

static TOOL: Table = Table {
    name: "policy.tool",
    entries: &[
        ("name", Rule::Token(Class::Tool)),
        ("description", Rule::Never),
        ("tags", Rule::Elements(Class::Tag)),
        ("delta", Rule::Table(&LABEL_WRITE)),
        ("requires", Rule::Table(&REQUIRES)),
        ("effects", Rule::Elements(Class::Effect)),
        // The parameter schema a deployment authored for this tool. Its property names and
        // literals are authored text exactly as a return contract's are, and the same rule
        // reads both: they are the same dialect.
        ("parameters", Rule::ReturnSchema),
        ("annotator", Rule::Token(Class::Annotator)),
    ],
};

static PERMITS: Table = Table {
    name: "policy.authority.permits",
    entries: &[
        ("trust_below", TRUST),
        ("audience_missing", Rule::Each(&READER)),
        ("effects_containing", Rule::Elements(Class::Effect)),
        ("attention", Rule::Elements(Class::Mark)),
    ],
};

static AUTHORITY: Table = Table {
    name: "policy.authority",
    entries: &[
        ("name", Rule::Token(Class::Authority)),
        ("hint", Rule::Never),
        ("permits", Rule::Table(&PERMITS)),
        ("tags", Rule::Elements(Class::Tag)),
    ],
};

static AUDIENCE_TRANSITION: Table = Table {
    name: "policy.sanitizer.permits.audience",
    entries: &[("from", Rule::Each(&READER)), ("to", Rule::Each(&READER))],
};

static TRUST_TRANSITION: Table = Table {
    name: "policy.sanitizer.permits.trust",
    entries: &[("from", TRUST), ("to", TRUST)],
};

static SANITIZER_PERMITS: Table = Table {
    name: "policy.sanitizer.permits",
    entries: &[
        ("audience", Rule::Table(&AUDIENCE_TRANSITION)),
        ("trust", Rule::Table(&TRUST_TRANSITION)),
    ],
};

static SANITIZER: Table = Table {
    name: "policy.sanitizer",
    entries: &[
        ("name", Rule::Token(Class::Sanitizer)),
        // Which surfaces this sanitizer binds to.
        ("on", Rule::Elements(Class::Surface)),
        ("hint", Rule::Never),
        ("tags", Rule::Elements(Class::Tag)),
        ("permits", Rule::Table(&SANITIZER_PERMITS)),
    ],
};

static ANNOTATOR: Table = Table {
    name: "policy.annotator",
    entries: &[
        ("name", Rule::Token(Class::Annotator)),
        ("hint", Rule::Never),
        // A stock transport's name: `claude-code`, `llm`. Engine vocabulary, not the
        // deployment's.
        ("builtin", Rule::Keep),
        // Consult inputs: each key is an input name this Annotator reads, each value a
        // `$tool_call` source expression naming an argument.
        ("inputs", Rule::MapKeys(Class::Field, &Rule::Token(Class::Argument))),
        ("ranks", Rule::Elements(Class::Trust)),
        ("audiences", Rule::Each(&READER)),
        ("marks", Rule::Elements(Class::Mark)),
        ("effects", Rule::Elements(Class::Effect)),
    ],
};

// ---------------------------------------------------------------- audiences and identity

/// Where a built-in audience level gets its members. Each entry is `provider:selector`, and
/// the selector is never carried: the loader accepts an instantiated
/// `google-workspace:group/finance@corp.example` wherever it accepts the template
/// `group/<group-address>`, so an address is exactly as likely there as a word.
static AUDIENCE_LEVEL: Table = Table {
    name: "policy.audience.level",
    entries: &[("from", Rule::Each(&SOURCE))],
};

/// One `provider:selector` audience source, wherever a policy writes one.
static SOURCE: Rule = Rule::AudienceSource;

static AUDIENCE_GROUP: Table = Table {
    name: "policy.audience.group",
    entries: &[
        ("name", Rule::Token(Class::Group)),
        // `within` asserts containment in a built-in audience, so its only values are `self`
        // and `internal` — the engine's words, not a group the deployment named.
        ("within", Rule::Keep),
        ("from", Rule::Each(&SOURCE)),
    ],
};

static AUDIENCE: Table = Table {
    name: "policy.audience",
    entries: &[
        ("self", Rule::Table(&AUDIENCE_LEVEL)),
        ("internal", Rule::Table(&AUDIENCE_LEVEL)),
        ("group", Rule::Each(&Rule::Table(&AUDIENCE_GROUP))),
    ],
};

static IDENTITY: Table = Table {
    name: "policy.identity",
    entries: &[("implementation", Rule::Token(Class::Identity))],
};

// ---------------------------------------------------------------- deployment

static STARTING_LABEL: Table = Table {
    name: "policy.deployment.starting_label",
    entries: &[
        ("trust", TRUST),
        // Untagged in the schema: either the bare token `public` or a list of them, and both
        // shapes reach this one entry.
        ("audience", Rule::OneOrMany(&READER)),
    ],
};

static DEPLOYMENT: Table = Table {
    name: "policy.deployment",
    entries: &[
        ("starting_label", Rule::Table(&STARTING_LABEL)),
        // Closed sets the engine defines: `harness`/`runtime`, `assumed`/`provider_run`.
        ("binding", Rule::Keep),
        ("context_control", Rule::Keep),
        ("dispatch", Rule::Keep),
        ("assumed_tools", Rule::Elements(Class::Tool)),
        ("provider_run_tools", Rule::Elements(Class::Tool)),
        ("confined_results", Rule::Elements(Class::Tool)),
        ("provider_surfaces", Rule::MapKeys(Class::Surface, &Rule::Keep)),
    ],
};

static LIMITS: Table = Table {
    name: "policy.limits",
    entries: &[("planner_cap", NUMBER)],
};

static POLICY: Table = Table {
    name: "policy",
    entries: &[
        ("version", NUMBER),
        ("trust_chain", Rule::Elements(Class::Trust)),
        ("boundary", Rule::Table(&LABEL_WRITE)),
        ("tool", Rule::Each(&Rule::Table(&TOOL))),
        ("authority", Rule::Each(&Rule::Table(&AUTHORITY))),
        ("sanitizer", Rule::Each(&Rule::Table(&SANITIZER))),
        ("annotator", Rule::Each(&Rule::Table(&ANNOTATOR))),
        ("audience", Rule::Table(&AUDIENCE)),
        ("identity", Rule::Table(&IDENTITY)),
        ("limits", Rule::Table(&LIMITS)),
        ("deployment", Rule::Table(&DEPLOYMENT)),
    ],
};

/// The composed `[policy]` table, stripped.
///
/// The document arrives as TOML and is converted to JSON first, because the walk reads JSON
/// and because that is the shape the report carries anyway. A document that will not convert
/// — which a loaded policy's cannot be, since it round-tripped through serde already — takes
/// the same path an unclassifiable value does.
pub(crate) fn strip_policy(document: &toml::Value, tokens: &mut Tokens, mode: Mode) -> Stripped {
    let Ok(json) = serde_json::to_value(document) else {
        return Stripped {
            value: Value::String(super::strip::UNCLASSIFIED.to_string()),
            unclassified: vec![super::strip::Drift {
                path: String::new(),
                table: POLICY.name,
            }],
        };
    };
    // No policy is vouched *by* a policy: the names in this document are the deployment's by
    // definition, which is what the whole inventory says. The set is the document's own tool
    // names, so the two sections agree on which spellings Baseline shows.
    let vouched = declared_tools(&json);
    strip(&json, &POLICY, tokens, mode, &vouched)
}

/// Every name the `[[policy.tool]]` entries write, which is exactly what makes a tool name
/// the deployment's own — the same question `RuntimeEngine::vouched_tools` answers for the
/// facts, asked here of the document itself so that the two never disagree.
fn declared_tools(document: &Value) -> std::collections::BTreeSet<String> {
    document
        .get("tool")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.get("name")?.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strip a fixture that the policy compiler accepts.
    ///
    /// The compile is the point. A fixture that only parses as TOML proves nothing about this
    /// inventory: it can name a field the schema does not have, or spell a closed value the
    /// loader rejects, and still walk cleanly. Going through `appa_policy::Config` makes the
    /// fixture a policy a deployment could actually run, so "every section is classified"
    /// means what it says.
    fn stripped(text: &str, mode: Mode) -> Stripped {
        appa_policy::Config::from_toml_str(text).expect("the fixture is a policy the loader accepts");
        let document: toml::Value = toml::from_str(text).expect("the fixture parses");
        let mut tokens = Tokens::default();
        strip_policy(&document, &mut tokens, mode)
    }

    /// Strip a fixture the loader would refuse. Only for the cases whose whole point is input
    /// outside the schema — everything a deployment can actually run goes through `stripped`.
    fn stripped_raw(text: &str, mode: Mode) -> Stripped {
        let document: toml::Value = toml::from_str(text).expect("the fixture parses");
        let mut tokens = Tokens::default();
        strip_policy(&document, &mut tokens, mode)
    }

    const FIXTURE: &str = r#"
        version = 2
        trust_chain = ["suspicious", "trusted"]

        [boundary]
        trust = "trusted"

        [[tool]]
        name = "Bash"
        description = "runs a shell command on /Users/alice/work"
        effects = ["shell"]

        [[tool]]
        name = "Artifact"
        [tool.requires]
        trust = "trusted"
        [tool.requires.audience]
        contains = ["public"]

        [[authority]]
        name = "hitl"
        hint = "ask alice@corp.example first"
        [authority.permits]
        audience_missing = ["public"]

        [[sanitizer]]
        name = "redact"
        on = ["tool_output"]
        [sanitizer.permits.trust]
        from = "suspicious"
        to = "trusted"

        [audience]
        [audience.internal]
        from = ["google-workspace:full-members"]
        [[audience.group]]
        name = "finance"
        within = "internal"
        from = ["google-workspace:group/finance@corp.example"]

        [identity]
        implementation = "verified-email"

        [deployment]
        context_control = true
        confined_results = ["Bash"]
    "#;

    /// Every policy this repository ships, composed by the real loader and walked.
    ///
    /// The fixture above proves the rules a fixture happens to use. This proves the rules real
    /// policies use, and it is the part that actually couples the inventory to `appa_policy`:
    /// a schema that grows a field, or a shipped policy that starts writing one, fails here
    /// with the path that needs a line.
    #[test]
    fn every_shipped_policy_is_classified() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits in the repository");
        for relative in DEPLOYMENTS {
            let path = repository.join(relative);
            let config = crate::config::Config::load(&path)
                .unwrap_or_else(|error| panic!("{} does not load: {error}", path.display()));
            classified(config.policy_file().value(), &path);
        }
    }

    fn classified(document: &toml::Value, path: &std::path::Path) {
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let mut tokens = Tokens::default();
            let stripped = strip_policy(document, &mut tokens, mode);
            assert!(
                stripped.unclassified.is_empty(),
                "{} in {mode:?}: the inventory does not cover {:?}",
                path.display(),
                stripped.unclassified
            );
        }
    }

    /// Deployment configurations, each composing (through its includes) to a policy a
    /// deployment could run.
    const DEPLOYMENTS: &[&str] = &[
        "integrations/claude-code/examples/claude-code.appa.toml",
        "integrations/claude-code/examples/claude-code-hitl.appa.toml",
        "examples/claude-code-battery/appa.toml",
        "bench/corp/policies/appa.toml",
        "bench/corp/policies/open.toml",
    ];

    /// The acceptance criterion for this inventory: a policy exercising every section is
    /// classified end to end, so a schema that grows past it fails here.
    #[test]
    fn a_policy_using_every_section_is_classified() {
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped(FIXTURE, mode);
            assert!(
                stripped.unclassified.is_empty(),
                "the inventory does not cover {:?} in {mode:?}",
                stripped.unclassified
            );
        }
    }

    /// The loader accepts an instantiated selector wherever it accepts the template that
    /// address fills, so `from` carries an address as readily as a provider's own word and
    /// nothing in the spelling says which. The provider half is the deployment's vocabulary
    /// and stays; the selector half goes in both modes.
    #[test]
    fn an_audience_source_never_spells_its_selector() {
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped(FIXTURE, mode);
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            for spelled in ["finance@corp.example", "group/", "full-members"] {
                assert!(!rendered.contains(spelled), "{spelled} survived {mode:?}: {rendered}");
            }
        }
        let baseline = stripped(FIXTURE, Mode::Baseline);
        let rendered = serde_json::to_string(&baseline.value).expect("serializes");
        assert!(
            rendered.contains("google-workspace:selector-"),
            "the provider a policy names is still readable: {rendered}"
        );
    }

    /// Baseline is the deployment reading its own rules back, so its own names and its own
    /// prose are the point of the mode.
    #[test]
    fn baseline_reads_back_the_deployment_s_own_policy() {
        let stripped = stripped(FIXTURE, Mode::Baseline);
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        for spelled in ["Bash", "Artifact", "hitl", "redact", "finance", "trusted"] {
            assert!(rendered.contains(spelled), "{spelled} is missing from baseline");
        }
        assert_eq!(stripped.value["version"], 2);
    }

    /// Pseudonymization replaces the names and drops the prose, and prose is where a path or
    /// an address hides in a document that is otherwise all vocabulary.
    #[test]
    fn pseudonymization_drops_the_prose_and_the_names() {
        let stripped = stripped(FIXTURE, Mode::Pseudonymized);
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        for spelled in [
            "Bash",
            "hitl",
            "redact",
            "finance",
            "/Users/alice",
            "alice@corp.example",
        ] {
            assert!(!rendered.contains(spelled), "{spelled} survived pseudonymization");
        }
        // The shape survives: two tools, one authority, one sanitizer.
        assert_eq!(stripped.value["tool"].as_array().expect("an array").len(), 2);
        assert_eq!(stripped.value["version"], 2);
    }

    /// A hint and a description are the deployer's own sentences, and a sentence is bounded by
    /// nothing: it holds a path or a colleague as readily as the reason a rule exists. The
    /// structure explains the decision; the prose leaves in neither mode.
    #[test]
    fn prose_leaves_in_neither_mode() {
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped(FIXTURE, mode);
            assert!(stripped.value["authority"][0].get("hint").is_none(), "{mode:?}");
            assert!(
                stripped.value["tool"][0].get("description").is_none(),
                "a description is prose too, in {mode:?}"
            );
            assert!(stripped.unclassified.is_empty(), "prose is classified, not drift");
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            for spelled in ["/Users/alice", "alice@corp.example"] {
                assert!(!rendered.contains(spelled), "{spelled} survived {mode:?}");
            }
        }
    }

    /// An inline binding cannot reach a loaded policy — the loader refuses one everywhere it
    /// can be written. So the inventory deliberately does not name it: if one ever appears in
    /// a pinned document, the loader's contract changed, and drift is the only thing that
    /// would say so. It carries nothing either way.
    #[test]
    fn an_inline_binding_drifts_rather_than_being_quietly_dropped() {
        let text = r#"
            version = 2
            trust_chain = ["trusted"]
            [identity]
            implementation = "corp-directory"
            [identity.url]
            url = "https://directory.corp.example/whoami"
            token_env = "APPA_PROVIDER_DIRECTORY"
        "#;
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped_raw(text, mode);
            assert_eq!(
                stripped
                    .unclassified
                    .iter()
                    .map(|drift| drift.path.as_str())
                    .collect::<Vec<_>>(),
                vec!["identity.url"],
                "the walk says where it found something it does not know"
            );
            let rendered = serde_json::to_string(&stripped.value).expect("serializes");
            assert!(
                !rendered.contains("directory.corp.example") && !rendered.contains("APPA_PROVIDER"),
                "a binding survived {mode:?}: {rendered}"
            );
        }
    }

    /// An audience list puts four unlike things in one slot. The engine's own words stay
    /// readable — `contains = ["public"]` is the commonest clause a policy writes — while a
    /// reader written out is a person and is tokenized in both modes.
    #[test]
    fn an_audience_list_carries_each_entry_as_what_it_is() {
        let text = r#"
            version = 2
            trust_chain = ["trusted"]
            [audience]
            [[audience.group]]
            name = "finance"
            within = "internal"
            from = ["slack:user-group/<handle>"]
            [[tool]]
            name = "Send"
            [tool.requires.audience]
            contains = ["internal", "@finance", "alice@corp.example"]
        "#;
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped(text, mode);
            assert!(
                stripped.unclassified.is_empty(),
                "every entry is classified in {mode:?}"
            );
            let carried = stripped.value["tool"][0]["requires"]["audience"]["contains"].clone();
            let entries: Vec<&str> = carried
                .as_array()
                .expect("an array")
                .iter()
                .map(|entry| entry.as_str().expect("a string"))
                .collect();
            assert_eq!(entries[0], "internal", "the engine's own word, in {mode:?}");
            assert_ne!(entries[2], "alice@corp.example", "a written-out reader is a person");
            let rendered = serde_json::to_string(&carried).expect("serializes");
            assert!(!rendered.contains("alice"), "the address survived {mode:?}: {rendered}");
        }
        // Baseline still spells the group the deployment named; pseudonymization does not.
        let baseline = stripped(text, Mode::Baseline);
        assert_eq!(
            baseline.value["tool"][0]["requires"]["audience"]["contains"][1],
            "@finance"
        );
        let pseudonymized = stripped(text, Mode::Pseudonymized);
        assert_ne!(
            pseudonymized.value["tool"][0]["requires"]["audience"]["contains"][1],
            "@finance"
        );
    }

    /// A `$argument` placeholder keeps its shape and loses the model-facing key it names. It
    /// stands alone in a `contains`, so it gets a fixture of its own.
    #[test]
    fn an_argument_placeholder_keeps_its_shape() {
        let text = r#"
            version = 2
            trust_chain = ["trusted"]
            [[tool]]
            name = "Send"
            parameters = { type = "object", properties = { recipients = { type = "string" } }, required = ["recipients"] }
            [tool.requires.audience]
            contains = ["$recipients"]
        "#;
        for mode in [Mode::Baseline, Mode::Pseudonymized] {
            let stripped = stripped(text, mode);
            let carried = stripped.value["tool"][0]["requires"]["audience"]["contains"][0].clone();
            let spelled = carried.as_str().expect("a string");
            assert!(spelled.starts_with('$'), "a placeholder stays one in {mode:?}");
            assert_ne!(spelled, "$recipients", "the key it names is the model's namespace");
        }
    }

    /// The property the walk exists for, asserted here too because this inventory is written
    /// against a different schema from the facts'.
    #[test]
    fn a_key_outside_the_schema_loses_its_value() {
        let text = r#"
            version = 2
            [surprise]
            secret = "s3cret-value"
        "#;
        let stripped = stripped_raw(text, Mode::Baseline);
        assert_eq!(stripped.value["surprise"], super::super::strip::UNCLASSIFIED);
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        assert!(!rendered.contains("s3cret-value"));
    }
}
