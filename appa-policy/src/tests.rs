use std::collections::BTreeMap;

use appa_engine::audience::{AudienceConfig, DeclaredTemplate, NamedAudience, SelectorSpec, SourceRegistration};
use appa_engine::authority::{Attends, DeclaredTransition, Hint};
use appa_engine::contract::{AudienceRequirement, DeltaAudience, RecipientSpec, SelectorPlaceholder, ToolDeclaration};
use appa_engine::label::{Audience, ChainAudience, Clause, DeclaredAudience, GroupRef, ReaderId, Trust};
use appa_engine::names::{AnnotatorName, AuthorityName, GroupName, SanitizerName, TagName};
use appa_engine::profile::{BindingMode, ExecutorClass, neutral_starting_label};
use appa_engine::registry::{LoadError, MAX_HINT_CHARS};
use appa_engine::value::ToolName;

use crate::{AnnotatorBuiltin, Config, ConfigError, InputSource};

/// The sources the shipped batteries declare, as a deployment's bindings would supply them.
fn declared_sources() -> Vec<SourceRegistration> {
    let source = |provider: &str, templates: Vec<DeclaredTemplate>| SourceRegistration {
        provider: appa_engine::names::ProviderName::new(provider),
        templates,
    };
    vec![
        source(
            "google-workspace",
            vec![
                DeclaredTemplate::new("viewer", Some(ChainAudience::Self_)).expect("a well-formed template"),
                DeclaredTemplate::new("full-members", Some(ChainAudience::Internal)).expect("a well-formed template"),
                DeclaredTemplate::named("group/<group-address>").expect("a well-formed template"),
            ],
        ),
        source(
            "slack",
            vec![
                DeclaredTemplate::new("viewer", Some(ChainAudience::Self_)).expect("a well-formed template"),
                DeclaredTemplate::new("full-members", Some(ChainAudience::Internal)).expect("a well-formed template"),
                DeclaredTemplate::named("user-group/<handle>").expect("a well-formed template"),
                DeclaredTemplate::named("channel/<id>").expect("a well-formed template"),
            ],
        ),
        source(
            "github",
            vec![
                DeclaredTemplate::new("viewer", Some(ChainAudience::Self_)).expect("a well-formed template"),
                DeclaredTemplate::new("org/<org>/members", Some(ChainAudience::Internal))
                    .expect("a well-formed template"),
                DeclaredTemplate::named("org/<org>/team/<team>").expect("a well-formed template"),
            ],
        ),
    ]
}

/// A policy loaded under the shipped batteries' declared sources.
fn load(policy: &str) -> Result<Config, ConfigError> {
    Config::from_toml_str_routed(policy, BTreeMap::new(), declared_sources())
}

const DECLARATIONS: &str = r#"
version = 2

[[annotator]]
name = "crm-acl"
inputs = { customer_id = "$tool_call.arguments.customer_id" }
audiences = ["finance", "insider"]

[[tool]]
name = "lookup"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator = "crm-acl"

[[tool]]
name = "send"
parameters = { type = "object", properties = { customer_id = { type = "string" } }, required = ["customer_id"] }
annotator = "crm-acl"

[[authority]]
name = "approver"
[authority.permits]
trust_below = "trusted"

[[sanitizer]]
name = "pii"
on = ["tool_output"]
[sanitizer.permits]
audience = { from = ["insider"], to = ["public"] }

[deployment]
dispatch = "enforced"
confined_results = ["lookup"]
"#;

#[test]
fn declaration_only_policy_builds_the_engine_registry() {
    let config = load(DECLARATIONS).expect("the policy compiles");
    assert!(
        config
            .engine()
            .registry()
            .variants(&ToolName::new("lookup"))
            .next()
            .is_some()
    );
    assert!(
        config
            .engine()
            .registry()
            .variants(&ToolName::new("send"))
            .next()
            .is_some()
    );
    assert!(
        config
            .engine()
            .registry()
            .authority(&AuthorityName::new("approver"))
            .is_some()
    );
    assert!(
        config
            .engine()
            .registry()
            .sanitizer(&SanitizerName::new("pii"))
            .is_some()
    );
    assert_eq!(config.registry_config().tools.len(), 2);
}

#[test]
fn server_qualification_preserves_the_contract_and_deployment_references() {
    let qualified = DECLARATIONS.replace("name = \"lookup\"", "name = \"lookup\"\nserver = \"demo\"");
    let canonical = DECLARATIONS.replace("\"lookup\"", "\"mcp/demo/lookup\"");
    let qualified = load(&qualified).unwrap();
    let canonical = load(&canonical).unwrap();
    assert_eq!(qualified.engine().identity(), canonical.engine().identity());
    assert!(
        qualified
            .engine()
            .profile()
            .confines_result(&ToolName::new("mcp/demo/lookup"))
    );
    assert!(
        !qualified
            .engine()
            .profile()
            .confines_result(&ToolName::new("mcp/other/lookup"))
    );
}

#[test]
fn server_qualification_preserves_argument_selectors() {
    let native = "version = 2\n[[tool]]\nname = 'read(path:private*)'\nserver = 'demo'\ndescription = 'selected contract'\ntags = ['files']\n";
    let canonical = native
        .replace("read(path:private*)", "mcp/demo/read(path:private*)")
        .replace("server = 'demo'\n", "");
    assert_eq!(
        load(native).unwrap().engine().identity(),
        load(&canonical).unwrap().engine().identity()
    );
}

#[test]
fn server_qualification_rejects_ambiguous_or_unspellable_names() {
    for (name, server) in [
        ("mcp/other/read", "demo"),
        ("mcp__other__read", "demo"),
        ("read", ""),
        ("read", "bad/server"),
        ("*", "demo"),
    ] {
        let policy = format!("version = 2\n[[tool]]\nname = '{name}'\nserver = '{server}'\n");
        assert!(
            matches!(load(&policy), Err(ConfigError::ToolServer { .. })),
            "{name:?} / {server:?}"
        );
    }
}

