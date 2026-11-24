use super::*;
use crate::api::AuditEvent;
use appa_runtime_api::{
    AutoApply, HookDecision, HookEvent, LineVariant, Outcome, RefusalClass, SessionSettings, StartKind,
};

fn variant(name: &str) -> LineVariant {
    LineVariant::parse(name).expect("a token")
}

/// What an observing host pins: narrowing accepted at the block, under its `observe` line.
fn observe() -> SessionSettings {
    SessionSettings {
        auto_apply: AutoApply::RulingsAndAccept,
        line_variant: Some(variant("observe")),
    }
}

fn enforce() -> SessionSettings {
    SessionSettings {
        auto_apply: AutoApply::RulingsOnly,
        line_variant: Some(variant("enforce")),
    }
}

async fn start(runtime: &Runtime, root: &TrajectoryId, settings: SessionSettings) -> HookDecision {
    crate::hooks::handle(
        runtime,
        HookEvent::SessionStart {
            root: root.clone(),
            principal: None,
            address: None,
            title: None,
            launch: None,
            start: None,
            settings,
        },
    )
    .await
}

fn settings_of(runtime: &Runtime, root: &TrajectoryId) -> SessionSettings {
    runtime.try_status(root).expect("the root answers its status").settings
}

fn narrowed(runtime: &Runtime) -> bool {
    runtime
        .audit(&root())
        .expect("the root answers its audit")
        .iter()
        .any(|entry| matches!(entry.event, AuditEvent::Narrowed { .. }))
}

/// Whether any execution of an offer was claimed: the record an agent's own execution of a
/// plan leaves, and one the runtime's application at the block does not.
fn claimed(session: &Session) -> bool {
    session
        .inner
        .log(&session.root)
        .expect("the log reads")
        .host_records()
        .iter()
        .any(|record| matches!(record.observation, appa_eventlog::HostObservation::Claimed { .. }))
}

async fn succeed(session: &Session, call: ProposedCall) {
    session
        .on_tool_result(
            call,
            ToolOutcome::Success {
                body: OutcomeBody::Available("data".to_string()),
            },
        )
        .await
        .expect("the result is admitted");
}

/// Under `RulingsAndAccept` a block whose plan only accepts the narrowing is accepted at the
/// block: the call proceeds, and the acceptance is on the record as the agent's is, with no
/// claim on the offer beside it. Under `RulingsOnly` the same block reaches the agent with its
/// offer, and the agent's execution of it leaves that claim.
#[tokio::test]
async fn a_narrowing_is_accepted_at_the_block_only_under_rulings_and_accept() {
    for (settings, accepted) in [
        (observe(), true),
        (SessionSettings::default(), false),
        (enforce(), false),
    ] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let session = runtime
            .create_pinned_session(root(), None, settings.clone())
            .expect("a fresh id opens");
        let decision = session
            .on_tool_call(taint(serde_json::json!({"a": 1})), false)
            .await
            .expect("the call is judged");
        match accepted {
            true => {
                assert!(
                    matches!(decision, ToolCallDecision::Allow { spawn: None, .. }),
                    "{decision:?}"
                );
                assert!(narrowed(&runtime), "the acceptance is recorded");
                assert!(!claimed(&session), "no agent claimed the execution");
                succeed(&session, taint(serde_json::json!({"a": 1}))).await;
                assert_eq!(runtime.status(&root()).expect("the root answers").trust, "suspicious");
            }
            false => {
                let ToolCallDecision::Deny { offers, outcomes, .. } = decision else {
                    panic!("{settings:?}: the narrowing reaches the agent, got {decision:?}");
                };
                assert_eq!(offers.len(), 1);
                assert_eq!(outcomes.deciding(), Outcome::Consent);
                assert!(!narrowed(&runtime));
                // The agent's own execution of the same plan records the same acceptance, and
                // its claim on the offer beside it.
                let quoted = OfferId(offers[0].id.clone());
                assert!(matches!(
                    runtime.execute_remedy(&the_root(), quoted).await,
                    crate::api::RemedyOutcome::Authorized { .. }
                ));
                let released = session
                    .on_tool_call(taint(serde_json::json!({"a": 1})), false)
                    .await
                    .expect("the call is judged");
                assert!(
                    matches!(released, ToolCallDecision::Allow { spawn: None, .. }),
                    "{released:?}"
                );
                assert!(narrowed(&runtime));
                assert!(claimed(&session));
            }
        }
    }
}

