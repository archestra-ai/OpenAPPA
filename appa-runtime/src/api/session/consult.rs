use super::*;

/// What the context providers answered about one call, the providers asked together. A
/// provider that answers `null` — the call is not its concern — is left out; one that gives
/// no answer is recorded with why, so the Annotator judges without that fact.
pub(in crate::api) async fn gather_context<'a, Asked>(
    consults: &'a [Consult],
    ask: impl Fn(&'a Consult) -> Asked,
) -> AnnotationContext
where
    Asked: std::future::Future<Output = ConsultOutcome>,
{
    let mut asked = Vec::with_capacity(consults.len());
    for consult in consults {
        asked.push(ask(consult));
    }
    let outcomes = crate::external::settle_batch(asked).await;
    let entries = consults
        .iter()
        .zip(outcomes)
        .filter_map(|(consult, outcome)| {
            let entry = match outcome {
                ConsultOutcome::Answer(serde_json::Value::Null) => return None,
                ConsultOutcome::Answer(answer) => ContextEntry::Answer(answer),
                ConsultOutcome::NoAnswer(reason) => {
                    tracing::debug!(
                        provider = consult.name,
                        ?reason,
                        "a context provider produced no answer"
                    );
                    ContextEntry::Error(reason.diagnostic())
                }
            };
            Some((ContextProviderName::new(consult.name.clone()), entry))
        })
        .collect();
    AnnotationContext::new(entries)
}