#[test]
fn every_inline_implementation_site_is_refused() {
    let cases = [
        (
            "tool",
            "version = 2\n[[tool]]\nname = \"t\"\nimplementation = { http = { url = \"https://tool.invalid\" } }\n",
        ),
        (
            "authority",
            "version = 2\n[[authority]]\nname = \"a\"\nimplementation = { builtin = \"approve\" }\n[authority.permits]\ntrust_below = \"trusted\"\n",
        ),
        (
            "sanitizer",
            "version = 2\n[[sanitizer]]\nname = \"s\"\non = [\"tool_output\"]\nimplementation = { builtin = \"hosted\" }\n[sanitizer.permits]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n",
        ),
        (
            "annotator",
            "version = 2\n[[annotator]]\nname = \"d\"\nimplementation = { url = \"https://annotator.invalid\" }\n",
        ),
    ];
    for (kind, policy) in cases {
        assert!(
            matches!(
                load(policy),
                Err(ConfigError::ForbiddenInlineBinding { kind: found, .. }) if found == kind
            ),
            "{kind} inline binding was accepted"
        );
    }
}

#[test]
fn an_inline_binding_on_the_reserved_attest_schema_is_refused() {
    let policy = "version = 2\n\
         [[sanitizer]]\nname = \"attest-schema\"\non = [\"tool_output\"]\n\
         implementation = { url = \"https://attest.invalid\" }\n\
         [sanitizer.permits]\ntrust = { from = \"suspicious\", to = \"trusted\" }\n";
    assert!(matches!(
        load(policy),
        Err(ConfigError::ForbiddenInlineBinding { kind: "sanitizer", name }) if name == "attest-schema"
    ));
}

#[test]
fn an_input_sanitizer_registers_with_its_scope_and_refuses_a_trust_mandate() {
    let policy = |mandate: &str| {
        format!(
            "version = 2\n\
             [[tool]]\nname = \"post\"\ntags = [\"outbound\"]\ndelta = {{}}\n\
             [[sanitizer]]\nname = \"redact\"\non = [\"tool_input\"]\n\
             tags = [\"outbound\"]\n\
             [sanitizer.permits]\n{mandate}\n"
        )
    };
    let config = load(&policy("audience = { from = [\"insider\"], to = [\"partner\"] }"))
        .expect("an input substitution compiles");
    let sanitizer = config
        .engine()
        .registry()
        .sanitizer(&SanitizerName::new("redact"))
        .expect("the input point registers the sanitizer");
    assert!(sanitizer.on.input && !sanitizer.on.output);
    assert!(sanitizer.scope.covers(&[TagName::new("outbound")]));
    assert!(!sanitizer.scope.covers(&[TagName::new("inbound")]));

    assert!(matches!(
        load(&policy("trust = { from = \"suspicious\", to = \"trusted\" }")),
        Err(ConfigError::Registry(LoadError::InputSanitizerTrust(name))) if name == "redact"
    ));
    assert!(matches!(
        load(
            &policy("audience = { from = [\"insider\"], to = [\"partner\"] }")
                .replace("on = [\"tool_input\"]", "on = []")
        ),
        Err(ConfigError::NoSanitizerPoint { name }) if name == "redact"
    ));
}

#[test]
fn the_audience_tables_compile_into_the_registered_configuration() {
    let policy = "version = 2\n\
         [audience]\nself = [\"slack:viewer\"]\n\
         internal = [\"slack:full-members\", \"github:org/corp/members\"]\n\
         [audience.group.finance]\nwithin = \"internal\"\n\
         from = [\"google-workspace:group/finance@corp.com\"]\n";
    let config = load(policy).expect("the audience tables load");
    let audience = &config.registry_config().audience;
    let spec = |spelled: &str| SelectorSpec::parse(spelled).expect("a stock selector parses");
    assert_eq!(
        audience.sources.iter().map(|s| s.provider.as_str()).collect::<Vec<_>>(),
        ["google-workspace", "slack", "github"],
        "exactly the referenced providers register, in catalog order"
    );
    assert_eq!(audience.self_from, [spec("slack:viewer")]);
    assert_eq!(
        audience.internal_from,
        [spec("slack:full-members"), spec("github:org/corp/members")]
    );
    assert_eq!(
        audience.groups,
        [NamedAudience {
            name: GroupName::new("finance"),
            within: Some(ChainAudience::Internal),
            from: vec![spec("google-workspace:group/finance@corp.com")],
        }]
    );
    let bare = load("version = 2\n").unwrap();
    assert_eq!(bare.registry_config().audience, AudienceConfig::default());
}

#[test]
fn an_audience_source_selector_must_fit_its_level() {
    for (case, table, expected) in [
        (
            "self reads only viewers",
            "[audience]\nself = [\"slack:full-members\"]\n",
            "source",
        ),
        (
            "internal reads only full memberships",
            "[audience]\ninternal = [\"slack:viewer\"]\n",
            "source",
        ),
        (
            "a team is not a full membership",
            "[audience]\ninternal = [\"github:org/corp/team/x\"]\n",
            "source",
        ),
        (
            "a group never reads a viewer",
            "[audience.group.g]\nfrom = [\"slack:viewer\"]\n",
            "source",
        ),
        (
            "an uncatalogued collection",
            "[audience]\nself = [\"slack:banana\"]\n",
            "source",
        ),
        (
            "an unknown provider",
            "[audience]\nself = [\"msft:viewer\"]\n",
            "provider",
        ),
        (
            "a bare word is no selector",
            "[audience]\nself = [\"banana\"]\n",
            "source",
        ),
        (
            "a group name never carries the @ mark",
            "[audience.group.\"@g\"]\nfrom = [\"slack:user-group/g\"]\n",
            "group",
        ),
        (
            "a reserved spelling is no group name",
            "[audience.group.internal]\nfrom = [\"slack:user-group/g\"]\n",
            "group",
        ),
        (
            "within targets the built-in chain only",
            "[audience.group.g]\nwithin = \"public\"\nfrom = [\"slack:user-group/g\"]\n",
            "group",
        ),
    ] {
        let refusal = load(&format!("version = 2\n{table}")).expect_err(case);
        let fits = match expected {
            "source" => matches!(refusal, ConfigError::BadAudienceSource { .. }),
            "provider" => matches!(refusal, ConfigError::UndeclaredProvider { .. }),
            _ => matches!(refusal, ConfigError::BadNamedAudience { .. }),
        };
        assert!(fits, "{case}: got {refusal:?}");
    }
}

