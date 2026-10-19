use super::*;

/// A bare explicit declaration always matches and beats the wildcard. Calls without
/// a matching explicit declaration require the wildcard annotator's annotation.
#[test]
fn an_exact_declaration_beats_the_wildcard_on_a_proposal() {
    let mut cfg = test_config(vec![plain_tool("read")]);
    cfg.tools.push(wildcard("any"));
    cfg.annotators.push(annotator("any"));
    let e = open_engine(cfg);
    let log = vec![opened(&e)];
    let exact = proposed(&e, &log, "b1", nonce(), call("read", json!({}))).expect("the static declaration decides");
    assert!(
        matches!(&exact.follow_up, FollowUp::Proposals { released, .. } if !released.is_empty()),
        "the exact declaration releases without an annotation consult: {:?}",
        exact.follow_up
    );
    assert_eq!(
        proposed(&e, &log, "b2", nonce(), call("ghost", json!({}))).err(),
        Some(TransitionError::AnnotationNeeded {
            annotators: vec![crate::names::AnnotatorName::new("any")]
        })
    );
}

#[test]
fn includes_missing_placeholder_is_an_invalid_call_and_still_fails_closed_underneath() {
    let send = ToolAnnotation {
        parameters: crate::params::test_string_argument_schema("to"),
        requires: Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into()))],
            },
            ..Requires::default()
        },
        ..plain_tool("send_email")
    };
    let e = engine(vec![send]);
    let malformed = call("send_email", json!({}));
    let log = vec![opened(&e)];
    let p = Projection::build(&log, log.len() as u64);
    assert!(matches!(
        e.check(&p.view(&traj()), &malformed),
        Err(EngineError::InvalidCall(_))
    ));

    let contract = e
        .registry
        .tool(&ToolName::new("send_email"))
        .unwrap()
        .declared()
        .expect("a static declaration");
    let evaluate = |log: &[Fact]| {
        let p = Projection::build(log, log.len() as u64);
        let parts = crate::label::TestContext::default();
        crate::check::evaluate(
            contract,
            &p.view(&traj()),
            &malformed,
            &CallStage::default(),
            CallRole::Ordinary,
            &parts.context(),
        )
    };
    match evaluate(&log) {
        Ok(CheckOutcome::Block(b)) => assert!(matches!(b.requirement_gaps.as_slice(), [Gap::Includes { .. }])),
        other => panic!("expected includes gap on a malformed call, got {other:?}"),
    }
}