impl Session {
    /// Run one consult, timed, and note what it cost.
    ///
    /// Every consult in this file goes through here rather than calling `externals.consult`
    /// directly. An external that is slow, unreachable, or answering nonsense is the single
    /// most common reason an agent appears to be stuck for no reason the trajectory's facts
    /// explain, and the duration is only knowable at the await.
    ///
    /// With a transcriber attached, the consult is also transcribed and its record handed over
    /// once the outcome is known. `call` is the canonical call an annotation consult judges.
    #[tracing::instrument(target = "appa_telemetry", name = "appa.external.call", skip_all, fields(
        appa.trajectory.root = crate::telemetry::name(&self.root.0),
        appa.trajectory.id = crate::telemetry::name(&self.trajectory.0),
        appa.external.name = crate::telemetry::name(&consult.name),
    ))]
    pub(super) async fn timed_consult(
        &self,
        consult: &Consult,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
        call: Option<&appa_engine::value::CanonicalDigest>,
    ) -> crate::external::ConsultOutcome {
        let started_at = std::time::SystemTime::now();
        let started = std::time::Instant::now();
        let deployment = self.deployment();
        let externals = &deployment.externals;
        let consulting = async {
            match &self.inner.transcriber {
                // Boxed: the transport future is large, and every drive awaits it deep in the stack.
                Some(_) => Box::pin(externals.consult_transcribed(consult, elicitation, ruling)).await,
                None => (Box::pin(externals.consult(consult, elicitation, ruling)).await, None),
            }
        };
        #[cfg(feature = "archestra")]
        let (outcome, transcript) = {
            let attribution = crate::model::archestra::Attribution {
                root: self.root.clone(),
                trajectory: self.trajectory.clone(),
                call_id: occasion.call_id().map(str::to_owned),
            };
            crate::model::archestra::within(self.inner.archestra_context.as_deref(), attribution, consulting).await
        };
        #[cfg(not(feature = "archestra"))]
        let (outcome, transcript) = consulting.await;
        // A transport that read on for the record alone says when the outcome was known.
        let settled = transcript
            .as_ref()
            .and_then(|transcript| transcript.settled)
            .unwrap_or_else(std::time::Instant::now);
        let duration_ms = u64::try_from(settled.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX);
        // Filed under the family, never the acting trajectory: a subagent's slow authority is
        // part of its family's account, and `EventLog` reads one root's bucket.
        self.inner.record(
            Some(&self.root),
            crate::events::RuntimeEvent::External {
                role: consult.kind().into(),
                name: consult.name.clone(),
                outcome: (&outcome).into(),
                duration_ms,
                offer: None,
                dispatch: None,
            },
        );
        if let (Some(transcriber), Some(transcript)) = (&self.inner.transcriber, transcript) {
            // The host's code: whatever it does, the outcome below stands.
            let recorded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let context = ConsultContext {
                    root: self.root.0.clone(),
                    trajectory: self.trajectory.0.clone(),
                    call_id: occasion.call_id().map(str::to_string),
                    offer_id: match occasion {
                        Occasion::Remedy { offer } => Some(offer.0.clone()),
                        Occasion::Proposal { .. } | Occasion::Report { .. } | Occasion::Other => None,
                    },
                    call_digest: call.map(|call| crate::engine::hex(call.bytes())),
                };
                if let Some(record) =
                    ConsultRecord::new(consult, &outcome, transcript, started_at, duration_ms, context)
                {
                    transcriber.record(record);
                }
            }));
            if recorded.is_err() {
                tracing::warn!(
                    external = consult.name,
                    "the consult transcriber panicked; the record is dropped"
                );
            }
        }
        outcome
    }

    /// One external consult. Only an annotation failure is an error: the call cannot be
    /// judged without its annotation, no fact may be appended, and the refusal is operational
    /// — never a policy denial. Every other external keeps its no-answer evidence shape.
    pub(super) async fn consult(
        &self,
        request: ExternalRequest,
        elicitation: Option<&Elicitation>,
        ruling: Option<appa_runtime_api::Ruling>,
        occasion: Occasion<'_>,
    ) -> Result<ExternalEvidence, EventError> {
        Ok(match &request {
            ExternalRequest::Authority {
                authority,
                declaration,
                artifact,
                review,
            } => {
                let consult = Consult {
                    name: authority.clone(),
                    body: ConsultBody::Authority {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let verdict = match self.timed_consult(&consult, elicitation, ruling, occasion, None).await {
                    ConsultOutcome::Answer(answer) => AuthorityVerdict::from_wire(&answer),
                    ConsultOutcome::NoAnswer(crate::external::NoAnswerReason::Unreachable) => {
                        AuthorityVerdict::Abstain(Abstention::Unreachable)
                    }
                    ConsultOutcome::NoAnswer(crate::external::NoAnswerReason::Unregistered) => {
                        AuthorityVerdict::Abstain(Abstention::Unregistered)
                    }
                    ConsultOutcome::NoAnswer(_) => AuthorityVerdict::Abstain(Abstention::Unanswered),
                };
                ExternalEvidence::Authority {
                    authority: authority.clone(),
                    verdict,
                    review: review.clone(),
                }
            }
            ExternalRequest::Sanitizer {
                sanitizer,
                source,
                declaration,
                artifact,
            } => {
                let consult = Consult {
                    name: sanitizer.clone(),
                    body: ConsultBody::Sanitizer {
                        declaration: declaration.clone(),
                        artifact: artifact.clone(),
                    },
                };
                let derived = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => SanitizerAnswer::from_wire(&answer).map(|answer| answer.body),
                    ConsultOutcome::NoAnswer(_) => None,
                };
                ExternalEvidence::Sanitizer {
                    sanitizer: sanitizer.clone(),
                    source: *source,
                    derived,
                }
            }
            ExternalRequest::Annotation {
                annotator,
                call,
                declaration,
                args,
                context,
            } => {
                // Every context provider answers first, together; one that does not leaves
                // an error in the context and the Annotator is still asked.
                let deployment = self.deployment();
                let consults = deployment.externals.context_consults(context);
                let context = gather_context(&consults, |consult| {
                    self.timed_consult(consult, None, None, occasion, Some(call))
                })
                .await;
                let declaration = declaration
                    .clone()
                    .with_context_hints(&context, &deployment.config.externals.context_hints);
                let consult = Consult {
                    name: annotator.clone(),
                    body: ConsultBody::Annotation {
                        declaration: declaration.clone(),
                        artifact: AnnotationArtifact {
                            args: args.clone(),
                            context: context.clone(),
                        },
                    },
                };
                let answer = match self.timed_consult(&consult, None, None, occasion, Some(call)).await {
                    ConsultOutcome::Answer(answer) => AnnotationAnswer::from_wire(&answer, &declaration)
                        .map_err(crate::external::NoAnswerReason::MalformedAnswer),
                    ConsultOutcome::NoAnswer(reason) => Err(reason),
                };
                // Annotation failure is an operational refusal, never model feedback: the
                // call is not judged, nothing is appended, and the harness fails closed.
                let answer = match answer {
                    Ok(answer) => answer,
                    Err(reason) => {
                        match reason {
                            crate::external::NoAnswerReason::Unreachable | crate::external::NoAnswerReason::Timeout => {
                                tracing::warn!(annotator, ?reason, "an annotation consult produced no answer")
                            }
                            _ => tracing::debug!(annotator, ?reason, "an annotation consult produced no answer"),
                        }
                        return Err(EventError::annotation_refused(annotator.clone(), reason.diagnostic()));
                    }
                };
                ExternalEvidence::Annotation {
                    annotator: annotator.clone(),
                    // The evidence names the exact call it answered for: a rewritten call
                    // never consumes a stale annotation.
                    call: *call,
                    answer,
                    context,
                }
            }
            ExternalRequest::AudienceSource {
                provider,
                selector,
                templates,
            } => {
                let consult = Consult::audience_selector(provider, selector, templates.clone());
                let members = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => MembersAnswer::from_wire(&answer)
                        .map(|answer| answer.members.into_iter().map(ReaderId::new).collect())
                        .ok_or(crate::events::NoAnswerClass::Malformed),
                    ConsultOutcome::NoAnswer(reason) => Err((&reason).into()),
                };
                ExternalEvidence::AudienceSource {
                    provider: provider.clone(),
                    selector: selector.clone(),
                    members,
                }
            }
            ExternalRequest::MemberLookup {
                provider,
                member,
                answering,
                templates,
            } => {
                // The evidence stays keyed by the member's own provider, whichever entry
                // answered.
                let consult = Consult::member_lookup(answering, member, templates.clone());
                let principal = match self.timed_consult(&consult, None, None, occasion, None).await {
                    ConsultOutcome::Answer(answer) => LookupAnswer::from_wire(&answer)
                        .map(|answer| answer.principal.map(ReaderId::new))
                        .ok_or(crate::events::NoAnswerClass::Malformed),
                    ConsultOutcome::NoAnswer(reason) => Err((&reason).into()),
                };
                ExternalEvidence::MemberLookup {
                    provider: provider.clone(),
                    member: member.clone(),
                    principal,
                }
            }
        })
    }
}
