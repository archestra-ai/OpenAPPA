//! Load outcomes at the policy entry points that no other test pins: the refusals of each
//! structural and semantic check, the selector-template declarations a deployment supplies,
//! the input-source spellings, a custom trust chain, and the `[boundary]` and `[deployment]`
//! fields as the compiled engine reads them.

use appa_engine::audience::DeclaredTemplate;
use appa_engine::contract::{Delta, DeltaAudience, SelectorPlaceholder, ToolAnnotation};
use appa_engine::label::{Audience, ChainAudience, DeclaredAudience, ReaderId, Trust};
use appa_engine::names::SurfaceName;
use appa_engine::params::ParamsError;
use appa_engine::profile::{BindingMode, OpenVector, SurfaceMode};
use appa_engine::registry::{LoadError, TrustChain};
use appa_policy::{
    Config, ConfigError, InputSource, SelectorDeclaration, ToolCallSource, declare_templates, declared_sources,
    parse_delta,
};

fn contract<'a>(config: &'a Config, name: &str) -> &'a ToolAnnotation {
    config
        .registry()
        .tools()
        .find(|tool| tool.name().as_str() == name)
        .unwrap_or_else(|| panic!("{name} registers"))
        .declared()
        .unwrap_or_else(|| panic!("{name} is declared"))
}

fn default_chain() -> TrustChain {
    TrustChain::new(vec!["suspicious".to_string(), "trusted".to_string()])
}

fn alice() -> DeclaredAudience {
    DeclaredAudience::restricted([ReaderId::new("alice")])
}

// --- version and structural refusals -------------------------------------------

#[test]
fn only_the_supported_dialect_version_loads() {
    assert!(Config::from_toml_str("version = 2\n").is_ok());
    for found in [0, 1, 3] {
        assert!(
            matches!(
                Config::from_toml_str(&format!("version = {found}\n")),
                Err(ConfigError::UnsupportedVersion { found: reported }) if reported == found
            ),
            "version {found} must be refused"
        );
    }
}

#[test]
fn an_unknown_key_is_a_parse_error_in_every_table() {
    for (table, policy) in [
        ("top level", "version = 2\nsurprise = 1\n"),
        ("[[tool]]", "version = 2\n[[tool]]\nname = \"t\"\nsurprise = 1\n"),
        (
            "tool delta",
            "version = 2\n[[tool]]\nname = \"t\"\ndelta = { surprise = 1 }\n",
        ),
        (
            "tool requires",
            "version = 2\n[[tool]]\nname = \"t\"\nrequires = { surprise = 1 }\n",
        ),
        (
            "[[authority]]",
            "version = 2\n[[authority]]\nname = \"a\"\nsurprise = 1\n[authority.permits]\ntrust_below = \"trusted\"\n",
        ),
        (
            "authority permits",
            "version = 2\n[[authority]]\nname = \"a\"\n[authority.permits]\ntrust_below = \"trusted\"\nsurprise = 1\n",
        ),
        (
            "[[sanitizer]]",
            "version = 2\n[[sanitizer]]\nname = \"s\"\non = [\"tool_input\"]\nsurprise = 1\n\
             [sanitizer.permits]\naudience = { from = [\"insider\"], to = [\"partner\"] }\n",
        ),
        (
            "[[annotator]]",
            "version = 2\n[[annotator]]\nname = \"d\"\nsurprise = 1\n",
        ),
        ("[audience]", "version = 2\n[audience]\nsurprise = 1\n"),
        ("[boundary]", "version = 2\n[boundary]\nsurprise = 1\n"),
        ("[limits]", "version = 2\n[limits]\nsurprise = 1\n"),
        ("[deployment]", "version = 2\n[deployment]\nsurprise = 1\n"),
    ] {
        assert!(
            matches!(Config::from_toml_str(policy), Err(ConfigError::Parse(_))),
            "an unknown key in {table} must be refused"
        );
        Config::from_toml_str(&policy.replace("surprise = 1", ""))
            .unwrap_or_else(|error| panic!("{table} without the key: {error}"));
    }
}

// --- tool, sanitizer, and limits refusals --------------------------------------

#[test]
fn a_tool_declaring_one_effect_twice_is_refused() {
    assert!(
        Config::from_toml_str("version = 2\n[[tool]]\nname = \"t\"\neffects = [\"email.sent\", \"backup.done\"]\n")
            .is_ok()
    );
    assert!(matches!(
        Config::from_toml_str("version = 2\n[[tool]]\nname = \"t\"\neffects = [\"email.sent\", \"email.sent\"]\n"),
        Err(ConfigError::DuplicateEffect { tool, kind }) if tool == "t" && kind == "email.sent"
    ));
}