#[test]
fn an_annotator_carries_the_stock_builtin_on_its_declaration() {
    let policy = |builtin: &str| {
        format!(
            "version = 2\n[[annotator]]\nname = \"classify\"\nbuiltin = \"{builtin}\"\n\
             [[annotator]]\nname = \"bound\"\n"
        )
    };
    for expected in AnnotatorBuiltin::ALL {
        let config = load(&policy(expected.wire_name())).expect("the stock builtin loads");
        let annotators: Vec<_> = config
            .annotators()
            .map(|(name, binding)| (name.as_str(), binding.builtin))
            .collect();
        assert_eq!(
            annotators,
            vec![("bound", None), ("classify", Some(expected))],
            "a bound annotator still registers"
        );
    }

    assert!(matches!(
        load(&policy("no-such")),
        Err(ConfigError::UnknownAnnotatorBuiltin { name, builtin }) if name == "classify" && builtin == "no-such"
    ));
}

#[test]
fn a_tool_requires_a_registered_annotator() {
    assert!(matches!(
        load("version = 2\n[[tool]]\nname = \"lookup\"\nannotator = \"classifier\"\n"),
        Err(ConfigError::Registry(LoadError::UnknownAnnotator { tool, annotator }))
            if tool == "lookup" && annotator == "classifier"
    ));
}

#[test]
fn a_duplicate_annotator_is_refused() {
    assert!(matches!(
        load("version = 2\n[[annotator]]\nname = \"a\"\n[[annotator]]\nname = \"a\"\n"),
        Err(ConfigError::Registry(LoadError::DuplicateAnnotator(name))) if name == "a"
    ));
}

#[test]
fn a_tool_names_an_annotator_or_declares_static_semantics_never_both() {
    let with = |statics: &str| {
        format!(
            "version = 2\n[[annotator]]\nname = \"acl\"\n\
             [[tool]]\nname = \"send\"\nannotator = \"acl\"\n{statics}\n"
        )
    };
    assert!(load(&with("")).is_ok());
    // Metadata is not a recipe: it stays legal beside `annotator`.
    assert!(load(&with("description = \"Sends one message.\"\ntags = [\"outbound\"]")).is_ok());
    for (field, statics) in [
        ("delta", "delta = {}"),
        ("requires", "requires = { trust = \"trusted\" }"),
        ("effects", "effects = [\"email.sent\"]"),
    ] {
        assert!(
            matches!(
                load(&with(statics)),
                Err(ConfigError::AnnotatorWithStatics { tool, annotator, field: found })
                    if tool == "send" && annotator == "acl" && found == field
            ),
            "`{field}` beside `annotator` must be refused"
        );
    }
}

#[test]
fn the_wildcard_tool_loads_with_an_annotator_and_nothing_else() {
    let policy = "version = 2\n[[annotator]]\nname = \"any\"\n\
                  [[tool]]\nname = \"*\"\nannotator = \"any\"\n";
    let config = load(policy).expect("the wildcard loads");
    assert_eq!(
        config
            .engine()
            .registry()
            .classify(&appa_engine::value::ToolName::new("ghost")),
        Some(appa_engine::registry::ToolKind::Wildcard)
    );
    assert_ne!(
        config
            .engine()
            .registry()
            .classify(&appa_engine::value::ToolName::new("*")),
        Some(appa_engine::registry::ToolKind::Declared),
        "the wildcard's spelling names no tool"
    );
    assert_eq!(
        config.engine().registry().tools().count(),
        0,
        "the wildcard is in no listing"
    );
}

#[test]
fn a_second_wildcard_tool_is_refused() {
    let policy = "version = 2\n[[annotator]]\nname = \"any\"\n\
                  [[tool]]\nname = \"*\"\nannotator = \"any\"\n\
                  [[tool]]\nname = \"*\"\nannotator = \"any\"\n";
    assert!(matches!(
        load(policy),
        Err(ConfigError::Registry(LoadError::DuplicateWildcard))
    ));
}

#[test]
fn a_wildcard_tool_with_static_semantics_is_refused() {
    for statics in ["", "delta = {}"] {
        let policy = format!("version = 2\n[[tool]]\nname = \"*\"\n{statics}\n");
        assert!(
            matches!(load(&policy), Err(ConfigError::Registry(LoadError::WildcardStatic))),
            "a wildcard without an annotator (statics: {statics:?}) must be refused"
        );
    }
}

#[test]
fn a_wildcard_tool_with_metadata_is_refused() {
    for metadata in [
        "description = \"Anything.\"",
        "tags = [\"web\"]",
        "parameters = { type = \"object\", properties = { path = { type = \"string\" } } }",
    ] {
        let policy = format!(
            "version = 2\n[[annotator]]\nname = \"any\"\n\
             [[tool]]\nname = \"*\"\nannotator = \"any\"\n{metadata}\n"
        );
        assert!(
            matches!(load(&policy), Err(ConfigError::Registry(LoadError::WildcardMetadata))),
            "wildcard metadata {metadata:?} must be refused"
        );
    }
    let selected = "version = 2\n[[annotator]]\nname = \"any\"\n\
                    [[tool]]\nname = \"*(path:*)\"\nannotator = \"any\"\n";
    assert!(
        matches!(load(selected), Err(ConfigError::Registry(LoadError::WildcardMetadata))),
        "a wildcard with an argument selector must be refused"
    );
}

#[test]
fn a_catch_all_attention_permit_loads_and_the_wildcard_never_mixes_with_names() {
    let catch_all = "version = 2\n[[authority]]\nname = \"anyone\"\n[authority.permits]\nattention = [\"*\"]\n";
    let config = load(catch_all).expect("a catch-all permit loads");
    let anyone = config
        .engine()
        .registry()
        .authority(&AuthorityName::new("anyone"))
        .expect("the catch-all registers");
    assert_eq!(anyone.mandate.attends, Attends::Any);

    let mixed =
        "version = 2\n[[authority]]\nname = \"anyone\"\n[authority.permits]\nattention = [\"*\", \"signoff\"]\n";
    assert!(matches!(
        load(mixed),
        Err(ConfigError::MixedAttentionWildcard { context }) if context == "authority anyone"
    ));

    let reserved = "version = 2\n[[authority]]\nname = \"anyone\"\n[authority.permits]\nattention = [\"blocked\"]\n";
    assert!(matches!(
        load(reserved),
        Err(ConfigError::Registry(LoadError::ReservedMark(name))) if name == "anyone"
    ));
}