#[test]
fn required_rulings_route_each_gap_to_its_authority() {
    use crate::authority::{Authority, Mandate};
    use crate::names::AuthorityName;

    let wire = ToolAnnotation {
        requires: Requires {
            label: floor(TRUSTED),
            ..Requires::default()
        },
        ..plain_tool("wire")
    };
    let officer = Authority {
        name: AuthorityName::new("officer"),
        mandate: Mandate {
            trust_ceiling: Some(TRUSTED),
            ..Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let cfg = RegistryConfig {
        authorities: vec![officer],
        ..test_config(vec![wire])
    };
    let e = open_engine_at(cfg, Label::new(SUSPICIOUS, Audience::public()));
    let log = vec![opened(&e)];
    let p = Projection::build(&log, log.len() as u64);
    let t = traj();
    let wire_call = call("wire", json!({}));
    let raw = match e.check(&p.view(&t), &wire_call).unwrap() {
        CheckOutcome::Block(raw) => raw,
        other => panic!("expected a block, got {other:?}"),
    };
    let planned = e.plan(&p.view(&t), &wire_call, &raw).unwrap();
    assert_eq!(planned.plans.len(), 1);
    let required = &planned.plans[0].executable().expect("an authority plan").required;
    assert_eq!(required.len(), 1);
    assert_eq!(required[0].authority, AuthorityName::new("officer"));
    assert_eq!(
        required[0].covers,
        vec![Gap::TrustFloor {
            required: TRUSTED,
            actual: SUSPICIOUS,
        }]
    );
}

#[test]
fn schema_invalid_arguments_are_an_invalid_call_at_every_fresh_entry_point() {
    let e = engine(vec![strict_tool("send")]);
    let log = vec![opened(&e)];
    let p = Projection::build(&log, 1);
    let t = traj();
    let views = p.view(&t);
    let bogus = call("send", json!({ "bogus": 1 }));
    assert!(matches!(e.check(&views, &bogus), Err(EngineError::InvalidCall(_))));
    let raw = crate::check::RawBlock {
        requirement_gaps: vec![],
        narrowing: None,
    };
    assert!(matches!(e.plan(&views, &bogus, &raw), Err(EngineError::InvalidCall(_))));
    assert_eq!(
        e.check(&views, &call("send", json!({ "to": "hr" }))).unwrap(),
        CheckOutcome::Allow
    );
}

#[test]
fn resolve_call_owns_tool_lookup_scanning_and_schema_binding() {
    let e = engine(vec![strict_tool("send")]);

    let resolved = e
        .resolve_call(ToolName::new("send"), br#"{ "to": "hr" }"#)
        .expect("the registered schema accepts the call");
    assert_eq!(resolved.canonical_arguments().canonical_text(), r#"{"to":"hr"}"#);

    assert!(matches!(
        e.resolve_call(ToolName::new("send"), br#"{"to":"hr","to":"finance"}"#),
        Err(EngineError::InvalidCall(ArgumentError::DuplicateKey(key))) if key == "to"
    ));
    assert!(matches!(
        e.resolve_call(ToolName::new("send"), br#"{"bogus":true}"#),
        Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
    ));
    // A name no declaration and no wildcard covers has no contract: the refusal is typed.
    assert!(matches!(
        e.resolve_call(ToolName::new("ghost"), br#"{}"#),
        Err(EngineError::UnknownTool(name)) if name == "ghost"
    ));
    // With a wildcard, the same name resolves onto it at ordinal zero.
    let mut cfg = test_config(vec![strict_tool("send")]);
    cfg.tools.push(wildcard("any"));
    cfg.annotators.push(annotator("any"));
    let covered = open_engine(cfg);
    let ghost = covered
        .resolve_call(ToolName::new("ghost"), br#"{}"#)
        .expect("the wildcard covers the name");
    assert_eq!(
        covered.registry.selected(&ghost),
        Some(crate::value::ToolDeclarationId::default())
    );
}

#[test]
fn ordered_selectors_choose_once_before_schema_validation() {
    let mut first = plain_tool("read(path:secret*)");
    first.parameters = crate::params::ToolParameters::compile(&json!({
        "type": "object",
        "properties": { "path": { "type": "string" }, "token": { "type": "string" } },
        "required": ["path", "token"]
    }))
    .unwrap();
    let mut overlap = plain_tool("read(path:*)");
    overlap.parameters = crate::params::test_string_argument_schema("path");
    let fallback = plain_tool("read");
    let e = engine(vec![first, overlap, fallback]);

    let selected = e
        .resolve_call(ToolName::new("read"), br#"{"path":"secret.txt","token":"ok"}"#)
        .unwrap();
    assert_eq!(
        e.registry.selected(&selected).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert!(
        matches!(
            e.resolve_call(ToolName::new("read"), br#"{"path":"secret.txt"}"#),
            Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
        ),
        "the first match's schema failure must not fall through"
    );
    let overlap = e.resolve_call(ToolName::new("read"), br#"{"path":"public"}"#).unwrap();
    assert_eq!(
        e.registry.selected(&overlap).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    let fallback = e.resolve_call(ToolName::new("read"), br#"{}"#).unwrap();
    assert_eq!(
        e.registry.selected(&fallback).unwrap(),
        crate::value::ToolDeclarationId::new(2).unwrap()
    );
    let no_fallback = engine(vec![plain_tool("read(path:secret*)")]);
    assert!(matches!(
        no_fallback.resolve_call(ToolName::new("read"), br#"{}"#),
        Err(EngineError::InvalidCall(ArgumentError::NoMatchingContract))
    ));
    assert!(matches!(
        e.resolve_call(ToolName::new("read"), br#"{"path":"x","path":"y"}"#),
        Err(EngineError::InvalidCall(ArgumentError::DuplicateKey(_)))
    ));
}

#[test]
fn selector_order_and_matchers_move_policy_identity_but_normalized_wildcards_do_not() {
    let identity = |names: &[&str]| {
        engine(names.iter().map(|name| plain_tool(name)).collect())
            .identity()
            .bytes()
            .to_owned()
    };
    let base = identity(&["read(path:secret*)", "read"]);
    assert_ne!(identity(&["read", "read(path:secret*)"]), base);
    assert_ne!(identity(&["read(path:private*)", "read"]), base);
    assert_eq!(identity(&["read(path:secret**)", "read"]), base);
    assert_eq!(identity(&["read", "send"]), identity(&["send", "read"]));

    // A conjunction is commutative, so clause order is spelling, not policy: the two
    // spellings are one matcher and one identity. Naming another argument is not.
    let conjunction = identity(&["read(path:secret*,mode:rw)", "read"]);
    assert_eq!(identity(&["read(mode:rw,path:secret*)", "read"]), conjunction);
    assert_ne!(conjunction, base, "a second clause is a different predicate");
    assert_ne!(identity(&["read(path:secret*,mode:ro)", "read"]), conjunction);
    assert_ne!(identity(&["read(path:secret*,scope:rw)", "read"]), conjunction);
}

#[test]
fn wildcard_fallback_does_not_bypass_a_matched_contract_schema() {
    let mut strict = plain_tool("builtin:shell(command:git push*)");
    strict.parameters = crate::params::ToolParameters::compile(&json!({
        "type": "object",
        "properties": { "command": { "type": "string" }, "token": { "type": "string" } },
        "required": ["command", "token"]
    }))
    .unwrap();
    let mut cfg = test_config(vec![strict]);
    cfg.tools.push(wildcard("any"));
    cfg.annotators.push(annotator("any"));
    let e = open_engine(cfg);
    let shell = ToolName::new("builtin:shell");
    let fallback = e.resolve_call(shell.clone(), br#"{"command":"ls"}"#).unwrap();
    assert_eq!(
        e.registry.selected(&fallback).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    assert_eq!(
        e.registry.declaration(&fallback).unwrap().annotator(),
        Some(&crate::names::AnnotatorName::new("any"))
    );
    assert!(matches!(
        e.resolve_call(shell.clone(), br#"{"command":"git push"}"#),
        Err(EngineError::InvalidCall(ArgumentError::Schema(_)))
    ));
    let matched = e
        .resolve_call(shell, br#"{"command":"git push","token":"ok"}"#)
        .unwrap();
    assert_eq!(
        e.registry.selected(&matched).unwrap(),
        crate::value::ToolDeclarationId::default()
    );
    assert!(!e.registry.selects_alike(&fallback, &matched));
}

#[test]
fn a_substitution_that_selects_another_ordered_contract_is_a_new_call_under_it() {
    let e = engine(vec![plain_tool("read(path:safe*)"), plain_tool("read(path:*)")]);
    let selected = e
        .resolve_call(ToolName::new("read"), br#"{"path":"private.txt"}"#)
        .unwrap();
    assert_eq!(
        e.registry.selected(&selected).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    let rewrite = |body: &str| substituted_call(&e.registry, &selected, &ValueBody::new(body), None);

    // Arguments that select another declaration render a new call under it: the selected
    // ordinal, nothing carried.
    let fresh = rewrite(r#"{"path":"safe.txt"}"#).expect("a new call under declaration 0");
    assert_eq!(
        e.registry.selected(&fresh).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert!(fresh.annotation().is_none());
    assert!(!e.registry.selects_alike(&selected, &fresh));

    // Arguments that stay in the declaration render the substitution.
    let kept = rewrite(r#"{"path":"other-private.txt"}"#).expect("the substitution");
    assert!(e.registry.selects_alike(&kept, &selected));
    assert!(kept.annotation().is_none());

    // Arguments no declaration selects, or that fail the selected schema, mint nothing.
    assert!(matches!(
        rewrite(r#"{"path":7}"#),
        Err(TransitionError::Call(EngineError::InvalidCall(
            ArgumentError::NoMatchingContract
        )))
    ));
}

/// One harness tool under two ordered contracts, both in the input sanitizers' scope.
/// `read(path:public/*)` at 0 uses an annotator that reads the complete call and owns the
/// recipients it requires; `read(path:private/*)` at 1 records a classified read and
/// requires `partner` and every desk in `desks` statically. `redact` widens the audience
/// from internal to internal+partner, `widen` from partner to partner+auditor.
fn ordered_read_engine(desks: &[&str]) -> Engine {
    ordered_read_engine_tagged(desks, "outbound")
}

/// [`ordered_read_engine`] with the private contract carrying `private_tag` instead of
/// `outbound`.
fn ordered_read_engine_tagged(desks: &[&str], private_tag: &str) -> Engine {
    let read = ordered_read;
    let includes = |reader: &str| {
        AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(Audience::restricted(
            [ReaderId::new(reader)],
        ))))
    };
    let public = annotated(read("read(path:public/*)"), "classify");
    let private = ToolAnnotation {
        emits: classified_read(),
        tags: vec![crate::names::TagName::new(private_tag)],
        requires: Requires {
            label: LabelRequirements {
                trust_floor: Some(TRUSTED),
                audience: std::iter::once("partner")
                    .chain(desks.iter().copied())
                    .map(includes)
                    .collect(),
            },
            ..Requires::default()
        },
        ..read("read(path:private/*)")
    };
    open_engine_at(
        RegistryConfig {
            tools: {
                let mut tools = vec![public];
                tools.extend(declared(vec![private, plain_tool("note")]));
                tools
            },
            annotators: vec![annotator_with_readers(
                "classify",
                &["insider", "partner", "auditor", "legal", "press"],
            )],
            sanitizers: vec![
                input_sanitizer("redact", &["insider"], &["insider", "partner"]),
                input_sanitizer("widen", &["partner"], &["insider", "partner", "auditor"]),
            ],
            ..test_config(vec![])
        },
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    )
}

fn classified_read() -> EffectSet {
    EffectSet::new([EffectKind::new("classified.read")]).unwrap()
}

fn ordered_read(name: &str) -> ToolAnnotation {
    ToolAnnotation {
        tags: vec![crate::names::TagName::new("outbound")],
        parameters: crate::params::ToolParameters::compile(&json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false,
        }))
        .unwrap(),
        requires: Requires {
            label: floor(TRUSTED),
            ..Requires::default()
        },
        ..plain_tool(name)
    }
}

/// The complete annotation `classify` produces for a `read` call under the public
/// declaration: the declaration's operational metadata with the readers the call requires.
/// It is evidence for exactly one canonical call: a rewrite is annotated afresh or not at all.
fn read_pin(bound_to: &ResolvedCall, readers: &[&str]) -> crate::contract::PinnedAnnotation {
    let mut produced = ordered_read("read");
    produced.requires.label.audience = readers
        .iter()
        .map(|reader| {
            AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(Audience::restricted(
                [ReaderId::new(*reader)],
            ))))
        })
        .collect();
    pinned_for(produced, "classify", bound_to)
}

fn read_of(e: &Engine, path: &str) -> ResolvedCall {
    e.resolve_call(ToolName::new("read"), format!(r#"{{"path":"{path}"}}"#).as_bytes())
        .expect("the call resolves")
}

fn rewrite(
    call: &ResolvedCall,
    sanitizer: &str,
    replacement: &str,
    annotation: Option<crate::contract::PinnedAnnotation>,
) -> OfferOutcome {
    OfferOutcome::Derived(crate::transition::Evidence::Rewrite {
        sanitizer: crate::names::SanitizerName::new(sanitizer),
        source: crate::value::RawResultDigest::of(call.canonical_arguments().canonical_bytes()),
        derived: ValueBody::new(replacement),
        annotation,
    })
}

/// The declaration `e` selects for the call the batch opened, its reserved effects and its pin.
fn opened_contract_of(
    e: &Engine,
    facts: &[Fact],
) -> (
    crate::value::ToolDeclarationId,
    EffectSet,
    Option<crate::contract::PinnedAnnotation>,
) {
    facts
        .iter()
        .find_map(|fact| match fact {
            Fact::DispatchOpened {
                tool,
                arguments,
                proposed_effects,
                annotation,
                ..
            } => Some((
                e.registry
                    .selected(&ResolvedCall::new(tool.clone(), arguments.clone()))
                    .expect("the opened call selects a declaration"),
                proposed_effects.clone(),
                annotation.clone(),
            )),
            _ => None,
        })
        .expect("the rewrite dispatches")
}

fn includes_gap(reader: &str) -> Gap {
    Gap::Includes {
        recipients: DeclaredAudience::restricted([ReaderId::new(reader)]),
    }
}

#[test]
fn a_rewrite_that_selects_another_contract_is_judged_under_it_with_the_answers_consulted_for_it() {
    let e = ordered_read_engine(&["auditor", "legal"]);
    let proposal = read_of(&e, "private/q3.md");
    assert_eq!(
        e.registry.selected(&proposal).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = hop_named(&facts, "redact");
    let log = [log, facts].concat();
    let public = r#"{"path":"public/q3.md"}"#;

    // The rewritten arguments select the public declaration, whose annotator was never
    // asked: the rewrite is a new call under it, and its annotation is owed first.
    assert_eq!(
        execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, None)).err(),
        Some(TransitionError::AnnotationNeeded {
            annotators: vec![crate::names::AnnotatorName::new("classify")]
        })
    );
    let answer = read_pin(&read_of(&e, "public/q3.md"), &["partner"]);
    let hopped = execute_offer(
        &e,
        &log,
        hop,
        rewrite(&proposal, "redact", public, Some(answer.clone())),
    )
    .expect("the hop runs");
    let approved = approved_by(&hopped);
    assert_eq!(
        e.registry.selected(&approved).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert_eq!(approved.annotation(), Some(&answer));
    let log = [log, appended_facts(hopped)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    let facts = taken(&e, &log, "b2", &approved);
    assert_eq!(
        opened_contract_of(&e, &facts),
        (
            crate::value::ToolDeclarationId::new(0).unwrap(),
            EffectSet::default(),
            approved.annotation().cloned()
        ),
        "the opening records the public declaration: its effects, not the classified read's, and its annotation"
    );
    assert_eq!(e.validate_replay(&[log.clone(), facts].concat()), Ok(()));

    // Replay holds the record to the same rule: the tool is the one the sanitizer rewrote —
    // another tool's open declaration is no place to land.
    let derived_at = log
        .iter()
        .position(|fact| matches!(fact, Fact::CandidateDerived { .. }))
        .expect("the hop derived a candidate");
    let mut forged = log.clone();
    let Fact::CandidateDerived {
        derived: DerivedCandidate::Call { call, .. },
        ..
    } = &mut forged[derived_at]
    else {
        unreachable!("found above")
    };
    *call = ResolvedCall::new(ToolName::new("note"), approved.canonical_arguments().clone());
    assert_eq!(e.validate_replay(&forged), Err(TransitionRefusal::ForgedLabel));
}

#[test]
fn a_rewrite_into_the_classified_declaration_records_its_effect_and_carries_no_annotation() {
    let e = ordered_read_engine(&[]);
    let unpinned = read_of(&e, "public/q3.md");
    let proposal = unpinned
        .clone()
        .with_annotation(Some(read_pin(&unpinned, &["partner"])));
    assert_eq!(
        e.registry.selected(&proposal).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = hop_named(&facts, "redact");
    let log = [log, facts].concat();

    let hopped = execute_offer(
        &e,
        &log,
        hop,
        rewrite(&proposal, "redact", r#"{"path":"private/q3.md"}"#, None),
    )
    .expect("the hop runs");
    let approved = approved_by(&hopped);
    assert_eq!(
        e.registry.selected(&approved).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    assert!(
        approved.annotation().is_none(),
        "the classified declaration is statically declared; the public annotation does not ride along"
    );
    let hopped_log = [log.clone(), appended_facts(hopped)].concat();
    assert_eq!(e.validate_replay(&hopped_log), Ok(()));
    let facts = taken(&e, &hopped_log, "b2", &approved);
    assert_eq!(
        opened_contract_of(&e, &facts),
        (
            crate::value::ToolDeclarationId::new(1).unwrap(),
            classified_read(),
            None
        ),
        "the opening records the classified read the selected declaration emits"
    );
    assert_eq!(e.validate_replay(&[hopped_log, facts].concat()), Ok(()));

    // A rewrite that stays in the annotated declaration is annotated afresh or not at all:
    // the proposal's annotation never rides through, and none means the rewrite still owes one.
    let public = r#"{"path":"public/q4.md"}"#;
    assert_eq!(
        execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, None)).err(),
        Some(TransitionError::AnnotationNeeded {
            annotators: vec![crate::names::AnnotatorName::new("classify")]
        })
    );
    let fresh = read_pin(&read_of(&e, "public/q4.md"), &["partner"]);
    let kept = approved_by(
        &execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, Some(fresh.clone()))).expect("the hop runs"),
    );
    assert_eq!(
        e.registry.selected(&kept).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert_eq!(kept.annotation(), Some(&fresh));
}

#[test]
fn a_rewrite_into_a_contract_the_sanitizer_does_not_reach_is_refused() {
    let e = ordered_read_engine_tagged(&[], "classified");
    let unpinned = read_of(&e, "public/q3.md");
    let proposal = unpinned
        .clone()
        .with_annotation(Some(read_pin(&unpinned, &["partner"])));
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = hop_named(&facts, "redact");
    let log = [log, facts].concat();

    // `redact` reaches `outbound` contracts. The public contract is one; the private contract
    // the rewritten arguments select is tagged to keep sanitizers off it, so the rewrite is
    // refused even though the private contract's own requirements would be met.
    assert_eq!(
        execute_offer(
            &e,
            &log,
            hop,
            rewrite(&proposal, "redact", r#"{"path":"private/q3.md"}"#, None),
        )
        .err(),
        Some(TransitionError::SanitizerUnapplicable)
    );
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn a_rewrite_within_the_declaration_is_annotated_afresh() {
    let e = ordered_read_engine(&["auditor", "legal"]);
    let proposal = read_of(&e, "private/q3.md");
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let redact = hop_named(&facts, "redact");
    let log = [log, facts].concat();

    // The first hop selects the public declaration; its annotation requires the auditor,
    // which the redaction does not reach, so the rewritten call blocks on that one gap — the
    // public declaration's, not the classified read's partner, auditor and legal desks.
    let answer = read_pin(&read_of(&e, "public/q3.md"), &["auditor"]);
    let hopped = execute_offer(
        &e,
        &log,
        redact,
        rewrite(&proposal, "redact", r#"{"path":"public/q3.md"}"#, Some(answer.clone())),
    )
    .expect("the hop runs");
    let block = match offer_answer(&hopped) {
        OfferFollowUp::Substituted { block } => (**block).clone(),
        other => panic!("the substitution re-plans over the derived call, got {other:?}"),
    };
    assert_eq!(
        e.registry.selected(&block.call).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert_eq!(block.call.annotation(), Some(&answer));
    assert_eq!(block.block.raw.requirement_gaps, vec![includes_gap("auditor")]);
    let facts = appended_facts(hopped);
    let widen = hop_named(&facts, "widen");
    let log = [log, facts].concat();

    // Deciding the batch again reads the candidate under the contract it selected.
    match proposed(&e, &log, "b1", nonce(), proposal.clone())
        .expect("the batch answers from the record")
        .follow_up
    {
        FollowUp::Proposals { blocked, .. } => {
            assert_eq!(blocked[0].call, block.call);
            assert_eq!(blocked[0].block.raw.requirement_gaps, vec![includes_gap("auditor")]);
        }
        other => panic!("a repeated batch answers as proposals, got {other:?}"),
    }

    // The second hop stays in the public declaration: annotation evidence binds the exact
    // canonical call, so the rewrite carries the fresh annotation obtained for it.
    let widened = read_pin(&read_of(&e, "public/q3-v2.md"), &["auditor"]);
    let hopped = execute_offer(
        &e,
        &log,
        widen,
        rewrite(
            &block.call,
            "widen",
            r#"{"path":"public/q3-v2.md"}"#,
            Some(widened.clone()),
        ),
    )
    .expect("the hop runs");
    let approved = approved_by(&hopped);
    assert_eq!(
        e.registry.selected(&approved).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    assert_eq!(approved.annotation(), Some(&widened));
    let log = [log, appended_facts(hopped)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));
    assert_eq!(
        e.validate_replay(&[log.clone(), taken(&e, &log, "b2", &approved)].concat()),
        Ok(())
    );
}

#[test]
fn a_rewrite_that_selects_a_contract_with_other_gaps_is_no_remedy_for_this_block() {
    let e = ordered_read_engine(&["auditor"]);
    let proposal = read_of(&e, "private/q3.md");
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = hop_named(&facts, "redact");
    let log = [log, facts].concat();
    let public = r#"{"path":"public/q3.md"}"#;

    // The block wants partner and auditor. A rewritten call that wants the press desk, or the
    // auditor and legal desks together, is blocked on something this block never was: the hop
    // improves nothing, lands no record, and the offer stands.
    for readers in [&["press"][..], &["auditor", "legal"][..]] {
        let answer = read_pin(&read_of(&e, "public/q3.md"), readers);
        assert_eq!(
            execute_offer(&e, &log, hop, rewrite(&proposal, "redact", public, Some(answer))).err(),
            Some(TransitionError::SanitizerUnapplicable)
        );
    }
    // A replacement the selected contract's schema refuses mints no call.
    assert!(matches!(
        execute_offer(
            &e,
            &log,
            hop,
            rewrite(&proposal, "redact", r#"{"path":"public/q3.md","extra":1}"#, None,),
        ),
        Err(TransitionError::Call(EngineError::InvalidCall(_)))
    ));
    assert_eq!(e.validate_replay(&log), Ok(()));
}

#[test]
fn a_rewrite_into_a_contract_reading_a_group_asks_for_its_answer_afresh() {
    let send = |name: &str, audience: AudienceRequirement| ToolAnnotation {
        parameters: crate::params::test_string_argument_schema("to"),
        tags: vec![crate::names::TagName::new("outbound")],
        requires: Requires {
            label: LabelRequirements {
                trust_floor: None,
                audience: vec![audience],
            },
            ..Requires::default()
        },
        ..plain_tool(name)
    };
    let e = open_engine_at(
        RegistryConfig {
            sanitizers: vec![input_sanitizer(
                "redact",
                &["insider"],
                &["insider", "partner", "partner@corp.com"],
            )],
            audience: slack_groups(&["team"]),
            ..test_config(vec![
                send(
                    "send(to:@*)",
                    AudienceRequirement::Includes(RecipientSpec::Placeholder("to".into())),
                ),
                send(
                    "send",
                    AudienceRequirement::Includes(RecipientSpec::Static(DeclaredAudience::literal(
                        Audience::restricted([ReaderId::new("partner")]),
                    ))),
                ),
            ])
        },
        Label::new(TRUSTED, Audience::restricted([ReaderId::new("insider")])),
    );
    let proposal = e
        .resolve_call(ToolName::new("send"), br#"{"to":"partner-desk"}"#)
        .expect("the call resolves");
    assert_eq!(
        e.registry.selected(&proposal).unwrap(),
        crate::value::ToolDeclarationId::new(1).unwrap()
    );
    let log = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &log, "b1", nonce(), proposal.clone()).expect("the batch decides"));
    let hop = hop_named(&facts, "redact");
    let log = [log, facts].concat();
    let group = r#"{"to":"@team"}"#;

    // The rewritten argument names a group under the first contract: this act owes its answer,
    // whatever any earlier act pinned.
    assert_eq!(
        execute_offer(&e, &log, hop, rewrite(&proposal, "redact", group, None)).err(),
        Some(TransitionError::MembershipNeeded {
            needed: vec![group_atom("team")]
        })
    );
    let answer = source_evidence(vec![user_group(
        "team",
        vec![slack_member("slack:UP", Some("partner@corp.com"))],
    )]);
    let hopped = execute_offer_with(&e, &log, hop, rewrite(&proposal, "redact", group, None), answer.clone())
        .expect("the hop runs");
    let approved = approved_by(&hopped);
    assert_eq!(
        e.registry.selected(&approved).unwrap(),
        crate::value::ToolDeclarationId::new(0).unwrap()
    );
    let log = [log, appended_facts(hopped)].concat();
    assert_eq!(e.validate_replay(&log), Ok(()));

    // The proposal that takes the derivation owes the group's answer on its own account:
    // the derivation carries the stage, never the membership the check reads now.
    let facts = taken_with(&e, &log, "b2", &approved, answer.clone());
    assert!(facts.iter().any(|fact| matches!(
        fact,
        Fact::DispatchOpened { evidence, .. } if evidence == &answer
    )));
    assert_eq!(e.validate_replay(&[log, facts].concat()), Ok(()));
}

/// A call's record names no declaration: replay selects one by the tool and arguments. An
/// ordinal an earlier build recorded beside them is not read.
#[test]
fn replay_selects_a_recorded_calls_declaration_and_reads_no_recorded_ordinal() {
    let e = engine(vec![plain_tool("read(path:*)"), plain_tool("read")]);
    let call = e
        .resolve_call(ToolName::new("read"), br#"{"path":"secret.txt"}"#)
        .unwrap();
    let opening = vec![opened(&e)];
    let facts = appended_facts(proposed(&e, &opening, "b1", nonce(), call).expect("the call releases"));
    let mut wire = serde_json::to_value(&facts).expect("the batch is JSON");
    assert!(
        !wire.to_string().contains("\"declaration\""),
        "no record stores a selection"
    );
    for fact in wire.as_array_mut().expect("a batch is an array") {
        if let Some(decided) = fact.get_mut("ProposalBatchDecided") {
            decided["proposals"][0]["declaration"] = 1.into();
        }
        if let Some(opened) = fact.get_mut("DispatchOpened") {
            opened["declaration"] = 1.into();
        }
    }
    let recorded: Vec<Fact> = serde_json::from_value(wire).expect("an earlier build's batch reads");
    assert_eq!(recorded, facts);
    assert_eq!(e.validate_replay(&[opening, recorded].concat()), Ok(()));
}

#[test]
fn replay_refuses_a_corrupt_dispatched_call() {
    let e = engine(vec![strict_tool("send")]);
    let mut log = vec![opened(&e)];
    let good = call("send", json!({ "to": "hr" }));
    open(&e, &mut log, &good);
    assert_eq!(e.validate_replay(&log), Ok(()));

    let dispatched = |tool: &str, payload: serde_json::Value, minted_from: &ResolvedCall| {
        vec![
            opened(&e),
            Fact::DispatchOpened {
                trajectory: traj(),
                dispatch: DispatchId::new(traj(), minted_from.digest(), 0),
                tool: ToolName::new(tool),
                arguments: crate::params::test_arguments(&payload),
                proposed_label: Label::new(TRUSTED, Audience::public()),
                receiving: Label::new(TRUSTED, Audience::public()),
                proposed_effects: EffectSet::default(),
                annotation: None,
                file_basis: None,
                subject: crate::basis::fixture_subject(&traj()),
                evidence: crate::audience::AudienceEvidence::default(),
                terms: crate::fact::ReleaseTerms::none(),
            },
        ]
    };
    // A record naming a tool nothing covers is refused by name, before any backing check.
    let ghost_call = call("ghost", json!({}));
    assert_eq!(
        e.validate_replay(&dispatched("ghost", json!({}), &ghost_call)),
        Err(TransitionRefusal::UnknownTool("ghost".to_string()))
    );
    // A dispatch no decision backs is refused as such.
    let undecided = call("send", json!({ "to": "hr" }));
    assert_eq!(
        e.validate_replay(&dispatched("send", json!({ "to": "hr" }), &undecided)),
        Err(TransitionRefusal::UnbackedDecision)
    );
    let smuggled = call("send", json!({ "bogus": 1 }));
    assert!(matches!(
        e.validate_replay(&dispatched("send", json!({ "bogus": 1 }), &smuggled)),
        Err(TransitionRefusal::InvalidPayload(_))
    ));
    assert!(matches!(
        e.validate_replay(&dispatched("send", json!({ "to": "hr" }), &smuggled)),
        Err(TransitionRefusal::DigestMismatch)
    ));
}

#[test]
fn the_ruling_binds_the_payload_by_digest_and_never_copies_it() {
    use crate::authority::{Authority, Mandate};
    use crate::names::AuthorityName;
    let mut wire = strict_tool("wire");
    wire.requires.label.trust_floor = Some(TRUSTED);
    let officer = Authority {
        name: AuthorityName::new("officer"),
        mandate: Mandate {
            trust_ceiling: Some(TRUSTED),
            ..Mandate::default()
        },
        scope: crate::authority::Scope::default(),
        hint: None,
    };
    let cfg = RegistryConfig {
        authorities: vec![officer],
        ..test_config(vec![wire])
    };
    let e = open_engine_at(cfg, Label::new(SUSPICIOUS, Audience::public()));

    let wire_call = call("wire", json!({ "to": "distinctive-recipient-hr" }));
    let log = vec![opened(&e)];
    let blocked = appended_facts(proposed(&e, &log, "b1", nonce(), wire_call.clone()).expect("the call blocks"));
    let (offer, plan) = opened_offers(&blocked)
        .into_iter()
        .find(|(_, plan)| !plan.required.is_empty())
        .expect("the block offers an authority plan");
    let log = [log, blocked].concat();

    let evidence = evidence_for(offer, &plan, "wire", Label::new(SUSPICIOUS, Audience::public()));
    let approved =
        appended_facts(execute_offer(&e, &log, offer, OfferOutcome::Approved(evidence)).expect("the offer executes"));
    let log = [log, approved].concat();

    let batch = appended_facts(proposed(&e, &log, "b2", nonce(), wire_call.clone()).expect("the approval releases"));
    let serialized = serde_json::to_string(&batch).unwrap();
    let carriers: Vec<&Fact> = batch
        .iter()
        .filter(|fact| {
            serde_json::to_string(fact)
                .unwrap()
                .contains("distinctive-recipient-hr")
        })
        .collect();
    assert!(
        matches!(
            carriers.as_slice(),
            [Fact::ProposalBatchDecided { .. }, Fact::DispatchOpened { .. }]
        ),
        "the payload lands on the proposal record and the dispatch, not {carriers:?}"
    );
    assert!(matches!(batch.last().unwrap(), Fact::DispatchOpened { .. }));
    let restored: Vec<Fact> = serde_json::from_str(&serialized).unwrap();
    assert_eq!(restored, batch);
}

#[test]
fn an_opening_records_the_label_the_call_was_proposed_at() {
    let internal = Audience::restricted([ReaderId::new("insider")]);
    let e = engine_at(vec![crm_tool()], Label::new(TRUSTED, internal.clone()));
    let mut log = vec![opened(&e)];
    open(&e, &mut log, &call("get_ticket", json!({})));
    match log.last().expect("the release opens a dispatch") {
        Fact::DispatchOpened { proposed_label, .. } => {
            assert_eq!(*proposed_label, Label::new(TRUSTED, internal));
        }
        other => panic!("expected DispatchOpened, got {other:?}"),
    }
}