/// A plan that accepts the narrowing beside a stock `approve` ruling is applied at the block
/// under `RulingsAndAccept`, and reaches the agent under `RulingsOnly`. A stock approve plan
/// alone is applied under either.
#[tokio::test]
async fn a_stock_approve_plan_is_applied_under_either_setting_and_beside_an_acceptance_only_under_rulings_and_accept() {
    for (settings, with_acceptance) in [(observe(), true), (SessionSettings::default(), false)] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(config_from(STOCK_APPROVED), dir.path().join("appa.db"), None).expect("the deployment opens");
        let session = runtime
            .create_pinned_session(root(), None, settings.clone())
            .expect("a fresh id opens");
        let waived = session
            .on_tool_call(wire(500), false)
            .await
            .expect("the call is judged");
        assert!(
            matches!(waived, ToolCallDecision::Allow { .. }),
            "{settings:?}: {waived:?}"
        );
        succeed(&session, wire(500)).await;

        let narrowing = session
            .on_tool_call(bare("import"), false)
            .await
            .expect("the call is judged");
        match with_acceptance {
            true => assert!(matches!(narrowing, ToolCallDecision::Allow { .. }), "{narrowing:?}"),
            false => assert!(
                matches!(&narrowing, ToolCallDecision::Deny { offers, .. } if !offers.is_empty()),
                "{narrowing:?}"
            ),
        }
    }
}

/// One block `RulingsAndAccept` leaves to the agent: the calls that reach it, the outcome it
/// decides on, and the shape of the offers it makes.
struct Case {
    remedy: &'static str,
    config: Config,
    setup: Vec<ProposedCall>,
    call: ProposedCall,
    spawn: bool,
    deciding: Outcome,
    shape: fn(&[appa_runtime_api::OfferedRemedy]) -> bool,
}

/// Every remedy but a stock ruling and an acceptance stays the agent's under
/// `RulingsAndAccept`: a spawn's return declaration, an input sanitizer's substitution, a
/// ruling by an authority that is not stock `approve`, and a redispatch. Each block reaches the
/// agent as it does under `RulingsOnly`. The narrowing each setup meets on the way is accepted.
#[tokio::test]
async fn rulings_and_accept_leaves_every_other_remedy_to_the_agent() {
    let with_approver = format!(
        "{FETCH_AND_SEND}\n[[policy.authority]]\nname = \"approver\"\n[policy.authority.permits]\ntrust_below = \"trusted\"\n"
    );
    let read_hr = ProposedCall {
        tool: "read_hr".to_string(),
        arguments: raw(serde_json::json!({})),
        cwd: None,
    };
    let cases: [Case; 4] = [
        Case {
            remedy: "a declaration",
            config: config_with(FETCH_AND_SEND, None),
            setup: vec![],
            call: fetch(serde_json::json!({"a": 9})),
            spawn: true,
            deciding: Outcome::AgentChoice,
            shape: |offers| !offers.is_empty() && offers.iter().all(|offer| offer.returns.is_some()),
        },
        Case {
            remedy: "an input sanitizer",
            config: substituting_config(SUBSTITUTED_SEND, None),
            setup: vec![read_hr],
            call: send(RAW_BODY),
            spawn: false,
            deciding: Outcome::AgentChoice,
            shape: |offers| !offers.is_empty() && offers.iter().all(|offer| offer.input_sanitizer.is_some()),
        },
        Case {
            remedy: "an authority that is not stock approve",
            config: config_with(&with_approver, None),
            setup: vec![taint(serde_json::json!({"a": 1}))],
            call: trusted_send(),
            spawn: false,
            deciding: Outcome::WaivableGap,
            shape: |offers| offers.len() == 1,
        },
        Case {
            remedy: "a redispatch",
            config: config_with(HISTORY, None),
            setup: vec![],
            call: history_call("need"),
            spawn: false,
            deciding: Outcome::AgentChoice,
            shape: |offers| offers.is_empty(),
        },
    ];
    for Case {
        remedy,
        config,
        setup,
        call,
        spawn,
        deciding,
        shape,
    } in cases
    {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config, dir.path().join("appa.db"), None).expect("the deployment opens");
        let session = runtime
            .create_pinned_session(root(), None, observe())
            .expect("a fresh id opens");
        for call in setup {
            let decision = session
                .on_tool_call(call.clone(), false)
                .await
                .expect("the call is judged");
            assert!(
                matches!(decision, ToolCallDecision::Allow { .. }),
                "{remedy}: the setup's narrowing is accepted, got {decision:?}"
            );
            succeed(&session, call).await;
        }
        let decision = session.on_tool_call(call, spawn).await.expect("the call is judged");
        let ToolCallDecision::Deny { offers, outcomes, .. } = &decision else {
            panic!("{remedy} reaches the agent, got {decision:?}");
        };
        assert_eq!(outcomes.deciding(), deciding, "{remedy}");
        assert!(shape(offers), "{remedy}: {offers:?}");
        assert!(!claimed(&session), "{remedy}");
    }
}