#[test]
fn an_annotator_mandate_resolves_omitted_bounds_to_the_whole_vocabulary() {
    let policy = r#"
version = 2

[[annotator]]
name = "bounded"
ranks = ["suspicious"]
audiences = ["alice"]
marks = ["operator-signoff"]
effects = ["email.sent"]

[[annotator]]
name = "open"

[[tool]]
name = "post"
delta = { audience = ["alice", "bob"] }
effects = ["email.sent", "backup.completed"]

[[tool]]
name = "fetch"
annotator = "open"

[[tool]]
name = "classify"
annotator = "bounded"

[[authority]]
name = "reviewer"
[authority.permits]
attention = ["operator-signoff", "legal-review"]
"#;
    let config = load(policy).expect("annotator bounds load");
    let registry = config.engine().registry();

    let open = registry
        .annotator_mandate(&AnnotatorName::new("open"))
        .expect("open registers");
    assert_eq!(
        open.trust_ranks().collect::<Vec<_>>(),
        vec![Trust::new(0), Trust::new(1)]
    );
    assert_eq!(
        open.audiences().entries().collect::<Vec<_>>(),
        ["self", "internal", "alice", "bob"]
    );
    assert_eq!(
        open.marks().map(|mark| mark.as_str()).collect::<Vec<_>>(),
        ["legal-review", "operator-signoff"]
    );
    assert_eq!(
        open.effects().map(|kind| kind.as_str()).collect::<Vec<_>>(),
        ["backup.completed", "email.sent"]
    );

    let bounded = registry
        .annotator_mandate(&AnnotatorName::new("bounded"))
        .expect("bounded registers");
    assert_eq!(bounded.trust_ranks().collect::<Vec<_>>(), vec![Trust::new(0)]);
    assert_eq!(bounded.audiences().entries().collect::<Vec<_>>(), ["alice"]);
    assert_eq!(
        bounded.marks().map(|m| m.as_str()).collect::<Vec<_>>(),
        ["operator-signoff"]
    );
    assert_eq!(
        bounded.effects().map(|k| k.as_str()).collect::<Vec<_>>(),
        ["email.sent"]
    );

    assert!(matches!(
        load("version = 2\n[[annotator]]\nname = \"a\"\nranks = [\"nope\"]\n"),
        Err(ConfigError::UnknownTrustRank { .. })
    ));
}

#[test]
fn an_annotator_hint_is_runtime_owned_and_bounded() {
    let source = "version = 2\n[[annotator]]\nname = \"classifier\"\nhint = \"Suspicious means unvetted data.\"\n";
    let config = load(source).expect("the Annotator hint loads");
    let (_, binding) = config.annotators().next().expect("the Annotator is registered");
    assert_eq!(
        binding.hint.as_ref().map(Hint::as_str),
        Some("Suspicious means unvetted data.")
    );

    let without_hint =
        load("version = 2\n[[annotator]]\nname = \"classifier\"\n").expect("the unhinted Annotator loads");
    assert_eq!(
        config.engine().identity(),
        without_hint.engine().identity(),
        "advisory hints do not change policy semantics"
    );

    let overlong = "x".repeat(MAX_HINT_CHARS + 1);
    let refused = format!("version = 2\n[[annotator]]\nname = \"classifier\"\nhint = \"{overlong}\"\n");
    assert!(matches!(
        load(&refused),
        Err(ConfigError::Registry(LoadError::HintTooLong { context, len, max }))
            if context == "annotator classifier" && len == MAX_HINT_CHARS + 1 && max == MAX_HINT_CHARS
    ));
}

#[test]
fn an_empty_annotator_audience_mandate_closes_it_to_public_answers() {
    let policy = r#"
version = 2

[[annotator]]
name = "acl"
audiences = []

[[tool]]
name = "post"
delta = { audience = ["alice"] }

[[tool]]
name = "fetch"
annotator = "acl"
"#;
    let config = load(policy).expect("the public-only mandate loads");
    let mandate = config
        .engine()
        .registry()
        .annotator_mandate(&AnnotatorName::new("acl"))
        .expect("acl registers");
    assert!(mandate.audiences().is_empty(), "no named audience is admissible");
}

#[test]
fn a_wildcard_covers_a_confined_result_the_policy_never_names() {
    let policy = r#"
version = 2

[deployment]
dispatch = "enforced"
confined_results = ["unwritten"]

[[annotator]]
name = "acl"

[[tool]]
name = "*"
annotator = "acl"
"#;
    load(policy).expect("the wildcard covers the confined tool");
}

#[test]
fn an_annotator_audience_bound_lists_symbolic_audiences_and_readers() {
    let with = |audiences: &str| {
        format!(
            "version = 2\n[audience.group.team]\nfrom = [\"slack:user-group/team\"]\n[[annotator]]\nname = \"acl\"\naudiences = {audiences}\n"
        )
    };
    let config = load(&with(
        "[\"alice\", \"@team\", \"internal\", \"self\", \"@slack:user-group/eng\", \"bob\"]",
    ))
    .expect("a symbolic bound loads");
    assert_eq!(
        config
            .engine()
            .registry()
            .annotator_mandate(&AnnotatorName::new("acl"))
            .expect("acl registers")
            .audiences()
            .entries()
            .collect::<Vec<_>>(),
        ["self", "internal", "@team", "@slack:user-group/eng", "alice", "bob"]
    );
    for (case, audiences) in [
        ("`public`", "[\"public\"]"),
        ("a placeholder", "[\"$to\"]"),
        ("an empty spelling", "[\"\"]"),
        ("a repeated entry", "[\"@team\", \"@team\"]"),
    ] {
        assert!(
            matches!(load(&with(audiences)), Err(ConfigError::BadAudience { .. })),
            "{case} must be refused in an annotator's `audiences`"
        );
    }
    assert!(
        load(&with("[\"@nobody\"]")).is_err(),
        "a group no configuration serves does not route"
    );
}

#[test]
fn a_written_audience_list_refuses_a_repeated_entry() {
    let with =
        |audience: &str| format!("version = 2\n[[tool]]\nname = \"post\"\ndelta = {{ audience = {audience} }}\n");
    assert!(load(&with("[\"alice\", \"bob\"]")).is_ok());
    for repeated in ["[\"alice\", \"alice\"]", "[\"a@CORP.example\", \"a@corp.example\"]"] {
        assert!(
            matches!(load(&with(repeated)), Err(ConfigError::BadAudience { .. })),
            "{repeated} names one reader twice"
        );
    }
}

