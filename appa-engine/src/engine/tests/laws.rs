use super::*;

mod admission_law {
    use super::*;
    use proptest::prelude::*;

    /// Provider-run tools whose results meet the fold at the identity or narrow one known
    /// dimension. A pending dimension is not a provider-run construct; that case is the
    /// projection's own unit test.
    const TOOLS: [&str; 3] = ["identity", "suspicious", "insider"];

    fn labeled_tool(name: &str) -> ToolAnnotation {
        let mut tool = plain_tool(name);
        tool.delta = match name {
            "identity" => Delta::NONE,
            "suspicious" => Delta {
                trust: Some(Trust::new(0)),
                audience: None,
            },
            "insider" => Delta {
                trust: None,
                audience: Some(DeltaAudience::Static(DeclaredAudience::restricted([ReaderId::new(
                    "insider",
                )]))),
            },
            other => panic!("no labeled tool named {other}"),
        };
        tool
    }

    proptest! {
        /// Effect-free admissions move neither family nor subject, so across them an open
        /// offer stays current exactly while the trajectory's partial label — bound and
        /// pending sources alike — stays where the offer found it.
        #[test]
        fn an_offer_outlives_exactly_the_admissions_that_leave_the_label_unchanged(
            admitted in prop::collection::vec(0usize..TOOLS.len(), 0..4),
        ) {
            let mut tools = vec![crm_tool()];
            tools.extend(TOOLS.iter().map(|name| labeled_tool(name)));
            let e = engine_with_provider_run(tools, &TOOLS);
            let log = opening_log(&e);
            let opened = appended_facts(blocked_batch(&e, &log, "b1", nonce()));
            let offer = opened_offers(&opened)[0].0;
            let mut log = [log, opened].concat();
            let label_of = |log: &[Fact]| {
                e.view(&traj(), log.to_vec(), log.len() as u64)
                    .expect("the log replays")
                    .views(&traj())
                    .expect("the root is opened")
                    .current_label()
            };
            let at_open = label_of(&log);
            for (i, tool) in admitted.iter().enumerate() {
                let decision = e
                    .handle(
                        &viewing(&e, &log),
                        batch(&format!("b{}", i + 2), vec![exposed(TOOLS[*tool], "a body")], Vec::new()),
                    )
                    .expect("an admission-only batch decides");
                log.extend(appended_facts(decision));
            }
            let now = label_of(&log);
            let stale = matches!(
                execute_offer(&e, &log, offer, OfferOutcome::Approved(Vec::new())),
                Err(TransitionError::StaleOffer)
            );
            prop_assert_eq!(stale, now != at_open);
        }
    }
}

/// A held view advanced by the batches the engine sealed stays the view a cold replay of the
/// same log builds, at every prefix: the runtime may keep one view per root and advance it
/// with its own appends instead of replaying the whole log each turn.
mod held_view_law {
    use super::*;
    use crate::value::{ForkId, OfferId};
    use proptest::prelude::*;

    const EXPOSED: [&str; 2] = ["seen", "suspicious"];
    const PROPOSED: [&str; 10] = [
        "quiet",
        "emit",
        "wire",
        "guard",
        "strict",
        "get_ticket",
        "taint",
        "spawn",
        "read_file",
        "strict_read",
    ];
    /// Proposed with a host-pinned file basis, so their failures carry a body.
    const FILE_TOOLS: [&str; 2] = ["read_file", "strict_read"];

    fn file_basis() -> crate::value::FileBasis {
        crate::value::FileBasis::Read(crate::value::FileSource {
            version: "v1".into(),
            digest: "content".into(),
            label: Label::new(SUSPICIOUS, Audience::public()),
        })
    }

    #[derive(Clone, Debug)]
    enum Step {
        Propose {
            on: usize,
            exposed: Vec<usize>,
            proposed: Vec<usize>,
        },
        Spawn {
            on: usize,
        },
        Outcome {
            pick: usize,
            outcome: usize,
        },
        Offer {
            pick: usize,
            approve: bool,
        },
        Bind {
            pick: usize,
        },
        Return {
            pick: usize,
            value: bool,
        },
        HostOnly,
    }

