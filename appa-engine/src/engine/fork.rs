use super::*;
use crate::fact::BelowFloor;
use crate::projection::DeferralStanding;
use crate::transition::{ReturnDecision, ReturnSettlement, SettlementFollowUp};

impl Engine {
    pub(super) fn decide_binding(
        &self,
        view: &EngineView,
        binding: &ForkBinding,
    ) -> Result<EngineDecision, TransitionError> {
        let prepared = view
            .projection()
            .prepared_fork(&binding.fork)
            .ok_or(TransitionError::UnbindableFork)?;
        if let (crate::transition::SpawnKind::FanOut, ForkId::Spawn(spawn)) = (prepared.kind, &binding.fork) {
            return self.decide_member_binding(view, spawn, &binding.child);
        }
        let parent = prepared.parent.clone();
        let views = view.projection().view(&parent);
        if let Some(bound) = view.projection().bound_child(&binding.fork) {
            if bound != &binding.child {
                return Err(TransitionError::UnbindableFork);
            }
            // The same pair again is the parent addressing its child once more: the parent's
            // label as it stands now flows into the child, and a derivation the child never
            // echoed is dropped with the errand it belonged to.
            if views.has_ended(&binding.child) {
                return Err(TransitionError::BranchEnded);
            }
            let batch = vec![Fact::Boundary {
                trajectory: binding.child.clone(),
                kind: BoundaryKind::Resume {
                    seed: views.current_label(),
                },
            }];
            return Ok(EngineDecision {
                append: Some(self.decided(view, crate::basis::DecidedAct::Binding(binding.fork.clone()), batch)?),
                follow_up: FollowUp::Fork {
                    child: binding.child.clone(),
                },
            });
        }
        if view.projection().is_opened(&binding.child) {
            return Err(TransitionError::ChildAlreadyUsed);
        }
        // A recorded spawn failure makes the preparation unbindable: no child ran.
        if views.dispatch_failed(binding.fork.dispatch()) {
            return Err(TransitionError::UnbindableFork);
        }
        let batch = vec![Fact::ForkOpened {
            trajectory: binding.child.clone(),
            fork: binding.fork.clone(),
        }];
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Binding(binding.fork.clone()), batch)?),
            follow_up: FollowUp::Fork {
                child: binding.child.clone(),
            },
        })
    }

    /// A fan-out spawn's child starting: its own member fork, prepared now under the fan-out's
    /// return policy and seeded at the parent's label as it stands now, so a child started after
    /// a sibling's return crossed inherits what that return carried. A child already bound to its
    /// member is addressed again, as a single spawn's child is.
    fn decide_member_binding(
        &self,
        view: &EngineView,
        spawn: &DispatchId,
        child: &TrajectoryId,
    ) -> Result<EngineDecision, TransitionError> {
        let member = ForkId::member(spawn, child);
        if view.projection().prepared_fork(&member).is_some() {
            return self.decide_binding(
                view,
                &ForkBinding {
                    fork: member,
                    child: child.clone(),
                },
            );
        }
        let fan_out = view
            .projection()
            .prepared_fork(&ForkId::of(spawn))
            .expect("the caller found this fan-out fork prepared");
        let views = view.projection().view(&fan_out.parent);
        if view.projection().is_opened(child) {
            return Err(TransitionError::ChildAlreadyUsed);
        }
        if views.has_ended(&fan_out.parent) {
            return Err(TransitionError::BranchEnded);
        }
        if views.dispatch_failed(spawn) {
            return Err(TransitionError::UnbindableFork);
        }
        let batch = vec![
            Fact::ForkPrepared {
                trajectory: fan_out.parent.clone(),
                fork: member.clone(),
                snapshot: views.freeze_basis(),
                return_policy: fan_out.return_policy.clone(),
                kind: crate::transition::SpawnKind::Single,
            },
            Fact::ForkOpened {
                trajectory: child.clone(),
                fork: member.clone(),
            },
        ];
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Binding(member), batch)?),
            follow_up: FollowUp::Fork { child: child.clone() },
        })
    }

    /// One child's return, judged against the policy its fork recorded. A void submission ends
    /// the branch. A value crosses at the child's fold, or through the fork's sanitizer: an
    /// attested body crosses in canonical form; a named sanitizer derives the submission once,
    /// stages the derivation, and crosses it when the child returns exactly it. A return the
    /// host names by its words alone, repeating the latest crossing or a deferral of its own
    /// kind, is a replay and appends nothing; one it names by an operation is decided afresh,
    /// the runtime having answered its retries from the operation. A raw return
    /// below the floor the parent set is held where the fork holds it, and otherwise refused:
    /// the child keeps it.
    pub(super) fn decide_child_return(
        &self,
        view: &EngineView,
        report: &ChildReport,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        let projection = view.projection();
        let parent = projection
            .view(child)
            .parent_of(child)
            .ok_or(TransitionError::NotForked)?
            .clone();
        let views = projection.view(&parent);
        // Every return addresses the exact fork that opened its child.
        if views.fork_of(child) != Some(&report.fork) {
            return Err(TransitionError::ReturnForkMismatch);
        }
        if views.has_ended(child) {
            return Err(TransitionError::BranchEnded);
        }
        let body = match &report.submission {
            ChildSubmission::Void => {
                let batch = branch::submit_void_return(&views, child).map_err(branch_refusal)?;
                return Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), batch)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Ended),
                });
            }
            ChildSubmission::Value { body } => body.clone(),
        };
        // The words of the latest return, made again as a return of its kind, are that return
        // again: a turn repeats a held turn, and a stop never reuses one.
        if let Some((id, _)) = views
            .latest_deferral(child)
            .filter(|(_, deferral)| report.kind.replays_by_words() && deferral.held == report.kind.holds())
            .filter(|(_, deferral)| deferral.value.body == body)
        {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Deferred { id }),
            });
        }
        let fold = views.branch_label(child);
        let policy = match views.return_policy_of(child).ok_or(TransitionError::NotForked)?.clone() {
            ReturnPolicy::Void => return Err(TransitionError::ReturnVoid),
            ReturnPolicy::Deferred => {
                return self.decide_deferred_return(view, &views, report, LabeledValue::new(body, fold), act);
            }
            ReturnPolicy::Crossing(policy) => policy,
        };
        let Some(floor) = plan::floor_of(&self.registry, &projection.view(child)) else {
            return Err(TransitionError::NotForked);
        };
        let replay = |admitted: &LabeledValue| EngineDecision {
            append: None,
            follow_up: FollowUp::Child(ChildFollowUp::Merged {
                admitted: admitted.body.clone(),
            }),
        };
        let above_floor = |label: &Label| match floor.holds(label) {
            true => Ok(()),
            false => Err(TransitionError::ReturnBelowFloor {
                floor: floor.label().clone(),
            }),
        };
        // An undecided mandate is the runtime's ask, never a refusal.
        let sanitized = |registered: &crate::authority::Sanitizer| -> Result<Label, TransitionError> {
            let label = registered
                .derive_output(&fold, &[], &self.context(act))?
                .ok_or(TransitionError::SanitizerUnapplicable)?;
            above_floor(&label)?;
            Ok(label)
        };
        let cross = |mut facts: Vec<Fact>,
                     value: LabeledValue,
                     derivation: ReturnDerivation|
         -> Result<EngineDecision, TransitionError> {
            let admitted = value.body.clone();
            facts.extend(branch::crossing_facts(
                &views,
                next_return(&views, child),
                value,
                derivation,
                act.pinned().clone(),
            ));
            Ok(EngineDecision {
                append: Some(self.decided(view, return_act(&views, child), facts)?),
                follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted }),
            })
        };
        match &policy.sanitizer {
            None => {
                if let Some(latest) = repeated_crossing(&views, report, &body) {
                    return Ok(replay(latest));
                }
                match (above_floor(&fold), policy.below_floor) {
                    (Ok(()), _) => cross(Vec::new(), LabeledValue::new(body, fold), ReturnDerivation::Raw),
                    (Err(TransitionError::ReturnBelowFloor { .. }), BelowFloor::Hold) => {
                        self.deferral(view, &views, report, LabeledValue::new(body, fold))
                    }
                    (Err(refused), _) => Err(refused),
                }
            }
            Some(ReturnSanitizer::Attest(shape)) => {
                let canonical = ValueBody::new(
                    shape
                        .validate(body.as_str())
                        .map_err(TransitionError::ReturnShapeMismatch)?,
                );
                if let Some(latest) = repeated_crossing(&views, report, &canonical) {
                    return Ok(replay(latest));
                }
                let name = SanitizerName::new(SanitizerName::ATTEST_SCHEMA);
                let registered = self
                    .registry
                    .sanitizer(&name)
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                if !views
                    .fork_seed(child)
                    .is_some_and(|seed| plan::attest_fits(&registered.transition, seed))
                {
                    return Err(TransitionError::SanitizerUnapplicable);
                }
                let label = sanitized(registered)?;
                cross(
                    Vec::new(),
                    LabeledValue::new(canonical, label),
                    ReturnDerivation::Sanitized {
                        sanitizer: name,
                        raw_digest: RawResultDigest::of(body.as_str().as_bytes()),
                    },
                )
            }
            Some(ReturnSanitizer::Named(name)) => {
                let registered = self
                    .registry
                    .sanitizer(name)
                    .ok_or(TransitionError::SanitizerUnapplicable)?;
                let subject = crate::basis::SubjectKey::Return(next_return(&views, child));
                if let Some(DerivedCandidate::Return { source, value }) = views.candidate(&subject) {
                    // A derivation stands: it crosses when the child says exactly it, and the
                    // sanitizer never runs again for this errand.
                    if value.body != body {
                        return Ok(EngineDecision {
                            append: None,
                            follow_up: FollowUp::Child(ChildFollowUp::Staged {
                                derived: value.body.clone(),
                            }),
                        });
                    }
                    return cross(
                        Vec::new(),
                        value.clone(),
                        ReturnDerivation::Sanitized {
                            sanitizer: name.clone(),
                            raw_digest: *source,
                        },
                    );
                }
                if let Some(latest) = repeated_crossing(&views, report, &body) {
                    return Ok(replay(latest));
                }
                let digest = RawResultDigest::of(body.as_str().as_bytes());
                let Some(derived) = sanitizer_evidence(&report.evidence, name, digest) else {
                    return Ok(EngineDecision {
                        append: None,
                        follow_up: FollowUp::Child(ChildFollowUp::Resolve(EvidenceRequest::Sanitizer {
                            sanitizer: name.clone(),
                            source: digest,
                            body,
                        })),
                    });
                };
                let value = LabeledValue::new(derived.clone(), sanitized(registered)?);
                let facts = vec![Fact::CandidateDerived {
                    trajectory: parent.clone(),
                    subject,
                    sanitizer: name.clone(),
                    derived: DerivedCandidate::Return {
                        source: digest,
                        value: value.clone(),
                    },
                    lineage: SanitizerLineage::default()
                        .extend(name.clone())
                        .expect("an empty lineage spends no sanitizer yet"),
                    evidence: act.pinned().clone(),
                }];
                // A derivation equal to the submission needs no echo: it crosses now.
                if derived == body {
                    return cross(
                        facts,
                        value,
                        ReturnDerivation::Sanitized {
                            sanitizer: name.clone(),
                            raw_digest: digest,
                        },
                    );
                }
                Ok(EngineDecision {
                    append: Some(self.decided(view, return_act(&views, child), facts)?),
                    follow_up: FollowUp::Child(ChildFollowUp::Staged { derived }),
                })
            }
        }
    }
}