#[test]
fn an_input_reads_a_tool_call_value_or_names_a_program() {
    let policy = |spelling: &str| {
        format!(
            "version = 2\n[[annotator]]\nname = \"r\"\ninputs = {{ subject = \"{spelling}\" }}\n\
             [[tool]]\nname = \"lookup\"\ndescription = \"d\"\n\
             parameters = {{ type = \"object\", properties = {{ id = {{ type = \"string\" }} }}, required = [\"id\"] }}\n\
             annotator = \"r\"\n"
        )
    };
    for supported in [
        "$tool_call",
        "$tool_call.name",
        "$tool_call.description",
        "$tool_call.arguments",
        "$tool_call.arguments.id",
        "$input.github.repository",
    ] {
        assert!(load(&policy(supported)).is_ok(), "{supported} is an input source");
    }
    // A program input is checked against no tool schema: the tool declares nothing
    // about a value the deployment establishes.
    let loaded = load(&policy("$input.repo")).expect("a program input loads");
    assert_eq!(
        loaded
            .annotators()
            .next()
            .map(|(_, binding)| binding.inputs["subject"].clone()),
        Some(InputSource::External("repo".to_string()))
    );
    for unsupported in [
        "$tool",
        "$tool_call.foo",
        "$tool_call.arguments.id.deep",
        "$tool_call.arguments.",
        "$input.",
        "$input",
        "id",
    ] {
        assert!(
            matches!(
                load(&policy(unsupported)),
                Err(ConfigError::UnknownCallSource { annotator, input, spelling })
                    if annotator == "r" && input == "subject" && spelling == unsupported
            ),
            "{unsupported} is not a tool-call value"
        );
    }
}

#[test]
fn a_mapped_argument_input_needs_a_required_top_level_property() {
    let policy = |parameters: &str| {
        format!(
            "version = 2\n[[annotator]]\nname = \"acl\"\ninputs = {{ subject = \"$tool_call.arguments.id\" }}\n\
             [[tool]]\nname = \"lookup\"\nannotator = \"acl\"\n{parameters}\n"
        )
    };
    assert!(
        load(&policy(
            "parameters = { type = \"object\", properties = { id = { type = \"string\" } }, required = [\"id\"] }"
        ))
        .is_ok()
    );
    for (case, parameters) in [
        ("an open schema", ""),
        (
            "an optional property",
            "parameters = { type = \"object\", properties = { id = { type = \"string\" } } }",
        ),
        (
            "another property",
            "parameters = { type = \"object\", properties = { cc = { type = \"string\" } }, required = [\"cc\"] }",
        ),
    ] {
        assert!(
            matches!(
                load(&policy(parameters)),
                Err(ConfigError::AnnotatorInput { tool, annotator, input, .. })
                    if tool == "lookup" && annotator == "acl" && input == "subject"
            ),
            "{case} must refuse the argument input"
        );
    }
}

#[test]
fn a_description_input_needs_a_declared_description() {
    let policy = |description: &str| {
        format!(
            "version = 2\n[[annotator]]\nname = \"acl\"\ninputs = {{ what = \"$tool_call.description\" }}\n\
             [[tool]]\nname = \"lookup\"\nannotator = \"acl\"\n{description}\n"
        )
    };
    assert!(load(&policy("description = \"Looks one customer up.\"")).is_ok());
    assert!(matches!(
        load(&policy("")),
        Err(ConfigError::AnnotatorInput { tool, input, .. }) if tool == "lookup" && input == "what"
    ));
}

#[test]
fn an_annotator_name_is_an_opaque_non_empty_string() {
    let policy = "version = 2\n[[annotator]]\nname = \"a.b\"\n\
        [[tool]]\nname = \"lookup\"\ndescription = \"Looks up a value.\"\nannotator = \"a.b\"\n";
    assert!(load(policy).is_ok());
    assert!(matches!(
        load("version = 2\n[[annotator]]\nname = \"\"\n"),
        Err(ConfigError::BadAnnotatorName(name)) if name.is_empty()
    ));
}

#[test]
fn an_audience_argument_binding_implies_a_required_string_in_parameters() {
    use appa_engine::params::PropertyFault;
    let policy = |parameters: &str| {
        format!(
            "version = 2\n[[tool]]\nname = \"send\"\n{parameters}\nrequires = {{ audience = {{ contains = [\"$to\"] }} }}\ndelta = {{}}\n"
        )
    };
    for parameters in [
        "",
        "parameters = { type = \"object\", properties = { cc = { type = \"string\" } }, required = [\"cc\"] }",
        "parameters = { type = \"object\", properties = { envelope = { type = \"object\", properties = { to = { type = \"string\" } }, required = [\"to\"] } }, required = [\"envelope\"] }",
        "parameters = { type = \"object\", properties = { to = { type = \"string\" } } }",
        "parameters = { type = \"object\", properties = { to = { type = \"string\", enum = [\"ops\"] }, body = { type = \"string\" } }, required = [\"to\"] }",
    ] {
        let config = load(&policy(parameters)).unwrap_or_else(|error| panic!("must load: {error}\n{parameters}"));
        let schema = config
            .engine()
            .registry()
            .variants(&ToolName::new("send"))
            .next()
            .and_then(ToolDeclaration::declared)
            .expect("send is declared")
            .parameters
            .normalized();
        assert_eq!(schema["properties"]["to"]["type"], "string", "{parameters}");
        assert!(
            schema["required"]
                .as_array()
                .is_some_and(|required| required.contains(&serde_json::json!("to"))),
            "{parameters}"
        );
    }
    let integer =
        policy("parameters = { type = \"object\", properties = { to = { type = \"integer\" } }, required = [\"to\"] }");
    assert!(matches!(
        load(&integer),
        Err(ConfigError::Registry(LoadError::AudienceBindingSchema { argument, fault, .. }))
            if argument == "to" && fault == PropertyFault::NotString
    ));
    let static_recipients =
        "version = 2\n[[tool]]\nname = \"send\"\nrequires = { audience = { contains = [\"finance\"] } }\ndelta = {}\n";
    assert!(load(static_recipients).is_ok());
}

#[test]
fn the_deployment_table_compiles_into_the_validated_profile() {
    let config = load(DECLARATIONS).expect("the policy compiles");
    let profile = config.engine().profile();
    assert_eq!(
        profile.executor_class(&ToolName::new("lookup")),
        ExecutorClass::Enforced
    );
    assert!(profile.confines_result(&ToolName::new("lookup")));
    assert!(!profile.confines_result(&ToolName::new("send")));
    assert_eq!(profile.binding(), BindingMode::Harness);
    assert!(!profile.context_control());
    assert_eq!(
        profile.starting_label(),
        &neutral_starting_label(config.engine().registry().trust_chain())
    );
}