#[test]
fn a_tool_parameters_schema_outside_the_dialect_is_refused() {
    assert!(matches!(
        Config::from_toml_str("version = 2\n[[tool]]\nname = \"t\"\nparameters = { type = \"string\" }\n"),
        Err(ConfigError::ToolParameters { tool, source: ParamsError::RootNotObject }) if tool == "t"
    ));
}

#[test]
fn a_sanitizer_point_outside_tool_input_and_tool_output_is_refused() {
    let policy = |on: &str| {
        format!(
            "version = 2\n[[sanitizer]]\nname = \"s\"\non = {on}\n\
             [sanitizer.permits]\naudience = {{ from = [\"insider\"], to = [\"partner\"] }}\n"
        )
    };
    assert!(matches!(
        Config::from_toml_str(&policy("[\"tool_call\"]")),
        Err(ConfigError::UnknownSanitizerPoint { token }) if token == "tool_call"
    ));
    assert!(matches!(
        Config::from_toml_str(&policy("[\"tool_input\", \"output\"]")),
        Err(ConfigError::UnknownSanitizerPoint { token }) if token == "output"
    ));
}

#[test]
fn a_sanitizer_permits_exactly_one_transition() {
    let policy = |permits: &str| {
        format!("version = 2\n[[sanitizer]]\nname = \"s\"\non = [\"tool_input\"]\n[sanitizer.permits]\n{permits}\n")
    };
    for (case, permits) in [
        (
            "both dimensions",
            "audience = { from = [\"insider\"], to = [\"partner\"] }\ntrust = { from = \"suspicious\", to = \"trusted\" }",
        ),
        ("no dimension", ""),
    ] {
        assert!(
            matches!(
                Config::from_toml_str(&policy(permits)),
                Err(ConfigError::SanitizerMandateShape { name, .. }) if name == "s"
            ),
            "a sanitizer declaring {case} must be refused"
        );
    }
}

#[test]
fn a_zero_planner_cap_is_refused() {
    assert!(Config::from_toml_str("version = 2\n[limits]\nplanner_cap = 1\n").is_ok());
    assert!(matches!(
        Config::from_toml_str("version = 2\n[limits]\nplanner_cap = 0\n"),
        Err(ConfigError::ZeroPlannerCap)
    ));
}

// --- trust chain ---------------------------------------------------------------

#[test]
fn a_custom_trust_chain_replaces_the_default_ranks() {
    let policy = "version = 2\ntrust_chain = [\"untrusted\", \"reviewed\", \"trusted\"]\n\
                  [[tool]]\nname = \"t\"\ndelta = { trust = \"reviewed\" }\nrequires = { trust = \"trusted\" }\n";
    let config = Config::from_toml_str(policy).expect("a custom chain loads");
    assert_eq!(
        config.registry().trust_chain().names().collect::<Vec<_>>(),
        ["untrusted", "reviewed", "trusted"]
    );
    let t = contract(&config, "t");
    assert_eq!(t.delta.trust, Some(Trust::new(1)));
    assert_eq!(t.requires.label.trust_floor, Some(Trust::new(2)));
    assert_eq!(
        config.boundary_label().trust,
        Trust::new(2),
        "the default boundary sits at the chain's top rank"
    );

    let default_rank = policy.replace("\"reviewed\" }", "\"suspicious\" }");
    assert!(matches!(
        Config::from_toml_str(&default_rank),
        Err(ConfigError::UnknownTrustRank { name, .. }) if name == "suspicious"
    ));
}

#[test]
fn an_invalid_trust_chain_is_refused() {
    assert!(matches!(
        Config::from_toml_str("version = 2\ntrust_chain = []\n"),
        Err(ConfigError::Registry(LoadError::EmptyTrustChain))
    ));
    assert!(matches!(
        Config::from_toml_str("version = 2\ntrust_chain = [\"low\", \"high\", \"low\"]\n"),
        Err(ConfigError::Registry(LoadError::DuplicateRank(rank))) if rank == "low"
    ));
}

// --- [boundary] ----------------------------------------------------------------