/// A child reads the settings its family's opening pinned: under `RulingsAndAccept` the
/// narrowing its floor allows is accepted at its block, under `RulingsOnly` it reaches it.
#[tokio::test]
async fn a_child_inherits_its_familys_settings() {
    for (settings, accepted) in [(observe(), true), (SessionSettings::default(), false)] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
            .expect("the deployment opens");
        let mut session = runtime
            .create_pinned_session(root(), None, settings.clone())
            .expect("a fresh id opens");
        let child = open_child_floored(
            &mut session,
            fetch(serde_json::json!({"a": 9})),
            TrajectoryId("cc:root:child".to_string()),
            floor_trust("suspicious"),
        )
        .await;
        assert_eq!(child.settings().expect("the child reads its family"), settings);
        let decision = child
            .on_tool_call(taint(serde_json::json!({"a": 2})), false)
            .await
            .expect("the call is judged");
        assert_eq!(
            matches!(decision, ToolCallDecision::Allow { .. }),
            accepted,
            "{settings:?}: {decision:?}"
        );
    }
}

/// The first start pins the family's settings. A later start naming another line variant is
/// refused; one naming the same one or none continues under what was pinned, whatever
/// `auto_apply` it names.
#[tokio::test]
async fn a_later_start_cannot_change_the_line_variant_its_family_pinned() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    assert_eq!(start(&runtime, &root(), observe()).await, HookDecision::Ack);
    assert!(matches!(
        start(&runtime, &root(), enforce()).await,
        HookDecision::Refuse {
            class: RefusalClass::Trajectory,
            ..
        }
    ));
    assert_eq!(start(&runtime, &root(), observe()).await, HookDecision::Ack);
    assert_eq!(
        start(&runtime, &root(), SessionSettings::default()).await,
        HookDecision::Ack
    );
    assert_eq!(settings_of(&runtime, &root()), observe());
}