    fn step() -> impl Strategy<Value = Step> {
        let index = || 0usize..8;
        prop_oneof![
            3 => (
                index(),
                prop::collection::vec(0..EXPOSED.len(), 0..2),
                prop::collection::vec(0..PROPOSED.len(), 0..3),
            )
                .prop_map(|(on, exposed, proposed)| Step::Propose { on, exposed, proposed }),
            2 => index().prop_map(|on| Step::Spawn { on }),
            3 => (index(), 0usize..6).prop_map(|(pick, outcome)| Step::Outcome { pick, outcome }),
            3 => (index(), any::<bool>()).prop_map(|(pick, approve)| Step::Offer { pick, approve }),
            2 => index().prop_map(|pick| Step::Bind { pick }),
            2 => (index(), any::<bool>()).prop_map(|(pick, value)| Step::Return { pick, value }),
            1 => Just(Step::HostOnly),
        ]
    }

    /// Provider-run results that raise effects or lower trust, calls gated on history and on
    /// trust, one officer who can clear either gap, and marked spawns: blocks, offers,
    /// approvals, denials, dispatch outcomes, forks and child returns all arise.
    fn law_engine() -> Engine {
        let k = || EffectSet::new([EffectKind::new("k")]).unwrap();
        let lowering = Delta {
            trust: Some(SUSPICIOUS),
            audience: None,
        };
        let mut seen = plain_tool("seen");
        seen.emits = k();
        let mut suspicious = plain_tool("suspicious");
        suspicious.delta = lowering.clone();
        let mut emit = plain_tool("emit");
        emit.emits = k();
        let mut wire = plain_tool("wire");
        wire.requires.history = vec![HistoryRequirement::Prior(EffectKind::new("k"))];
        let mut guard = plain_tool("guard");
        guard.requires.history = vec![HistoryRequirement::NoPrior(EffectKind::new("k"))];
        let mut strict = plain_tool("strict");
        strict.requires.label.trust_floor = Some(TRUSTED);
        let mut taint = plain_tool("taint");
        taint.delta = lowering;
        let mut strict_read = plain_tool("strict_read");
        strict_read.requires.label.trust_floor = Some(TRUSTED);
        let officer = crate::authority::Authority {
            name: AuthorityName::new("officer"),
            mandate: crate::authority::Mandate {
                trust_ceiling: Some(TRUSTED),
                waivers: vec![EffectKind::new("k")],
                ..crate::authority::Mandate::default()
            },
            scope: crate::authority::Scope::default(),
            hint: None,
        };
        provider_run_engine(
            RegistryConfig {
                authorities: vec![officer],
                ..test_config(vec![
                    seen,
                    suspicious,
                    plain_tool("quiet"),
                    emit,
                    wire,
                    guard,
                    strict,
                    crm_tool(),
                    taint,
                    plain_tool("spawn"),
                    plain_tool("read_file"),
                    strict_read,
                ])
            },
            &EXPOSED,
        )
    }

    /// One generated trajectory: the log the store holds, the view held beside it, and the
    /// identities the log's facts surfaced for later steps to pick from.
    struct Walk {
        engine: Engine,
        log: Vec<Fact>,
        batches: u64,
        held: EngineView,
        trajectories: Vec<TrajectoryId>,
        dispatches: Vec<DispatchId>,
        file_dispatches: Vec<DispatchId>,
        offers: Vec<(TrajectoryId, OfferId)>,
        forks: Vec<ForkId>,
        children: Vec<(TrajectoryId, ForkId)>,
        acts: usize,
    }

    impl Walk {
        fn open() -> Walk {
            let engine = law_engine();
            let opening = engine
                .open_trajectory(&traj(), crate::profile::PolicyFileKey::of(b"law"), None)
                .expect("the opening validates against the empty log");
            let log = opening.into_unsealed();
            let held = engine.view(&traj(), log.clone(), 1).expect("the opening replays");
            Walk {
                engine,
                log,
                batches: 1,
                held,
                trajectories: vec![traj()],
                dispatches: Vec::new(),
                file_dispatches: Vec::new(),
                offers: Vec::new(),
                forks: Vec::new(),
                children: Vec::new(),
                acts: 0,
            }
        }

        fn cold(&self) -> EngineView {
            self.engine
                .view(&traj(), self.log.clone(), self.batches)
                .expect("every prefix the engine sealed replays")
        }

        fn next_id(&mut self) -> String {
            self.acts += 1;
            format!("law-{}", self.acts)
        }

