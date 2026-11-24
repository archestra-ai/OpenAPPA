use super::*;
use appa_runtime_api::Initiator;

pub(super) fn remedy_presentation(
    feedback: String,
    outcomes: Option<appa_runtime_api::Outcomes>,
    offers: Vec<appa_runtime_api::OfferedRemedy>,
    pending: Vec<PendingReview>,
    display: Vec<crate::api::RemedyDisplay>,
    externals: &crate::external::ExternalServices,
) -> RemedyPresentation {
    RemedyPresentation {
        feedback,
        outcomes,
        offers,
        review: reviews(&pending, externals),
        display,
    }
}

/// The reviews a harness with its own channel shows: one per offer whose plan consults a
/// `hitl` authority, the texts of several such authorities joined. A joined review names no
/// one authority, hint or scope, so those read as unknown and a host shows the whole text.
/// Authorities of every other backend answer themselves and need no person here.
pub(in crate::api) fn reviews(
    pending_reviews: &[PendingReview],
    externals: &crate::external::ExternalServices,
) -> Vec<appa_runtime_api::Review> {
    let mut reviews: Vec<appa_runtime_api::Review> = Vec::new();
    for pending in pending_reviews {
        if !externals.is_hitl(&pending.authority) {
            continue;
        }
        match reviews.iter_mut().find(|review| review.offer == pending.offer.0) {
            Some(review) => {
                review.text.push_str("\n\n");
                review.text.push_str(&pending.text);
                review.authority = None;
                review.hint = None;
                review.scope = None;
            }
            None => reviews.push(appa_runtime_api::Review {
                offer: pending.offer.0.clone(),
                text: pending.text.clone(),
                authority: Some(pending.authority.clone()),
                hint: pending.hint.clone(),
                scope: Some(pending.scope.clone()),
            }),
        }
    }
    reviews
}

/// The return and the decision a reference names, where `owner` owns that return.
pub(in crate::api) fn referenced_return(
    context: &Decided<'_>,
    owner: &TrajectoryId,
    reference: &ReturnReference,
) -> Option<(appa_engine::value::ChildReturnId, ReturnDecision)> {
    let (id, decision) = context.log.settlement(&reference.digest())?;
    context
        .engine()
        .deferred_return(context.view, owner, id)
        .map(|_| (id.clone(), decision))
}

impl Session {
    pub(in crate::api) fn owns_return(&self, reference: &ReturnReference) -> Result<bool, EventError> {
        self.with_view(|context| Ok(referenced_return(context, &self.trajectory, reference).is_some()))
    }

    pub(in crate::api) fn deferred_return(
        &self,
        call_id: &str,
    ) -> Result<Option<crate::api::DeferredReturnNotice>, EventError> {
        self.with_view(|context| {
            let deferred = context.spawn_dispatch(call_id).and_then(|dispatch| {
                context
                    .engine()
                    .deferred_return_of(context.view, &self.trajectory, &dispatch)
            });
            Ok(deferred.map(|deferred| self.notice(context, &deferred)))
        })
    }

    pub(in crate::api) fn child_occurrences(
        &self,
        call_id: &str,
    ) -> Result<Vec<crate::api::ChildOccurrence>, EventError> {
        self.with_view(|context| {
            let Some(dispatch) = context.spawn_dispatch(call_id) else {
                return Ok(Vec::new());
            };
            let frozen = context
                .engine()
                .child_occurrences(context.view, &self.trajectory, &dispatch);
            let ended = context.engine().liveness(context.view, &self.trajectory) == Liveness::Ended;
            Ok(frozen
                .iter()
                .map(|deferred| {
                    let state = match (deferred.settled, ended) {
                        (None, true) => crate::api::OccurrenceState::ParentEnded,
                        (None, false) => crate::api::OccurrenceState::Pending,
                        (Some(ReturnDecision::Accept), _) => crate::api::OccurrenceState::Accepted,
                        (Some(ReturnDecision::Discard), _) => crate::api::OccurrenceState::Discarded,
                    };
                    crate::api::ChildOccurrence {
                        occurrence: deferred.id.occurrence(),
                        state,
                        operation: context.log.held_operation(&deferred.id).cloned(),
                        notice: (state != crate::api::OccurrenceState::ParentEnded)
                            .then(|| self.notice(context, deferred)),
                    }
                })
                .collect())
        })
    }

    fn notice(
        &self,
        context: &Decided<'_>,
        deferred: &crate::engine::DeferredReturn,
    ) -> crate::api::DeferredReturnNotice {
        crate::api::DeferredReturnNotice {
            feedback: context.engine().deferral_notice(deferred, &self.presentation),
            accept: ReturnReference::of(&deferred.id, ReturnDecision::Accept),
            discard: ReturnReference::of(&deferred.id, ReturnDecision::Discard),
        }
    }

    pub(in crate::api) fn defers_return(&self, call_id: &str) -> Result<bool, EventError> {
        self.with_view(|context| {
            Ok(context.spawn_dispatch(call_id).is_some_and(|dispatch| {
                context
                    .engine()
                    .defers_return(context.view, &self.trajectory, &dispatch)
            }))
        })
    }