impl Engine {
    /// A return under a deferred return. A value that leaves the parent's label where it is
    /// crosses now, as a value under a floor does. One that would narrow it is deferred.
    fn decide_deferred_return(
        &self,
        view: &EngineView,
        views: &Views,
        report: &ChildReport,
        value: LabeledValue,
        act: &ActEvidence,
    ) -> Result<EngineDecision, TransitionError> {
        let child = &report.child;
        if let Some(latest) = repeated_crossing(views, report, &value.body) {
            return Ok(EngineDecision {
                append: None,
                follow_up: FollowUp::Child(ChildFollowUp::Merged {
                    admitted: latest.body.clone(),
                }),
            });
        }
        if let Some(floor) = plan::floor_of(&self.registry, &view.projection().view(child))
            && !floor.holds(&value.label)
        {
            return Err(TransitionError::ReturnBelowFloor {
                floor: floor.label().clone(),
            });
        }
        let standing = views.branch_label(views.trajectory());
        if standing.combine(&value.label) != standing {
            return self.deferral(view, views, report, value);
        }
        let admitted = value.body.clone();
        let facts = branch::crossing_facts(
            views,
            next_return(views, child),
            value,
            ReturnDerivation::Raw,
            act.pinned().clone(),
        );
        Ok(EngineDecision {
            append: Some(self.decided(view, return_act(views, child), facts)?),
            follow_up: FollowUp::Child(ChildFollowUp::Merged { admitted }),
        })
    }