#[test]
fn the_boundary_table_converts_trust_and_a_reader_audience() {
    let unset = Config::from_toml_str("version = 2\n").expect("loads");
    assert_eq!(unset.boundary_label().trust, Trust::new(1));
    assert_eq!(unset.boundary_label().audience, Audience::public());

    let config = Config::from_toml_str("version = 2\n[boundary]\ntrust = \"suspicious\"\naudience = [\"alice\"]\n")
        .expect("a boundary with a rank and a reader loads");
    assert_eq!(config.boundary_label().trust, Trust::new(0));
    assert_eq!(config.boundary_label().audience, Audience::of_declared(&alice()));

    let trust_only = Config::from_toml_str("version = 2\n[boundary]\ntrust = \"suspicious\"\n").expect("loads");
    assert_eq!(trust_only.boundary_label().audience, Audience::public());

    assert!(matches!(
        Config::from_toml_str("version = 2\n[boundary]\ntrust = \"root\"\n"),
        Err(ConfigError::UnknownTrustRank { name, .. }) if name == "root"
    ));
}

// --- [deployment] --------------------------------------------------------------

#[test]
fn the_deployment_table_accepts_every_surface_mode_binding_and_context_control() {
    let config = Config::from_toml_str(
        "version = 2\n[deployment]\ncontext_control = true\nbinding = \"token\"\n\
         provider_surfaces = { web_search = \"mediated\", code_execution = \"open\" }\n",
    )
    .expect("the deployment fields load");
    let profile = config.engine().profile();
    assert!(profile.context_control());
    assert_eq!(profile.binding(), BindingMode::Token);
    assert_eq!(
        profile
            .provider_surfaces()
            .map(|(surface, mode)| (surface.as_str(), mode))
            .collect::<Vec<_>>(),
        [
            ("code_execution", SurfaceMode::Open),
            ("web_search", SurfaceMode::Mediated)
        ]
    );
    assert_eq!(
        config.engine().open_vectors(),
        [OpenVector::OpenProviderSurface {
            surface: SurfaceName::new("code_execution")
        }],
        "an open surface is an open vector; a mediated one is not"
    );

    let explicit = Config::from_toml_str("version = 2\n[deployment]\ncontext_control = false\nbinding = \"harness\"\n")
        .expect("the explicit defaults load");
    assert!(!explicit.engine().profile().context_control());
    assert_eq!(explicit.engine().profile().binding(), BindingMode::Harness);
    assert_eq!(explicit.engine().profile().provider_surfaces().count(), 0);
}

// --- parse_delta ---------------------------------------------------------------

#[test]
fn parse_delta_reads_the_delta_spelling_against_a_chain() {
    let chain = default_chain();
    assert_eq!(
        parse_delta(None, None, &chain, "floor").expect("an empty delta parses"),
        Delta::default()
    );
    assert_eq!(
        parse_delta(Some("suspicious"), Some(&["alice".to_string()]), &chain, "floor").expect("a full delta parses"),
        Delta {
            trust: Some(Trust::new(0)),
            audience: Some(DeltaAudience::Static(alice())),
        }
    );
    assert_eq!(
        parse_delta(None, Some(&["@slack:channel/$id".to_string()]), &chain, "floor")
            .expect("a placeholder delta parses")
            .audience,
        Some(DeltaAudience::Selector(
            SelectorPlaceholder::parse("slack:channel/$id").expect("a placeholder spelling")
        ))
    );
}

#[test]
fn parse_delta_refuses_an_unknown_rank_and_a_malformed_audience() {
    let chain = default_chain();
    assert!(matches!(
        parse_delta(Some("root"), None, &chain, "floor"),
        Err(ConfigError::UnknownTrustRank { name, context }) if name == "root" && context == "floor"
    ));
    for audience in [
        vec![],
        vec!["alice".to_string(), "alice".to_string()],
        vec!["@slack:channel/$id".to_string(), "alice".to_string()],
    ] {
        assert!(
            matches!(
                parse_delta(None, Some(&audience), &chain, "floor"),
                Err(ConfigError::BadAudience { .. })
            ),
            "{audience:?} must be refused"
        );
    }
}

// --- input-source spellings ----------------------------------------------------