    /// This trajectory's decision on a deferred return it owns, named by reference. The
    /// ownership is read in the step that appends the decision.
    pub(in crate::api) async fn on_settle(
        &self,
        reference: &ReturnReference,
    ) -> Result<SettlementDecision, EventError> {
        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, _| {
                    let (id, decision) =
                        referenced_return(context, &trajectory, reference).ok_or(EventError::UnknownOffer)?;
                    Ok(EngineEvent::SettleReturn {
                        trajectory: trajectory.clone(),
                        id,
                        decision,
                    })
                },
                None,
                None,
                Occasion::Other,
            )
            .await?;
        match decision.then {
            Next::PresentToModel(Presentation::Value { value }) => Ok(SettlementDecision::Accepted { value }),
            Next::PresentToModel(Presentation::Discarded) => Ok(SettlementDecision::Discarded),
            Next::PresentToModel(Presentation::Declined { feedback }) => {
                Ok(SettlementDecision::Unacceptable { feedback })
            }
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// The model called the `execute_remedy_plan` MCP tool. Executes one
    /// offer by its canonical id, which the runtime
    /// resolved from the quoted form before this point. An offer this
    /// trajectory does not pursue is refused.
    pub async fn on_remedy(
        &self,
        offer: OfferId,
        arguments: RemedyArguments,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
    ) -> Result<RemedyDecision, EventError> {
        self.on_remedy_at(
            &offer,
            Initiator::Agent,
            DeclaredReturn::Arguments(arguments),
            elicitation,
            ruling,
            Occasion::Remedy { offer: &offer },
        )
        .await
    }

    /// Execute an offer the runtime takes itself under the family's
    /// [`appa_runtime_api::AutoApply`], through the execution an agent's takes, recorded as
    /// the runtime's.
    pub(super) async fn auto_apply(&self, offer: OfferId) -> Result<RemedyDecision, EventError> {
        self.on_remedy_at(
            &offer,
            Initiator::AutoApply,
            DeclaredReturn::Arguments(RemedyArguments::default()),
            None,
            None,
            Occasion::Remedy { offer: &offer },
        )
        .await
    }

    /// Apply the return a blocked spawn's own proposal declares through its block's offer.
    pub(super) async fn apply_declared_return(
        &self,
        offer: &OfferId,
        declared: DeclaredReturn,
    ) -> Result<RemedyDecision, EventError> {
        self.on_remedy_at(
            offer,
            Initiator::Agent,
            declared,
            None,
            None,
            Occasion::Remedy { offer },
        )
        .await
    }

    /// Apply the declaration a blocked spawn took through its block's offer. The application
    /// is judged again where it commits, so one the family settled or moved meanwhile is
    /// refused rather than applied.
    pub(super) async fn apply_ahead(
        &self,
        offer: &OfferId,
        claim: &AheadClaim<'_>,
        arguments: RemedyArguments,
    ) -> Result<RemedyDecision, EventError> {
        self.on_remedy_at(
            offer,
            Initiator::Agent,
            DeclaredReturn::Arguments(arguments),
            None,
            None,
            Occasion::AheadApply { offer, claim },
        )
        .await
    }

    async fn on_remedy_at(
        &self,
        offer: &OfferId,
        initiator: Initiator,
        declared: DeclaredReturn,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<RemedyDecision, EventError> {
        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.offer_pursuer(offer).as_ref() != Some(&trajectory) {
                        return Err(EventError::UnknownOffer);
                    }
                    if let Occasion::AheadApply { claim, .. } = occasion {
                        let ahead = AheadState::fold(context.log.host_records());
                        if ahead.settlement(&trajectory, claim.attempt).is_some()
                            || !matches!(
                                context.judge(&ahead, claim)?,
                                Judgment::Apply { offer: judged, .. } if judged == *offer
                            )
                        {
                            return Err(EventError::AheadMoved);
                        }
                    }
                    Ok(EngineEvent::ExecuteOffer {
                        trajectory: trajectory.clone(),
                        offer: offer.clone(),
                        initiator,
                        declared: declared.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        cwd: self.inner.working_directory(&self.root),
                    })
                },
                elicitation,
                ruling,
                occasion,
            )
            .await?;

        match decision.then {
            Next::Approved { tool, bytes } => Ok(RemedyDecision::Authorized {
                call: ExactCall { tool, bytes },
            }),
            Next::PresentToModel(Presentation::Value { value }) => Ok(RemedyDecision::Returned { value }),
            Next::PresentToModel(Presentation::Declined { feedback }) => Ok(RemedyDecision::Declined {
                presentation: RemedyPresentation {
                    feedback,
                    outcomes: None,
                    offers: Vec::new(),
                    review: Vec::new(),
                    display: Vec::new(),
                },
            }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            Next::PresentToModel(Presentation::Blocked {
                feedback,
                outcomes,
                offers,
                review,
                display,
            }) => Ok(RemedyDecision::Declined {
                presentation: remedy_presentation(
                    feedback,
                    Some(outcomes),
                    offers,
                    review,
                    display.into_iter().collect(),
                    &self.deployment().externals,
                ),
            }),
            _ => Err(EventError::UnexpectedDecision),
        }
    }
}