/// A family opened before line variants pinned none: its opening is stored exactly as such a
/// family's was. A later start naming a line variant continues it without pinning anything,
/// and the family keeps deciding under the defaults it opened with.
#[tokio::test]
async fn a_family_pinned_without_a_line_variant_continues_under_a_start_that_names_one() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    let session = runtime.create_session(root(), None).expect("a fresh id opens");
    let log = session.inner.log(&session.root).expect("the log reads");
    let stored = serde_json::to_value(&log.facts()[0]).expect("the opening serializes");
    let opening = stored["TrajectoryOpened"]
        .as_object()
        .expect("the first fact is the opening");
    assert!(!opening.contains_key("settings"), "stored as a legacy opening");

    for named in [observe(), enforce(), observe()] {
        assert_eq!(start(&runtime, &root(), named).await, HookDecision::Ack);
    }
    assert!(settings_of(&runtime, &root()).is_default());
    let decision = session
        .on_tool_call(taint(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is judged");
    assert!(
        matches!(&decision, ToolCallDecision::Deny { offers, .. } if offers.len() == 1),
        "the narrowing still reaches the agent: {decision:?}"
    );
}

/// A root fork pins the settings it was opened with, not its parent's, and a start that names
/// none afterwards keeps them. Opening it again is refused only under another line variant.
#[tokio::test]
async fn a_root_fork_pins_its_settings_before_its_start() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    assert_eq!(start(&runtime, &root(), enforce()).await, HookDecision::Ack);
    let fork = root_fork_id("observed-fork");
    runtime
        .open_root_fork(&root(), &root(), &fork, observe())
        .expect("the root forks");
    assert_eq!(
        start(&runtime, &fork, SessionSettings::default()).await,
        HookDecision::Ack
    );
    assert_eq!(settings_of(&runtime, &fork), observe());
    assert_eq!(settings_of(&runtime, &root()), enforce());

    assert_eq!(runtime.open_root_fork(&root(), &root(), &fork, observe()), Ok(()));
    assert_eq!(
        runtime.open_root_fork(&root(), &root(), &fork, SessionSettings::default()),
        Ok(())
    );
    assert_eq!(
        runtime.open_root_fork(&root(), &root(), &fork, enforce()),
        Err(crate::api::RootForkRefusal::LineVariantMismatch)
    );
    assert!(matches!(
        start(&runtime, &fork, enforce()).await,
        HookDecision::Refuse { .. }
    ));

    let session = runtime.session(&fork, &fork).expect("the fork reopens");
    let decision = session
        .on_tool_call(taint(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is judged");
    assert!(matches!(decision, ToolCallDecision::Allow { .. }), "{decision:?}");
}

/// A launched fork start opens its family with the settings the start names.
#[tokio::test]
async fn a_launched_fork_pins_the_settings_its_start_names() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), dir.path().join("appa.db"), None)
        .expect("the deployment opens");
    assert_eq!(
        start(&runtime, &root(), SessionSettings::default()).await,
        HookDecision::Ack
    );
    let fork = root_fork_id("launched-fork");
    let launched = crate::hooks::handle(
        &runtime,
        HookEvent::SessionStart {
            root: fork.clone(),
            principal: None,
            address: None,
            title: None,
            start: Some(StartKind::Fork),
            launch: Some(appa_runtime_api::LaunchStart {
                launch: appa_runtime_api::LaunchToken::parse("0b6c1f5e-8f0a-4a57-9d6e-2f7c3e1a9b10").expect("a token"),
                forked_from: Some(root()),
            }),
            settings: observe(),
        },
    )
    .await;
    assert_eq!(launched, HookDecision::Ack);
    assert_eq!(settings_of(&runtime, &fork), observe());
    assert!(settings_of(&runtime, &root()).is_default());
}

/// The pinned settings are in the log: a runtime reopened on it reads them back and decides
/// under them.
#[tokio::test]
async fn a_restarted_runtime_keeps_the_pinned_settings() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let db = dir.path().join("appa.db");
    {
        let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db.clone(), None).expect("the deployment opens");
        assert_eq!(start(&runtime, &root(), observe()).await, HookDecision::Ack);
    }
    let runtime = Runtime::open(config_with(FETCH_AND_SEND, None), db, None).expect("the deployment reopens");
    assert_eq!(settings_of(&runtime, &root()), observe());
    let session = runtime.session(&root(), &root()).expect("the root reopens");
    let decision = session
        .on_tool_call(taint(serde_json::json!({"a": 1})), false)
        .await
        .expect("the call is judged");
    assert!(matches!(decision, ToolCallDecision::Allow { .. }), "{decision:?}");
    assert!(matches!(
        start(&runtime, &root(), enforce()).await,
        HookDecision::Refuse { .. }
    ));
}

/// `leak` brings an insider, suspicious result. `scrub` clears its audience and leaves its
/// trust, so a result routed through it is staged with that residual narrowing. `vet`, where
/// registered, lifts the trust that remains.
fn staged_config(url: &str, vet: bool) -> Config {
    let vet = match vet {
        true => format!(
            "[[policy.sanitizer]]\nname = \"vet\"\non = [\"tool_output\"]\n[policy.sanitizer.permits]\n\
             trust = {{ from = \"suspicious\", to = \"trusted\" }}\n\n[externals.sanitizers.vet]\nurl = \"{url}\"\n"
        ),
        false => String::new(),
    };
    config_from(&format!(
        "[policy]\n{}\ncontext_control = true\n\n[[policy.tool]]\nname = \"fetch\"\n\n{vet}[externals]\ntimeout_ms = 2000\n\
         max_body_bytes = 65536\n[externals.sanitizers.scrub]\nurl = \"{url}\"\n",
        PARTLY_CLEARED.trim_end()
    ))
}

