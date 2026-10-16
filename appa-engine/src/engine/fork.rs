use super::*;

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

    /// One child's stop, judged against the policy its fork recorded. A void submission ends
    /// the branch. A value crosses at the child's fold, or through the fork's sanitizer: an
    /// attested body crosses in canonical form; a named sanitizer derives the submission once,
    /// stages the derivation, and crosses it when the child returns exactly it. A stop repeating
    /// the latest crossing is a replay and appends nothing. A return below the floor the parent
    /// set is refused, and the child keeps it.
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
        let ReturnPolicy::Crossing(policy) = views.return_policy_of(child).ok_or(TransitionError::NotForked)?.clone()
        else {
            return Err(TransitionError::ReturnVoid);
        };
        let floor = plan::floor_of(&self.registry, &projection.view(child))
            .expect("a forked child stands under its fork's floor");
        let fold = views.branch_label(child);
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
                child,
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
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == body) {
                    return Ok(replay(latest));
                }
                above_floor(&fold)?;
                cross(Vec::new(), LabeledValue::new(body, fold), ReturnDerivation::Raw)
            }
            Some(ReturnSanitizer::Attest(shape)) => {
                let canonical = ValueBody::new(
                    shape
                        .validate(body.as_str())
                        .map_err(TransitionError::ReturnShapeMismatch)?,
                );
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == canonical) {
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
                let id = ChildReturnId::new(child.clone(), views.returns_by(child));
                let subject = crate::basis::SubjectKey::Return(id);
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
                if let Some(latest) = views.latest_return(child).filter(|latest| latest.body == body) {
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

/// The act one child's stop lands under: its next crossing's identity.
fn return_act(views: &Views, child: &TrajectoryId) -> crate::basis::DecidedAct {
    crate::basis::DecidedAct::ChildReturn(ChildReturnId::new(child.clone(), views.returns_by(child)))
}

fn branch_refusal(error: BranchError) -> TransitionError {
    match error {
        BranchError::NotDirectParent | BranchError::NotForked => TransitionError::NotForked,
        BranchError::AlreadyEnded => TransitionError::BranchEnded,
    }
}
