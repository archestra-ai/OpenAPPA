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
//! # What the two modes differ on
//!
//! Baseline is the deployment reading its own policy back, so it spells its own names and
//! keeps the prose it wrote. Pseudonymization replaces the names and drops the prose. Neither
//! mode carries an endpoint, a bearer token, a command's argv or its working directory: those
//! are the deployment's infrastructure rather than its rules, and no reader of a report needs
//! them to follow a decision.

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
        ("description", Rule::Prose),
        ("tags", Rule::Elements(Class::Effect)),
        ("delta", Rule::Table(&LABEL_WRITE)),
        ("requires", Rule::Table(&REQUIRES)),
        ("effects", Rule::Elements(Class::Effect)),
        // The parameter schema a deployment authored for this tool. Its property names and
        // literals are authored text exactly as a return contract's are, and the same rule
        // reads both: they are the same dialect.
        ("parameters", Rule::ReturnSchema),
        ("annotator", Rule::Token(Class::Annotator)),
        // `implementation` is refused at compile time for a tool, an authority and a
        // sanitizer alike (`ConfigError::ForbiddenInlineBinding`), so a loaded policy carries
        // none. Named anyway, and carried in no mode: if the loader ever admits one it holds
        // a URL or an argv, and this is not the place to discover that.
        ("implementation", Rule::Never),
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
        ("hint", Rule::Prose),
        ("permits", Rule::Table(&PERMITS)),
        ("tags", Rule::Elements(Class::Effect)),
        ("implementation", Rule::Never),
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
        ("hint", Rule::Prose),
        ("tags", Rule::Elements(Class::Effect)),
        ("permits", Rule::Table(&SANITIZER_PERMITS)),
        ("implementation", Rule::Never),
    ],
};

static ANNOTATOR: Table = Table {
    name: "policy.annotator",
    entries: &[
        ("name", Rule::Token(Class::Annotator)),
        ("hint", Rule::Prose),
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
        ("implementation", Rule::Never),
    ],
};

// ---------------------------------------------------------------- audiences and identity

/// A selector as the policy *templates* it — `group/<group-address>` — which is the
/// deployment's own text, unlike the instantiated selector that reaches a fact. Carried as a
/// source token so the two halves of `provider:selector` stay legible.
static AUDIENCE_LEVEL: Table = Table {
    name: "policy.audience.level",
    entries: &[("from", Rule::Elements(Class::Source))],
};