#[test]
fn every_input_source_spelling_round_trips_through_parse() {
    for (spelling, expected) in [
        ("$tool_call", InputSource::Call(ToolCallSource::Call)),
        ("$tool_call.name", InputSource::Call(ToolCallSource::Name)),
        ("$tool_call.description", InputSource::Call(ToolCallSource::Description)),
        ("$tool_call.arguments", InputSource::Call(ToolCallSource::Arguments)),
        (
            "$tool_call.arguments.id",
            InputSource::Call(ToolCallSource::Argument("id".to_string())),
        ),
        ("$input.repo", InputSource::External("repo".to_string())),
        ("$input.github.repo", InputSource::External("github.repo".to_string())),
    ] {
        let parsed = InputSource::parse(spelling).unwrap_or_else(|| panic!("{spelling} parses"));
        assert_eq!(parsed, expected, "{spelling}");
        assert_eq!(parsed.spelling(), spelling);
        if let InputSource::Call(call) = &parsed {
            assert_eq!(ToolCallSource::parse(spelling).as_ref(), Some(call));
            assert_eq!(call.spelling(), spelling);
        }
    }
    assert_eq!(ToolCallSource::parse("$input.repo"), None);
}

// --- selector-template declarations --------------------------------------------

fn selector(template: &str, feeds: Option<&str>) -> SelectorDeclaration {
    SelectorDeclaration {
        template: template.to_string(),
        feeds: feeds.map(str::to_string),
    }
}

#[test]
fn declare_templates_carries_each_template_and_what_it_feeds() {
    let templates = declare_templates(
        "slack",
        &[
            selector("viewer", Some("self")),
            selector("full-members", Some("internal")),
            selector("channel/<id>", None),
        ],
    )
    .expect("the templates declare");
    assert_eq!(
        templates,
        [
            DeclaredTemplate::new("viewer", Some(ChainAudience::Self_)),
            DeclaredTemplate::new("full-members", Some(ChainAudience::Internal)),
            DeclaredTemplate::named("channel/<id>"),
        ]
    );
}

#[test]
fn declare_templates_refuses_every_malformed_list() {
    for (case, selectors) in [
        ("an empty list", vec![]),
        ("an empty template", vec![selector("", None)]),
        ("an empty segment", vec![selector("channel//x", None)]),
        ("a trailing slash", vec![selector("channel/", None)]),
        ("a `$` segment", vec![selector("channel/$id", None)]),
        ("an empty variable", vec![selector("channel/<>", None)]),
        ("an unclosed variable", vec![selector("channel/<id", None)]),
        ("an unopened variable", vec![selector("channel/id>", None)]),
        ("`feeds = public`", vec![selector("viewer", Some("public"))]),
        ("an unknown `feeds`", vec![selector("viewer", Some("everyone"))]),
        (
            "a duplicate template",
            vec![selector("channel/<id>", None), selector("channel/<id>", None)],
        ),
    ] {
        assert!(
            matches!(
                declare_templates("slack", &selectors),
                Err(ConfigError::BadSelectorDeclaration { provider, .. }) if provider == "slack"
            ),
            "{case} must be refused"
        );
    }
}

#[test]
fn declared_sources_reads_every_audience_entry_with_selectors() {
    let document: toml::Value = toml::from_str(
        "[externals.audience.slack]\nselectors = [{ template = \"viewer\", feeds = \"self\" }]\n\
         [externals.audience.roster]\nurl = \"https://roster.invalid\"\n",
    )
    .expect("the document parses");
    let sources = declared_sources(&document).expect("the sources declare");
    assert_eq!(
        sources
            .iter()
            .map(|source| (source.provider.as_str(), source.templates.clone()))
            .collect::<Vec<_>>(),
        [(
            "slack",
            vec![DeclaredTemplate::new("viewer", Some(ChainAudience::Self_))]
        )],
        "an entry without `selectors` declares no source"
    );
    let empty: toml::Value = toml::from_str("version = 2\n").expect("the document parses");
    assert!(declared_sources(&empty).expect("no externals").is_empty());
}

#[test]
fn declared_sources_refuses_a_selectors_value_that_is_not_a_table_list() {
    for selectors in [
        "\"viewer\"",
        "[\"viewer\"]",
        "[{ template = \"viewer\", surprise = 1 }]",
        "[{ template = \"viewer/$x\" }]",
    ] {
        let document: toml::Value = toml::from_str(&format!("[externals.audience.slack]\nselectors = {selectors}\n"))
            .expect("the document parses");
        assert!(
            matches!(
                declared_sources(&document),
                Err(ConfigError::BadSelectorDeclaration { provider, .. }) if provider == "slack"
            ),
            "selectors = {selectors} must be refused"
        );
    }
}
