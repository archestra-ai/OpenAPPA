use super::*;

pub(super) fn remedy_presentation(
    feedback: String,
    offers: Vec<appa_runtime_api::OfferedRemedy>,
    pending: Vec<PendingReview>,
    display: Vec<crate::api::RemedyDisplay>,
    externals: &crate::external::ExternalServices,
) -> RemedyPresentation {
    RemedyPresentation {
        feedback,
        offers,
        review: reviews(&pending, externals),
        display,
    }
}

/// The reviews a harness with its own channel shows: one per offer whose plan consults a
/// `hitl` authority, the texts of several such authorities joined. A joined review names no
/// one authority, hint or scope, so those read as unknown and a host shows the whole text.
/// Authorities of every other backend answer themselves and need no person here.
pub(super) fn join_review(
    feedback: &[Feedback],
    externals: &crate::external::ExternalServices,
) -> Vec<appa_runtime_api::Review> {
    reviews(
        &feedback
            .iter()
            .flat_map(|entry| entry.review.iter())
            .cloned()
            .collect::<Vec<_>>(),
        externals,
    )
}

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

pub(super) fn join_feedback(feedback: &[Feedback]) -> String {
    feedback
        .iter()
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

impl Session {
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
        let trajectory = self.trajectory.clone();
        let decision = self
            .drive_with_evidence(
                |context, evidence| {
                    if context.offer_pursuer(&offer).as_ref() != Some(&trajectory) {
                        return Err(EventError::UnknownOffer);
                    }
                    Ok(EngineEvent::ExecuteOffer {
                        trajectory: trajectory.clone(),
                        offer: offer.clone(),
                        arguments: arguments.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        cwd: self.inner.working_directory(&self.root),
                    })
                },
                elicitation,
                ruling,
                Occasion::Remedy { offer: &offer },
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
                    offers: Vec::new(),
                    review: Vec::new(),
                    display: Vec::new(),
                },
            }),
            Next::PresentToModel(Presentation::NoAnswer { feedback }) => Ok(RemedyDecision::NoAnswer { feedback }),
            Next::PresentToModel(Presentation::Blocked {
                feedback,
                offers,
                review,
                display,
            }) => Ok(RemedyDecision::Declined {
                presentation: remedy_presentation(
                    feedback,
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