static AUDIENCE_GROUP: Table = Table {
    name: "policy.audience.group",
    entries: &[
        ("name", Rule::Token(Class::Group)),
        ("within", Rule::Token(Class::Group)),
        ("from", Rule::Elements(Class::Source)),
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
    entries: &[
        ("implementation", Rule::Token(Class::Identity)),
        // An inline binding: a URL, or an argv and a working directory. Infrastructure, not
        // rules.
        ("url", Rule::Never),
        ("command", Rule::Never),
    ],
};

// ---------------------------------------------------------------- deployment

static STARTING_LABEL: Table = Table {
    name: "policy.deployment.starting_label",
    entries: &[
        ("trust", TRUST),
        // Either the bare token `public` or a list of audience tokens, so both shapes reach
        // this entry and the rule has to read both.
        ("audience", Rule::Each(&READER)),
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

/// How a deployment's externals are bound, without any of what they are bound to.
///
/// Written as a typed projection rather than a walk, because unlike the policy document this
/// comes from Rust structs and not from free-form TOML: the fields are known at compile time,
/// so the deny-by-default machinery has nothing to protect against here. What a reader needs
/// is which names are bound and how they are served — an authority answering over HTTP and
/// one running a local command fail in entirely different ways — and that is all this is.
#[derive(Debug, serde::Serialize)]
pub(crate) struct Externals {
    pub(crate) timeout_ms: u64,
    pub(crate) review_timeout_ms: u64,
    pub(crate) max_body_bytes: usize,
    pub(crate) bindings: Vec<Binding>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Binding {
    /// Which kind of external this is: `authority`, `sanitizer`, `annotator`,
    /// `audience_source`, `identity`. The same words the runtime events use, so a reader can
    /// line a binding up with the consults it answered.
    pub(crate) role: &'static str,
    pub(crate) name: String,
    pub(crate) served: Served,
}

/// How one binding is served. An endpoint's URL is tokenized rather than carried: it is the
/// deployment's address, not its vocabulary, and the one thing worth knowing — that two
/// bindings reach the same service — survives a token. A command's argv and working directory
/// are filesystem paths and are carried nowhere; that it *is* a command, and whether it takes
/// a credential, is the part that explains a failure.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Served {
    Endpoint { url: String, authenticated: bool },
    Command { argc: usize, authenticated: bool },
    Builtin { name: String },
}

pub(crate) fn strip_externals(externals: &crate::config::Externals, tokens: &mut Tokens, mode: Mode) -> Externals {
    use crate::config::{AnnotatorImplementation, Implementation};

    let mut bindings = Vec::new();
    let mut push = |role: &'static str, name: &str, served: Served| {
        bindings.push(Binding {
            role,
            name: name.to_string(),
            served,
        });
    };
    let served = |implementation: &Implementation, tokens: &mut Tokens| match implementation {
        Implementation::Resolver(endpoint) => Served::Endpoint {
            url: tokens.token(mode, Class::Url, &endpoint.url),
            authenticated: endpoint.token.is_some(),
        },
        Implementation::Command(command) => Served::Command {
            argc: command.argv.len(),
            authenticated: command.token_env.is_some(),
        },
        Implementation::Builtin(name) => Served::Builtin { name: name.clone() },
    };

    for (role, entries) in [
        ("authority", &externals.authorities),
        ("sanitizer", &externals.sanitizers),
        ("audience_source", &externals.audience),
        ("identity", &externals.identity),
    ] {
        for (name, implementation) in entries {
            let name = tokens.token(mode, role_class(role), name);
            push(role, &name, served(implementation, tokens));
        }
    }
    for (name, implementation) in &externals.annotators {
        let name = tokens.token(mode, Class::Annotator, name);
        let shape = match implementation {
            AnnotatorImplementation::Resolver(endpoint) => Served::Endpoint {
                url: tokens.token(mode, Class::Url, &endpoint.url),
                authenticated: endpoint.token.is_some(),
            },
            AnnotatorImplementation::Command(command) => Served::Command {
                argc: command.argv.len(),
                authenticated: command.token_env.is_some(),
            },
        };
        push("annotator", &name, shape);
    }

    Externals {
        timeout_ms: externals.timeout.as_millis() as u64,
        review_timeout_ms: externals.review_timeout.as_millis() as u64,
        max_body_bytes: externals.max_body_bytes,
        bindings,
    }
}

fn role_class(role: &str) -> Class {
    match role {
        "authority" => Class::Authority,
        "sanitizer" => Class::Sanitizer,
        "audience_source" => Class::Source,
        _ => Class::Identity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stripped(text: &str, mode: Mode) -> Stripped {
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
        on = ["tool_result"]
        [sanitizer.permits.trust]
        from = "suspicious"
        to = "trusted"

        [audience]
        [audience.internal]
        from = ["directory:full-members"]
        [[audience.group]]
        name = "finance"
        from = ["directory:group/<group-address>"]

        [identity]
        implementation = "verified-email"

        [deployment]
        context_control = true
        confined_results = ["Bash"]
    "#;

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

    /// A hint is the deployer's own sentence and nothing bounds what is in it. Baseline
    /// carries it because the deployment wrote it about itself; pseudonymization does not.
    #[test]
    fn a_hint_is_carried_in_one_mode_only() {
        let baseline = stripped(FIXTURE, Mode::Baseline);
        assert!(baseline.value["authority"][0]["hint"].is_string());
        let pseudonymized = stripped(FIXTURE, Mode::Pseudonymized);
        assert!(pseudonymized.value["authority"][0].get("hint").is_none());
    }

    /// An inline binding is infrastructure, not rules: it holds an endpoint or an argv, and
    /// no mode carries either.
    #[test]
    fn an_inline_binding_is_carried_in_no_mode() {
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
            let stripped = stripped(text, mode);
            assert!(stripped.unclassified.is_empty(), "an inline binding is classified");
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
            [[tool]]
            name = "Send"
            [tool.requires.audience]
            contains = ["public", "internal", "@finance", "alice@corp.example", "$recipients"]
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
            assert_eq!(entries[0], "public", "the engine's own word, in {mode:?}");
            assert_eq!(entries[1], "internal");
            assert_ne!(entries[3], "alice@corp.example", "a written-out reader is a person");
            assert!(entries[4].starts_with('$'), "an argument placeholder stays one");
            let rendered = serde_json::to_string(&carried).expect("serializes");
            assert!(!rendered.contains("alice"), "the address survived {mode:?}: {rendered}");
        }
        // Baseline still spells the group the deployment named; pseudonymization does not.
        let baseline = stripped(text, Mode::Baseline);
        assert_eq!(
            baseline.value["tool"][0]["requires"]["audience"]["contains"][2],
            "@finance"
        );
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
        let stripped = stripped(text, Mode::Baseline);
        assert_eq!(stripped.value["surprise"], super::super::strip::UNCLASSIFIED);
        let rendered = serde_json::to_string(&stripped.value).expect("serializes");
        assert!(!rendered.contains("s3cret-value"));
    }
}