        /// The event a step names against the log so far; `None` when it names nothing
        /// the log has surfaced yet.
        fn event(&mut self, step: &Step) -> Option<EngineEvent> {
            let pick = |len: usize, at: usize| (len > 0).then(|| len - 1 - at % len);
            match step {
                Step::Propose { on, exposed, proposed } => {
                    let on = self.trajectories[on % self.trajectories.len()].clone();
                    let id = self.next_id();
                    let exposed = exposed
                        .iter()
                        .map(|tool| super::exposed(EXPOSED[*tool], "body"))
                        .collect();
                    let evidence = proposed
                        .iter()
                        .enumerate()
                        .filter(|(_, tool)| FILE_TOOLS.contains(&PROPOSED[**tool]))
                        .map(|(position, _)| Evidence::File {
                            position,
                            basis: file_basis(),
                        })
                        .collect();
                    let proposed = proposed
                        .iter()
                        .map(|tool| raw(&call(PROPOSED[*tool], json!({}))))
                        .collect();
                    Some(EngineEvent::Proposals(ProposalBatch {
                        id: crate::transition::ProposalBatchId::new(id),
                        trajectory: on,
                        provider_results: exposed,
                        proposals: proposed,
                        spawn: None,
                        offer_nonce: nonce(),
                        evidence,
                        audience: crate::audience::AudienceEvidence::default(),
                    }))
                }
                Step::Spawn { on } => {
                    let on = self.trajectories[on % self.trajectories.len()].clone();
                    let id = self.next_id();
                    let spawn = raw(&call("spawn", json!({})));
                    Some(batch_on(&on, &id, Vec::new(), vec![spawn], Some(SpawnMark::at(0))))
                }
                Step::Outcome { pick: at, outcome } => {
                    // A failure body on a file call is admitted; on any other call it is refused.
                    let from = match (outcome, self.file_dispatches.is_empty()) {
                        (3, false) => &self.file_dispatches,
                        _ => &self.dispatches,
                    };
                    let dispatch = from[pick(from.len(), *at)?].clone();
                    let outcome = match outcome {
                        0 => ToolOutcome::Success {
                            body: OutcomeBody::Available(ValueBody::new("result")),
                        },
                        1 => ToolOutcome::Success {
                            body: OutcomeBody::Unavailable,
                        },
                        2 => ToolOutcome::Failure,
                        3 | 4 => ToolOutcome::FailureWithBody {
                            body: ValueBody::new("error"),
                        },
                        _ => ToolOutcome::Indeterminate,
                    };
                    Some(EngineEvent::Outcome(ToolReport {
                        dispatch,
                        outcome,
                        evidence: Vec::new(),
                        offer_nonce: nonce(),
                        audience: crate::audience::AudienceEvidence::default(),
                    }))
                }
                Step::Offer { pick: at, approve } => {
                    let (trajectory, offer) = self.offers[pick(self.offers.len(), *at)?].clone();
                    let views = self.held.views(&trajectory)?;
                    let recorded = views.offer(&offer)?;
                    let tool = views.standing_call(&recorded.subject)?.tool().as_str().to_string();
                    let plan = recorded.plan.clone();
                    let outcome = match approve {
                        true => OfferOutcome::Approved(evidence_for(offer, &plan, &tool, views.current_label())),
                        false => OfferOutcome::Denied {
                            authority: AuthorityName::new("officer"),
                        },
                    };
                    let return_policy = plan.return_step().map(|_| {
                        ReturnPolicy::Crossing(crate::fact::CrossingReturn {
                            floor: views.current_label(),
                            sanitizer: None,
                        })
                    });
                    Some(EngineEvent::ExecuteOffer(OfferExecution {
                        trajectory,
                        offer,
                        outcome,
                        return_policy,
                        offer_nonce: nonce(),
                        audience: crate::audience::AudienceEvidence::default(),
                    }))
                }
                Step::Bind { pick: at } => {
                    let fork = self.forks[pick(self.forks.len(), *at)?].clone();
                    let child = TrajectoryId::new(format!("child-{}", self.next_id()));
                    Some(EngineEvent::BindFork(ForkBinding { fork, child }))
                }
                Step::Return { pick: at, value } => {
                    let (child, fork) = self.children[pick(self.children.len(), *at)?].clone();
                    let submission = match value {
                        true => ChildSubmission::Value {
                            body: ValueBody::new("child says"),
                        },
                        false => ChildSubmission::Void,
                    };
                    Some(EngineEvent::ChildReturn(ChildReport {
                        child,
                        fork,
                        submission,
                        evidence: Vec::new(),
                        audience: crate::audience::AudienceEvidence::default(),
                    }))
                }
                Step::HostOnly => None,
            }
        }