#[test]
fn an_absent_deployment_table_is_the_no_coverage_default_and_refuses_covered_constructs() {
    let plain = load("version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n").expect("loads");
    assert_eq!(
        plain.engine().profile().executor_class(&ToolName::new("t")),
        ExecutorClass::Assumed
    );
    assert_eq!(plain.engine().open_vectors().len(), 1);
    let uncovered = DECLARATIONS.replace(
        "[deployment]\ndispatch = \"enforced\"\nconfined_results = [\"lookup\"]\n",
        "",
    );
    assert!(matches!(
        load(&uncovered),
        Err(ConfigError::Registry(LoadError::OutputSanitizerUncovered { .. }))
    ));
}

#[test]
fn deployment_tokens_and_exception_conflicts_are_load_errors() {
    let base = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
    let with = |table: &str| format!("{base}\n[deployment]\n{table}\n");
    for bad_token in [
        "dispatch = \"trusted\"",
        "binding = \"content\"",
        "provider_surfaces = { web_search = \"proxied\" }",
    ] {
        assert!(matches!(load(&with(bad_token)), Err(ConfigError::Parse(_))));
    }
    assert!(matches!(
        load(&with("starting_label = { audience = \"everyone\" }")),
        Err(ConfigError::BadDeploymentToken {
            field: "starting_label audience",
            ..
        })
    ));
    assert!(matches!(
        load(&with("assumed_tools = [\"t\"]\nprovider_run_tools = [\"t\"]")),
        Err(ConfigError::ConflictingExecutorException { tool }) if tool == "t"
    ));
    assert!(matches!(
        load(&with("confined_results = [\"ghost\"]")),
        Err(ConfigError::Registry(LoadError::UnknownDeploymentTool { .. }))
    ));
}

#[test]
fn the_wildcard_is_part_of_the_policy_identity() {
    let identity = |source: &str| load(source).expect("loads").engine().identity();
    let base = "version = 2\n\n[[annotator]]\nname = \"acl\"\n\n[[annotator]]\nname = \"other\"\n\n[[tool]]\nname = \"post\"\ndelta = {}\n";
    let with_wildcard = format!("{base}\n[[tool]]\nname = \"*\"\nannotator = \"acl\"\n");
    let with_other = format!("{base}\n[[tool]]\nname = \"*\"\nannotator = \"other\"\n");
    assert_ne!(
        identity(base),
        identity(&with_wildcard),
        "adding the wildcard changes what an unwritten tool call does"
    );
    assert_ne!(
        identity(&with_wildcard),
        identity(&with_other),
        "rerouting the wildcard changes who annotates the long tail"
    );
}

#[test]
fn hints_and_limits_never_move_the_policy_identity() {
    let identity = |source: &str| load(source).expect("loads").engine().identity();
    let base = identity(DECLARATIONS);
    let hinted = DECLARATIONS.replace(
        "name = \"approver\"",
        "name = \"approver\"\nhint = \"the wire-approval desk\"",
    );
    assert_eq!(identity(&hinted), base);
    let capped = format!("{DECLARATIONS}\n[limits]\nplanner_cap = 7\n");
    assert_eq!(identity(&capped), base);
    let weakened = DECLARATIONS.replace("dispatch = \"enforced\"", "dispatch = \"assumed\"");
    assert_ne!(identity(&weakened), base);
}

#[test]
fn compiled_tool_parameters_are_normalized_in_policy_identity() {
    let config = load(
        "version = 2\n[[tool]]\nname = \"t\"\nparameters = { type = \"object\", properties = { value = { type = \"string\" } } }\n",
    )
    .expect("the schema compiles");
    let tool = config
        .engine()
        .registry()
        .variants(&ToolName::new("t"))
        .next()
        .expect("the tool is registered");
    assert_eq!(
        tool.parameters().normalized(),
        serde_json::json!({
            "type": "object",
            "properties": { "value": { "type": "string" } },
            "required": [],
            "additionalProperties": false
        })
    );
}