    /// The value frozen at the child's next occurrence: nothing crosses until the parent
    /// accepts it. At the stop it ends the child's errand; a return made without ending holds it
    /// while the child runs on.
    fn deferral(
        &self,
        view: &EngineView,
        views: &Views,
        report: &ChildReport,
        value: LabeledValue,
    ) -> Result<EngineDecision, TransitionError> {
        let id = next_return(views, &report.child);
        let facts = vec![Fact::ReturnDeferred {
            trajectory: report.child.clone(),
            id: id.clone(),
            value,
            held: report.kind.holds(),
        }];
        Ok(EngineDecision {
            append: Some(self.decided(view, return_act(views, &report.child), facts)?),
            follow_up: FollowUp::Child(ChildFollowUp::Deferred { id }),
        })
    }

    /// The parent's decision on a deferred return. Accepting crosses the frozen value under the
    /// narrowing it causes at the parent's label now, which the parent's own floor must hold;
    /// discarding crosses nothing. A settled return answers its recorded outcome and appends
    /// nothing, whatever the repeat asks for.
    pub(super) fn decide_settlement(
        &self,
        view: &EngineView,
        settlement: &ReturnSettlement,
    ) -> Result<EngineDecision, TransitionError> {
        let parent = &settlement.trajectory;
        let id = &settlement.id;
        let views = view.projection().view(parent);
        if views.parent_of(id.child()) != Some(parent) {
            return Err(TransitionError::NoDeferredReturn);
        }
        let settled = |follow_up: SettlementFollowUp| EngineDecision {
            append: None,
            follow_up: FollowUp::Settlement(follow_up),
        };
        let deferral = views.deferral(id).ok_or(TransitionError::NoDeferredReturn)?;
        let frozen = match deferral.standing {
            // A pending return never crosses into a parent that has ended.
            DeferralStanding::Pending if views.has_ended(parent) => return Err(TransitionError::BranchEnded),
            DeferralStanding::Pending => deferral.value.clone(),
            DeferralStanding::Discarded => return Ok(settled(SettlementFollowUp::Discarded)),
            DeferralStanding::Accepted => {
                let admitted = views
                    .child_return(id)
                    .expect("an accepted deferred return crossed in the batch that accepted it")
                    .body
                    .clone();
                return Ok(settled(SettlementFollowUp::Accepted { admitted }));
            }
        };
        let (facts, follow_up) = match settlement.decision {
            ReturnDecision::Discard => (
                vec![Fact::ReturnDiscarded {
                    trajectory: parent.clone(),
                    id: id.clone(),
                }],
                SettlementFollowUp::Discarded,
            ),
            ReturnDecision::Accept => {
                let from = views.branch_label(parent);
                let to = from.combine(&frozen.label);
                if let Some(floor) = plan::floor_of(&self.registry, &views)
                    && !floor.holds(&to)
                {
                    return Err(TransitionError::ReturnBelowFloor {
                        floor: floor.label().clone(),
                    });
                }
                let admitted = frozen.body.clone();
                let mut facts = vec![Fact::ReturnAccepted {
                    trajectory: parent.clone(),
                    id: id.clone(),
                    narrowing: Narrowing { from, to },
                }];
                facts.extend(branch::crossing_facts(
                    &views,
                    id.clone(),
                    frozen,
                    ReturnDerivation::Raw,
                    AudienceEvidence::default(),
                ));
                (facts, SettlementFollowUp::Accepted { admitted })
            }
        };
        Ok(EngineDecision {
            append: Some(self.decided(view, crate::basis::DecidedAct::Settlement(id.clone()), facts)?),
            follow_up: FollowUp::Settlement(follow_up),
        })
    }
}

/// The crossing `report` repeats, where its words may stand for the child's latest return.
fn repeated_crossing<'v>(views: &'v Views, report: &ChildReport, body: &ValueBody) -> Option<&'v LabeledValue> {
    match report.kind.replays_by_words() {
        true => views.latest_return(&report.child).filter(|latest| latest.body == *body),
        false => None,
    }
}

/// The identity `child`'s next return takes, crossed or deferred.
fn next_return(views: &Views, child: &TrajectoryId) -> ChildReturnId {
    ChildReturnId::new(child.clone(), views.returns_by(child))
}

/// The act one child's return lands under: its next occurrence's identity.
fn return_act(views: &Views, child: &TrajectoryId) -> crate::basis::DecidedAct {
    crate::basis::DecidedAct::ChildReturn(next_return(views, child))
}

fn branch_refusal(error: BranchError) -> TransitionError {
    match error {
        BranchError::NotDirectParent | BranchError::NotForked => TransitionError::NotForked,
        BranchError::AlreadyEnded => TransitionError::BranchEnded,
    }
}