        /// Append one batch the way the store does, advance the held view by it, and compare
        /// against a cold replay of the new log.
        fn append(&mut self, batch: &ValidatedFactBatch) -> Result<(), TestCaseError> {
            self.held
                .advance(batch)
                .expect("the batch was sealed at the held view's position");
            self.log.extend(batch.facts().iter().cloned());
            self.batches += 1;
            prop_assert_eq!(&self.held, &self.cold());
            for fact in batch.facts() {
                match fact {
                    Fact::DispatchOpened { dispatch, tool, .. } => {
                        self.dispatches.push(dispatch.clone());
                        if FILE_TOOLS.contains(&tool.as_str()) {
                            self.file_dispatches.push(dispatch.clone());
                        }
                    }
                    Fact::OfferOpened { trajectory, offer, .. } => self.offers.push((trajectory.clone(), *offer)),
                    Fact::ForkPrepared { fork, .. } => self.forks.push(fork.clone()),
                    Fact::ForkOpened { trajectory, fork } => {
                        self.trajectories.push(trajectory.clone());
                        self.children.push((trajectory.clone(), fork.clone()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }

        fn take(&mut self, step: &Step) -> Result<(), TestCaseError> {
            if let Step::HostOnly = step {
                let before = self.held.position();
                let empty = self
                    .engine
                    .seal(&self.held, Vec::new())
                    .expect("no facts pass the validator at an opened view");
                self.append(&empty)?;
                prop_assert_eq!(self.held.position(), before + 1);
                return Ok(());
            }
            let Some(event) = self.event(step) else {
                return Ok(());
            };
            match self.engine.handle(&self.held, event) {
                Ok(EngineDecision {
                    append: Some(batch), ..
                }) => self.append(&batch),
                Ok(EngineDecision { append: None, .. }) | Err(_) => Ok(()),
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn a_view_advanced_by_its_sealed_batches_is_the_cold_replay_at_every_prefix(
            steps in prop::collection::vec(step(), 1..48),
        ) {
            let mut walk = Walk::open();
            for step in &steps {
                walk.take(step)?;
            }
        }
    }

    /// The generator is not vacuous: one fixed walk reaches every kind of act the law
    /// claims to cover.
    #[test]
    fn the_law_walk_reaches_offers_approvals_outcomes_forks_and_returns() {
        let script = [
            Step::Propose {
                on: 0,
                exposed: vec![1],
                proposed: vec![0, 4],
            },
            Step::Offer { pick: 0, approve: true },
            Step::Propose {
                on: 0,
                exposed: vec![],
                proposed: vec![4],
            },
            Step::Outcome { pick: 0, outcome: 0 },
            Step::Outcome { pick: 1, outcome: 0 },
            Step::HostOnly,
            Step::Spawn { on: 0 },
            Step::Offer { pick: 1, approve: true },
            Step::Offer { pick: 2, approve: true },
            Step::Spawn { on: 0 },
            Step::Bind { pick: 0 },
            Step::Return { pick: 0, value: true },
            Step::Propose {
                on: 0,
                exposed: vec![],
                proposed: vec![9],
            },
            Step::Offer { pick: 0, approve: true },
            Step::Propose {
                on: 0,
                exposed: vec![],
                proposed: vec![9],
            },
            Step::Outcome { pick: 0, outcome: 3 },
            Step::Propose {
                on: 0,
                exposed: vec![],
                proposed: vec![4],
            },
            Step::Offer {
                pick: 0,
                approve: false,
            },
        ];
        let mut walk = Walk::open();
        for step in &script {
            walk.take(step).expect("the held view is the cold replay");
        }
        let reached = |kind: fn(&Fact) -> bool| walk.log.iter().any(kind);
        assert!(reached(|fact| matches!(fact, Fact::OfferOpened { .. })));
        assert!(reached(|fact| matches!(fact, Fact::CallApproved { .. })));
        assert!(reached(|fact| matches!(fact, Fact::DispatchClosed { .. })));
        assert!(reached(|fact| matches!(fact, Fact::ForkOpened { .. })));
        assert!(reached(|fact| matches!(fact, Fact::ChildReturn { .. })));
        assert!(reached(|fact| matches!(
            fact,
            Fact::DispatchClosed {
                outcome: crate::fact::CloseOutcome::FailureWithBody { .. },
                ..
            }
        )));
        assert!(reached(|fact| matches!(fact, Fact::OfferDenied { .. })));
    }
}