#[test]
fn a_declared_group_mention_loads_when_routed_and_refuses_at_load_when_not() {
    let policy = r#"
version = 2

[audience]
internal = ["slack:full-members"]

[audience.group.team]
within = "internal"
from = ["slack:user-group/team"]

[audience.group.board]
from = ["slack:user-group/board"]

[audience.group.officers]
from = ["slack:user-group/officers"]

[[tool]]
name = "read"
delta = { audience = ["auditor", "@team"] }

[[tool]]
name = "send"
requires = { audience = { within = ["@team"], contains = ["@board"] } }
delta = {}

[[authority]]
name = "officer"
[authority.permits]
audience_missing = ["@officers"]

[[sanitizer]]
name = "declassify"
on = ["tool_output"]
[sanitizer.permits]
audience = { from = ["internal"], to = ["@team"] }

[deployment]
dispatch = "enforced"
confined_results = ["read", "send"]
"#;
    let mention = |name: &str, readers: &[&str]| {
        DeclaredAudience::Union(
            Clause::new(
                [],
                [GroupRef::Named(GroupName::new(name))],
                readers.iter().map(|reader| ReaderId::new(*reader)),
            )
            .expect("the fixture readers are literal"),
        )
    };
    let config = load(policy).expect("routed group mentions load");
    let registry = config.engine().registry();
    assert_eq!(
        registry
            .audience()
            .groups()
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>(),
        ["board", "officers", "team"]
    );
    let read = registry
        .variants(&ToolName::new("read"))
        .next()
        .expect("read registers")
        .declared()
        .expect("read is declared");
    assert_eq!(
        read.delta.audience.as_ref(),
        Some(&DeltaAudience::Static(mention("team", &["auditor"])))
    );
    let officer = registry
        .authority(&AuthorityName::new("officer"))
        .expect("officer registers");
    assert_eq!(officer.mandate.reader_ceiling, Some(mention("officers", &[])));
    match &registry
        .sanitizer(&SanitizerName::new("declassify"))
        .expect("declassify registers")
        .transition
    {
        DeclaredTransition::Audience { from_includes, to } => {
            assert_eq!(
                from_includes,
                &DeclaredAudience::Union(Clause::new([ChainAudience::Internal], [], []).expect("a chain clause"))
            );
            assert_eq!(to, &mention("team", &[]));
        }
        other => panic!("expected an audience transition, got {other:?}"),
    }

    let unrouted = policy.replace("[audience.group.board]\nfrom = [\"slack:user-group/board\"]\n", "");
    assert!(matches!(
        load(&unrouted),
        Err(ConfigError::Registry(LoadError::UnroutableAudience { .. }))
    ));

    for (case, replacement) in [
        ("bare @", "within = [\"@\"]"),
        ("public beside a group", "within = [\"public\", \"@team\"]"),
    ] {
        let malformed = policy.replace("within = [\"@team\"]", replacement);
        assert!(
            matches!(load(&malformed), Err(ConfigError::BadAudience { .. })),
            "{case} loads"
        );
    }

    let routed = "version = 2\n\
         [audience.group.team]\nfrom = [\"slack:user-group/team\"]\n\
         [[tool]]\nname = \"t\"\ndelta = {}\n";
    let bare = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
    let symbolic = load(&format!(
        "{routed}[deployment]\nstarting_label = {{ audience = [\"@team\"] }}\n"
    ))
    .expect("a routed symbolic starting label loads");
    assert_eq!(
        symbolic.engine().profile().starting_label().audience,
        Audience::of_declared(&mention("team", &[]))
    );
    load(&format!("{routed}[boundary]\naudience = [\"@team\"]\n")).expect("a routed symbolic boundary label loads");
    assert!(matches!(
        load(&format!(
            "{bare}[deployment]\nstarting_label = {{ audience = [\"@team\"] }}\n"
        )),
        Err(ConfigError::Registry(LoadError::UnroutableAudience { .. }))
    ));
    assert!(matches!(
        load(&format!("{bare}[boundary]\naudience = [\"@team\"]\n")),
        Err(ConfigError::BadAudience { .. })
    ));
}
#[test]
fn a_string_token_and_a_reader_list_are_different_audience_shapes() {
    let base = "version = 2\n[[tool]]\nname = \"t\"\ndelta = {}\n";
    let starting = |audience: &str| {
        let policy = format!("{base}[deployment]\nstarting_label = {{ audience = {audience} }}\n");
        load(&policy)
            .expect("a public starting label loads")
            .engine()
            .profile()
            .starting_label()
            .clone()
    };
    assert_eq!(starting("\"public\""), starting("[\"public\"]"));

    let delta = |audience: &str| {
        let policy = format!(
            "version = 2\n[[tool]]\nname = \"t\"\ndelta = {{ audience = {audience} }}\n\
             [deployment]\ndispatch = \"enforced\"\nconfined_results = [\"t\"]\n"
        );
        load(&policy)
    };
    assert!(
        delta("\"public\"").is_err(),
        "a tool delta audience is a reader list, never a token"
    );
    assert!(matches!(delta("[]"), Err(ConfigError::BadAudience { .. })));
}

#[test]
fn a_sanitizer_transition_and_a_component_tag_list_keep_every_member() {
    let policy = "version = 2\n\
         [audience.group.auditors]\nfrom = [\"slack:user-group/auditors\"]\n\
         [audience.group.reviewers]\nfrom = [\"slack:user-group/reviewers\"]\n\
         [[tool]]\nname = \"read\"\ntags = [\"hr\", \"crm\"]\ndelta = { audience = [\"alice\", \"bob\"] }\n\
         [[sanitizer]]\nname = \"redact\"\non = [\"tool_output\"]\ntags = [\"hr\", \"crm\"]\n\
         [sanitizer.permits]\naudience = { from = [\"alice\", \"@auditors\"], to = [\"alice\", \"@reviewers\"] }\n\
         [[authority]]\nname = \"officer\"\ntags = [\"hr\", \"crm\"]\n\
         [authority.permits]\naudience_missing = [\"alice\", \"bob\"]\n\
         [deployment]\ndispatch = \"enforced\"\nconfined_results = [\"read\"]\n";
    let config = load(policy).expect("lists of more than one member load");
    let registry = config.engine().registry();
    let redact = registry
        .sanitizer(&SanitizerName::new("redact"))
        .expect("redact registers");
    assert_eq!(redact.scope.tags.len(), 2);
    match &redact.transition {
        DeclaredTransition::Audience { from_includes, to } => {
            let expected = |group: &str| {
                DeclaredAudience::Union(
                    Clause::new([], [GroupRef::Named(GroupName::new(group))], [ReaderId::new("alice")]).unwrap(),
                )
            };
            assert_eq!(from_includes, &expected("auditors"));
            assert_eq!(to, &expected("reviewers"));
        }
        other => panic!("expected an audience transition, got {other:?}"),
    }
    let officer = registry
        .authority(&AuthorityName::new("officer"))
        .expect("officer registers");
    assert_eq!(officer.scope.tags.len(), 2);
    assert_eq!(
        officer.mandate.reader_ceiling,
        Some(DeclaredAudience::restricted([
            ReaderId::new("alice"),
            ReaderId::new("bob")
        ]))
    );
}