/// A child whose return its parent floored at `public` and `trust`: no plan accepts the
/// insider audience `leak` would bring, so its call blocks until the agent routes the result
/// through `scrub`, and the result is staged with the trust `scrub` leaves.
async fn staged_in_child(runtime: &Runtime, settings: SessionSettings, trust: &str) -> ToolResultDecision {
    let mut session = runtime
        .create_pinned_session(root(), None, settings)
        .expect("a fresh id opens");
    let child_id = TrajectoryId("cc:child".to_string());
    let child = open_child_floored(
        &mut session,
        fetch(serde_json::json!({})),
        child_id.clone(),
        crate::engine::LabelSpelling {
            trust: Some(trust.to_string()),
            audience: Some(vec!["public".to_string()]),
        },
    )
    .await;
    let blocked = child.on_tool_call(leak(), false).await.expect("the call is judged");
    assert!(matches!(blocked, ToolCallDecision::Deny { .. }), "{blocked:?}");
    sanitized_result(runtime, &child, &child_id).await
}

/// Under `RulingsAndAccept` a staged result whose block offers only the acceptance of its
/// residual narrowing is accepted at the result: the derivation is delivered, and the
/// acceptance is on the record. Under `RulingsOnly` the same stage reaches the agent.
#[tokio::test]
async fn a_staged_narrowing_is_accepted_at_the_result_only_under_rulings_and_accept() {
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    for (settings, accepted) in [(observe(), true), (SessionSettings::default(), false)] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(staged_config(&url, false), dir.path().join("appa.db"), None).expect("the deployment opens");
        let decided = staged_in_child(&runtime, settings.clone(), "suspicious").await;
        let accepted_on_record = runtime
            .audit(&root())
            .expect("the root answers its audit")
            .iter()
            .any(|entry| entry.trajectory == "cc:child" && matches!(entry.event, AuditEvent::Narrowed { .. }));
        match accepted {
            true => {
                assert_eq!(
                    decided,
                    ToolResultDecision::Deliver {
                        value: "scrubbed".to_string()
                    }
                );
                assert!(accepted_on_record);
            }
            false => {
                let ToolResultDecision::Replace {
                    presentation: Some(presentation),
                    ..
                } = decided
                else {
                    panic!("{settings:?}: the stage reaches the agent, got {decided:?}");
                };
                assert_eq!(presentation.offers.len(), 1);
                assert_eq!(
                    presentation.outcomes.map(|outcomes| outcomes.deciding()),
                    Some(Outcome::Consent)
                );
                assert!(!accepted_on_record);
            }
        }
    }
}

/// A staged result whose only route is another sanitizer's hop reaches the agent under
/// `RulingsAndAccept` as under `RulingsOnly`: a hop is the agent's choice.
#[tokio::test]
async fn a_staged_result_whose_only_route_is_a_hop_reaches_the_agent() {
    let url = stub(serde_json::json!({"body": "scrubbed"})).await;
    for settings in [observe(), SessionSettings::default()] {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime =
            Runtime::open(staged_config(&url, true), dir.path().join("appa.db"), None).expect("the deployment opens");
        let decided = staged_in_child(&runtime, settings.clone(), "trusted").await;
        let ToolResultDecision::Replace {
            presentation: Some(presentation),
            ..
        } = decided
        else {
            panic!("{settings:?}: the stage reaches the agent, got {decided:?}");
        };
        assert_eq!(presentation.offers.len(), 1, "{settings:?}: the hop alone");
        assert_eq!(
            presentation.outcomes.map(|outcomes| outcomes.deciding()),
            Some(Outcome::AgentChoice),
            "{settings:?}"
        );
    }
}
