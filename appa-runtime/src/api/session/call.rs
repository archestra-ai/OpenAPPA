use super::*;

/// What a spawn's block says when the return its trajectory declared ahead has lapsed. The
/// block's reason already says to declare right before the subagent call.
pub(super) const LAPSED_AHEAD: &str = "[appa] The return you declared ahead lapsed, so it was not applied: a call between it \
    and this subagent call changed this session's label.";

/// What a spawn's block says when no declaration it offers fits the return declared ahead.
pub(super) const UNFIT_AHEAD: &str =
    "[appa] The return you declared ahead does not fit this call, so it was not applied.";

/// What a fan-out spawn's block says about a return declared ahead, which it does not apply.
pub(super) const FAN_OUT_AHEAD: &str = "[appa] A return declared ahead covers a single subagent call, so it was not applied. \
    A call that starts several subagents declares at its block.";

/// Follows a notice on a block that offers plans.
pub(super) const APPLY_BELOW: &str = " Apply a plan below.";

/// How many plans of stock `approve` rulings one proposal applies before its block stands. A
/// plan can go stale between its block and its execution, and an approval can meet a label
/// another dispatch moved; each round proposes again and reads the block that stands then.
const STOCK_APPROVE_ROUNDS: usize = 3;

/// One proposed tool call and what came with it: the host's call id, the spawn it starts, the
/// prompt a fan-out spawn is keyed by, and the held peer message a held read delivers.
struct Proposal {
    call: ProposedCall,
    call_id: Option<String>,
    spawn: Option<SpawnKind>,
    prompt: Option<PromptKey>,
    held_message: Option<appa_engine::value::HeldMessage>,
}