/// A selector placeholder is a contract over the call: it stands alone in its list, each
/// argument it reads is a required string of the tool's schema, and its spelling fits a
/// template some declared provider serves. Naming the provider is what registers it.
#[test]
fn a_selector_placeholder_reads_one_declared_collection_per_call() {
    let channel =
        r#"{ type = "object", properties = { channel_id = { type = "string" } }, required = ["channel_id"] }"#;
    let tool = |parameters: &str, contract: &str| {
        format!("version = 2\n[[tool]]\nname = \"read_channel\"\nparameters = {parameters}\n{contract}\n")
    };
    let delta = "delta = { audience = [\"@slack:channel/$channel_id\"] }";
    let placeholder = SelectorPlaceholder::parse("slack:channel/$channel_id").expect("a placeholder spelling");

    let config = load(&tool(channel, delta)).expect("a placeholder delta loads");
    let read = config
        .engine()
        .registry()
        .variants(&ToolName::new("read_channel"))
        .next()
        .and_then(ToolDeclaration::declared)
        .expect("read_channel is declared");
    assert_eq!(read.delta.audience, Some(DeltaAudience::Selector(placeholder.clone())));
    assert_eq!(
        config
            .engine()
            .registry()
            .audience()
            .providers()
            .iter()
            .collect::<Vec<_>>(),
        ["slack"],
        "a placeholder names its provider into the policy"
    );
    let floor = "requires = { audience = { contains = [\"@slack:channel/$channel_id\"] } }";
    let send = load(&tool(channel, floor)).expect("a placeholder floor loads");
    let send = send
        .engine()
        .registry()
        .variants(&ToolName::new("read_channel"))
        .next()
        .and_then(ToolDeclaration::declared)
        .expect("read_channel is declared");
    assert_eq!(
        send.requires.audience_requirements(),
        [AudienceRequirement::Includes(RecipientSpec::Selector(placeholder))]
    );

    let integer =
        r#"{ type = "object", properties = { channel_id = { type = "integer" } }, required = ["channel_id"] }"#;
    for (case, policy, expected) in [
        (
            "beside another entry",
            tool(
                channel,
                "delta = { audience = [\"@slack:channel/$channel_id\", \"alice\"] }",
            ),
            "spelling",
        ),
        (
            "beside another entry in a floor",
            tool(
                channel,
                "requires = { audience = { contains = [\"alice\", \"@slack:channel/$channel_id\"] } }",
            ),
            "spelling",
        ),
        (
            "under within",
            tool(
                channel,
                "requires = { audience = { within = [\"@slack:channel/$channel_id\"] } }",
            ),
            "spelling",
        ),
        (
            "an argument the schema declares as another type",
            tool(integer, delta),
            "schema",
        ),
        (
            "a template the provider does not declare",
            tool(channel, "delta = { audience = [\"@slack:room/$channel_id\"] }"),
            "selector",
        ),
        (
            "a variable where the template is literal",
            tool(channel, "delta = { audience = [\"@slack:$channel_id\"] }"),
            "selector",
        ),
        (
            "a provider no source declares",
            tool(channel, "delta = { audience = [\"@msft:channel/$channel_id\"] }"),
            "provider",
        ),
        (
            "a provider-run tool",
            tool(channel, delta) + "[deployment]\nprovider_run_tools = [\"read_channel\"]\n",
            "provider-run",
        ),
    ] {
        let refusal = load(&policy).expect_err(case);
        let fits = match expected {
            "spelling" => matches!(refusal, ConfigError::BadAudience { .. }),
            "schema" => matches!(
                &refusal,
                ConfigError::Registry(LoadError::AudienceBindingSchema { argument, .. }) if argument == "channel_id"
            ),
            "selector" => matches!(
                refusal,
                ConfigError::Registry(LoadError::UnroutableAudience {
                    fault: appa_engine::audience::Unroutable::UnknownSelector { .. },
                    ..
                })
            ),
            "provider" => matches!(
                refusal,
                ConfigError::Registry(LoadError::UnroutableAudience {
                    fault: appa_engine::audience::Unroutable::UnknownProvider(_),
                    ..
                })
            ),
            _ => matches!(refusal, ConfigError::Registry(LoadError::ProviderRunPlaceholder { .. })),
        };
        assert!(fits, "{case}: got {refusal:?}");
    }
}

/// A mandate placeholder is read per call: it admits exactly the collection the routed
/// call's arguments spell, so every routed tool carries the argument as a required string, and the wildcard —
/// whose calls the policy does not describe — cannot route through it.
#[test]
fn an_annotator_mandate_placeholder_binds_to_each_routed_tools_arguments() {
    let policy = |tool: &str| {
        format!("version = 2\n[[annotator]]\nname = \"acl\"\naudiences = [\"@slack:channel/$channel_id\"]\n{tool}")
    };
    let routed = "[[tool]]\nname = \"read_channel\"\nparameters = { type = \"object\", properties = { channel_id = { type = \"string\" } }, required = [\"channel_id\"] }\nannotator = \"acl\"\n";
    let config = load(&policy(routed)).expect("a mandate placeholder loads");
    let mandate = config
        .engine()
        .registry()
        .annotator_mandate(&AnnotatorName::new("acl"))
        .expect("acl registers");
    assert_eq!(
        mandate.audiences().entries().collect::<Vec<_>>(),
        ["@slack:channel/$channel_id"]
    );
    assert_eq!(
        mandate
            .instantiate(&serde_json::json!({ "channel_id": "C1" }))
            .expect("C1 fills the placeholder")
            .audiences()
            .entries()
            .collect::<Vec<_>>(),
        ["@slack:channel/C1"]
    );
    assert!(config.engine().registry().audience().providers().contains("slack"));

    let unbound = "[[tool]]\nname = \"read_channel\"\nannotator = \"acl\"\n";
    let implied = load(&policy(unbound)).expect("the mandate's argument implies its schema");
    let Some(ToolDeclaration::Annotated { parameters, .. }) = implied
        .engine()
        .registry()
        .variants(&ToolName::new("read_channel"))
        .next()
    else {
        panic!("read_channel is Annotator-routed");
    };
    let schema = parameters.normalized();
    assert_eq!(schema["properties"]["channel_id"]["type"], "string");
    assert_eq!(schema["required"], serde_json::json!(["channel_id"]));
    let wildcard = "[[tool]]\nname = \"*\"\nannotator = \"acl\"\n";
    assert!(matches!(
        load(&policy(wildcard)),
        Err(ConfigError::Registry(LoadError::WildcardPlaceholderMandate(name))) if name == "acl"
    ));
}

/// A provider is part of the policy exactly when the policy names it — by an `[audience]`
/// selector, a mention in a contract or a mandate, or a placeholder — and no other
/// declared source enters the registry or the identity.
#[test]
fn a_provider_enters_the_policy_when_a_contract_or_mandate_names_it() {
    let providers = |policy: &str| {
        load(policy)
            .expect("the policy loads")
            .engine()
            .registry()
            .audience()
            .providers()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(providers("version = 2\n[[tool]]\nname = \"t\"\n").is_empty());
    assert_eq!(
        providers("version = 2\n[[tool]]\nname = \"t\"\ndelta = { audience = [\"@github:org/corp/team/x\"] }\n"),
        ["github"]
    );
    assert_eq!(
        providers(
            "version = 2\n[[tool]]\nname = \"t\"\nrequires = { audience = { within = [\"@google-workspace:group/eng@corp.com\"] } }\n"
        ),
        ["google-workspace"]
    );
    assert_eq!(
        providers("version = 2\n[[annotator]]\nname = \"acl\"\naudiences = [\"@slack:user-group/eng\"]\n"),
        ["slack"]
    );
}