impl Session {
    #[tracing::instrument(target = "appa_telemetry", name = "appa.policy.check", skip_all, fields(
        appa.trajectory.root = crate::telemetry::name(&self.root.0),
        appa.trajectory.id = crate::telemetry::name(&self.trajectory.0),
        appa.tool.name = crate::telemetry::name(&call.tool),
        appa.tool.call.id = call_id.as_deref().map(crate::telemetry::name).unwrap_or_default(),
        appa.outcome = tracing::field::Empty,
        appa.policy.id = tracing::field::Empty,
        appa.policy.gaps = tracing::field::Empty,
        appa.policy.narrowing = tracing::field::Empty,
    ))]
    pub async fn on_tool_call_identified(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
    ) -> Result<ToolCallDecision, EventError> {
        let started = std::time::Instant::now();
        let tool = call.tool.clone();
        let result = self
            .check_tool_call(Proposal {
                call,
                call_id,
                spawn,
                prompt,
                held_message: None,
            })
            .await;
        crate::telemetry::policy(&result, &tool, started.elapsed().as_secs_f64());
        result
    }

    async fn check_tool_call(&self, proposal: Proposal) -> Result<ToolCallDecision, EventError> {
        let held_message = match crate::api::peer::held_read(&proposal.call) {
            Some(id) => {
                let log = self.inner.log(&self.root)?;
                let chain = self.inner.trust_chain(&self.deployment(), &log)?;
                Some(crate::api::peer::held_message(
                    &self.inner.store,
                    &self.root,
                    &id,
                    &chain,
                )?)
            }
            None => None,
        };
        self.propose_tool_call(Proposal {
            held_message,
            ..proposal
        })
        .await
    }

    pub(crate) async fn propose_with_held(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        held: appa_engine::value::HeldMessage,
    ) -> Result<ToolCallDecision, EventError> {
        self.propose_tool_call(Proposal {
            call,
            call_id,
            spawn: None,
            prompt: None,
            held_message: Some(held),
        })
        .await
    }

    /// Check a return declaration made ahead of the spawn at this trajectory's label, and answer
    /// that label: the declaration stands only there.
    pub(crate) fn declare_return_ahead(
        &self,
        arguments: &RemedyArguments,
    ) -> Result<appa_engine::label::Label, EventError> {
        self.with_view(|context| {
            context
                .engine()
                .declare_ahead(context.view, &self.trajectory, arguments)
                .map_err(|detail| EventError::RemedyArguments { detail })
        })
    }

    /// A return this trajectory declared ahead covers its next spawn, which takes it whatever
    /// its decision. A blocked single spawn applies it through the block's own declaration
    /// offer, one that asks no authority, and is proposed again, so it meets only what else
    /// blocks it. A declaration held at another label has lapsed; one no offer fits, or one a
    /// fan-out spawn meets, is not applied. The block says which.
    async fn propose_tool_call(&self, proposal: Proposal) -> Result<ToolCallDecision, EventError> {
        let decision = self.propose_resolving(&proposal).await?;
        let spawn = proposal.spawn;
        let Some(held) = spawn.and_then(|_| self.inner.take_held_return(&self.root, &self.trajectory)) else {
            return Ok(decision);
        };
        let ToolCallDecision::Deny {
            feedback,
            offers,
            display,
            review,
        } = decision
        else {
            return Ok(decision);
        };
        let noted = |notice: &str, apply: &str| ToolCallDecision::Deny {
            feedback: format!("{notice}{apply}\n\n{feedback}"),
            offers: offers.clone(),
            display: display.clone(),
            review: review.clone(),
        };
        let apply = if offers.is_empty() { "" } else { APPLY_BELOW };
        if spawn == Some(SpawnKind::FanOut) {
            return Ok(noted(FAN_OUT_AHEAD, apply));
        }
        let route = match held.arguments.return_schema {
            Some(_) => appa_runtime_api::OfferedReturn::Sanitized {
                sanitizer: appa_engine::names::SanitizerName::ATTEST_SCHEMA.to_string(),
            },
            None => appa_runtime_api::OfferedReturn::AsSpoken,
        };
        let (offer, label) = self.with_view(|context| {
            let offer = offers
                .iter()
                .filter(|offer| offer.returns.as_ref() == Some(&route))
                .filter_map(|offer| crate::engine::resolve_rendered(context.log, &OfferId(offer.id.clone())))
                .find(|offer| {
                    context
                        .engine()
                        .offer_declares_only(context.view, &self.trajectory, offer)
                });
            Ok((offer, context.engine().label_of(context.view, &self.trajectory)))
        })?;
        // A lapse is told first: at a changed label no offer may fit, and the cause is the change.
        if label != Some(held.label) {
            return Ok(noted(LAPSED_AHEAD, apply));
        }
        let Some(offer) = offer else {
            return Ok(noted(UNFIT_AHEAD, apply));
        };
        // A declaration that cannot be applied leaves the spawn the block it already met.
        match self.on_remedy(offer, held.arguments, None, None).await {
            Ok(RemedyDecision::Authorized { .. }) => self.propose_resolving(&proposal).await,
            Ok(_) => Ok(noted(UNFIT_AHEAD, apply)),
            Err(error) => {
                tracing::debug!(trajectory = %self.trajectory.0, %error, "the declaration made ahead was not applied");
                Ok(noted(UNFIT_AHEAD, apply))
            }
        }
    }

    /// Propose, and while the block offers a plan that asks only stock `approve` authorities,
    /// apply that plan and propose again: the agent sees the call released, never the block.
    /// The plan is read from the block that stands, never an earlier one, and every execution is
    /// followed by a proposal, so the decision returned is never older than the last fact this
    /// loop appended. An approval already recorded stays recorded, and the next proposal of the
    /// call spends it. A stock `approve` authority never denies, so a plan that was not applied
    /// lost a race: another dispatch moved its basis or took its subject. The next proposal
    /// reads what stands after that, and the rounds bound how often it is tried.
    async fn propose_resolving(&self, proposal: &Proposal) -> Result<ToolCallDecision, EventError> {
        let mut decision = self.propose_once(proposal).await?;
        for _ in 0..STOCK_APPROVE_ROUNDS {
            let ToolCallDecision::Deny { offers, .. } = &decision else {
                return Ok(decision);
            };
            let Some(offer) = self.stock_approved_offer(offers)? else {
                return Ok(decision);
            };
            let applied = self.on_remedy(offer, RemedyArguments::default(), None, None).await;
            if !matches!(applied, Ok(RemedyDecision::Authorized { .. })) {
                tracing::debug!(trajectory = %self.trajectory.0, ?applied, "a stock approve plan was not applied");
            }
            decision = self.propose_once(proposal).await?;
        }
        Ok(decision)
    }

    /// The first offered plan that asks only stock `approve` authorities, if the block has one.
    fn stock_approved_offer(&self, offers: &[appa_runtime_api::OfferedRemedy]) -> Result<Option<OfferId>, EventError> {
        let deployment = self.deployment();
        if offers.is_empty() || !deployment.externals.has_stock_approve() {
            return Ok(None);
        }
        self.with_view(|context| {
            Ok(offers
                .iter()
                .filter_map(|offer| crate::engine::resolve_rendered(context.log, &OfferId(offer.id.clone())))
                .find(|offer| {
                    context
                        .engine()
                        .offer_asks_only(context.view, &self.trajectory, offer, |authority| {
                            deployment.externals.is_stock_approve(authority)
                        })
                }))
        })
    }

    async fn propose_once(&self, proposal: &Proposal) -> Result<ToolCallDecision, EventError> {
        let Proposal {
            call,
            call_id,
            spawn,
            prompt,
            held_message,
        } = proposal;
        let spawn = *spawn;
        self.inner.note_working_directory(&self.root, call.cwd.as_deref());
        if spawn.is_some()
            && self.inner.shared.naming.spawn_coverage() == crate::api::SpawnCoverage::Declared
            && !self.names_tool(&call.tool)?
        {
            tracing::debug!(trajectory = %self.trajectory.0, tool = %call.tool, "spawn denied: the policy names no such agent");
            return Err(EventError::UndeclaredSpawn {
                tool: call.tool.clone(),
            });
        }
        let decision = self
            .drive_with_evidence(
                |context, mut evidence| {
                    if let Some(message) = held_message {
                        evidence.push(ExternalEvidence::Held {
                            message: message.clone(),
                        });
                    }
                    Ok(EngineEvent::ModelResponse {
                        call: call.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        spawn,
                        replay: context.replay_batch(call, call_id.as_deref())?,
                    })
                },
                None,
                None,
                Occasion::Proposal {
                    call_id: call_id.as_deref(),
                    prompt: prompt.as_ref(),
                },
            )
            .await?;

        match decision.then {
            Next::ModelResponse { invocations, feedback } => match (invocations.as_slice(), feedback.as_slice()) {
                ([released], []) => {
                    tracing::debug!(
                        trajectory = %self.trajectory.0,
                        tool = %released.tool,
                        spawn = released.fork.is_some(),
                        "call released"
                    );
                    Ok(ToolCallDecision::Allow {
                        spawn: released.fork.clone(),
                        dispatch: released.dispatch.clone(),
                    })
                }
                ([], feedback) if !feedback.is_empty() => {
                    tracing::debug!(trajectory = %self.trajectory.0, "call blocked");
                    Ok(ToolCallDecision::Deny {
                        feedback: join_feedback(feedback),
                        offers: feedback.iter().flat_map(|entry| entry.offers.clone()).collect(),
                        display: feedback.iter().filter_map(|entry| entry.display.clone()).collect(),
                        review: join_review(feedback, &self.deployment().externals),
                    })
                }
                _ => Err(EventError::UnexpectedDecision),
            },
            _ => Err(EventError::UnexpectedDecision),
        }
    }

    /// Whether the serving policy writes a contract for this tool's exact name — the
    /// wildcard does not count. What a spawn needs under [`crate::api::SpawnCoverage::Declared`].
    pub(super) fn names_tool(&self, tool: &str) -> Result<bool, EventError> {
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        Ok(policy.engine().names_tool(tool))
    }

    /// Close one call this trajectory has open as one that did not run.
    /// The dispatch is re-read from the view on every replay, so a
    /// contended append never closes an occurrence the winning writer
    /// already closed. Both callers reach here the same way: a released
    /// call that got no outcome before its turn or its child ended. A
    /// staged derivation opens nothing and so has nothing to close.
    pub(super) async fn abandon_dispatch(
        &self,
        dispatch: appa_engine::value::DispatchId,
        outcome: &ToolOutcome,
    ) -> Result<EngineDecision, EventError> {
        self.drive_with_evidence(
            move |context, evidence| {
                if !context.open_dispatches().iter().any(|open| open.id == dispatch) {
                    return Err(EventError::UnknownDispatch);
                }
                Ok(EngineEvent::ToolOutcome {
                    dispatch: dispatch.clone(),
                    outcome: outcome.clone(),
                    evidence,
                    entropy: fresh_entropy(),
                })
            },
            None,
            None,
            Occasion::Other,
        )
        .await
    }
}
