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
        let result = self.check_tool_call(call, call_id, spawn, prompt).await;
        crate::telemetry::policy(&result, &tool, started.elapsed().as_secs_f64());
        result
    }

    pub(super) async fn check_tool_call(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
    ) -> Result<ToolCallDecision, EventError> {
        if let Some(id) = crate::api::peer::held_read(&call) {
            let log = self.inner.log(&self.root)?;
            let chain = self.inner.trust_chain(&self.deployment(), &log)?;
            let basis = crate::api::peer::held_basis(&self.inner.store, &self.root, &id, &chain)?;
            return self.propose_tool_call(call, call_id, spawn, prompt, Some(basis)).await;
        }
        let Some(files) = &self.inner.shared.files else {
            return self.propose_tool_call(call, call_id, spawn, prompt, None).await;
        };
        if !crate::api::files::owns(&call) {
            let bound = files
                .root_is_bound(&self.inner.store, &self.root)
                .map_err(crate::api::files::refused)?;
            if bound && crate::api::files::bypasses_tracking(&call) {
                return Err(crate::api::files::refused(
                    "the bound workspace refuses native filesystem and shell tools",
                ));
            }
            return self.propose_tool_call(call, call_id, spawn, prompt, None).await;
        }
        let ledger_policy = match call.cwd.as_deref() {
            Some(cwd) => files.bind(&self.inner.store, &self.root, cwd),
            None => files.store(&self.inner.store, &self.root),
        }
        .and_then(|store| store.policy())
        .map_err(crate::api::files::refused)?;
        let (operation, path) = crate::api::files::operation(&call)?;
        let log = self.inner.log(&self.root)?;
        let deployment = self.deployment();
        // A family shares the ledger with every family of its deployment while the ledger opened
        // under a policy file the deployment published with the trust chain the family is judged
        // under: the ledger's labels rank against that chain.
        let followed = log.deployment();
        let shares_ledger = followed.as_str() == files.deployment
            && match self
                .inner
                .store
                .published_as(followed, &ledger_policy)
                .map_err(|error| EventError::Storage(error.to_string()))?
            {
                Some(revision) => {
                    let ledger_chain = self.inner.replay_engine(revision.value(), None)?;
                    let judging = self.inner.trust_chain(&deployment, &log)?;
                    ledger_chain.trust_chain().names().eq(judging.names())
                }
                None => false,
            };
        if !shares_ledger {
            return Err(crate::api::files::refused(
                "the workspace ledger must belong to the trajectory's policy and rank its labels against \
                 the same trust chain",
            ));
        }
        let policy = self.inner.resolve_policy(&deployment, &log)?;
        let view = policy.rebuild_view(&log)?;
        let expected = policy.engine().file_dispatch(&view, &self.trajectory, &call)?;
        let key = crate::api::files::key(&expected)?;
        let pin = match operation {
            appa_eventlog::files::FileOperation::Process => {
                if files.process_backend.is_none() {
                    return Err(crate::api::files::refused("isolated processing is not enabled"));
                }
                let args: crate::api::files::ProcessArgs =
                    serde_json::from_str(call.arguments.get()).map_err(crate::api::files::refused)?;
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path, inputs) =
                        (self.trajectory.0.clone(), key.clone(), path.clone(), args.input_paths);
                    move |store| store.prepare_process(&actor, &key, &inputs, &path)
                })
                .await?
            }
            appa_eventlog::files::FileOperation::Copy | appa_eventlog::files::FileOperation::Move => {
                let args: crate::api::files::FileTransferArgs =
                    serde_json::from_str(call.arguments.get()).map_err(crate::api::files::refused)?;
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path, source) =
                        (self.trajectory.0.clone(), key.clone(), path.clone(), args.source_path);
                    move |store| store.prepare_transfer(&actor, &key, operation, &source, &path)
                })
                .await?
            }
            _ => {
                ledger(self.inner.clone(), self.root.clone(), {
                    let (actor, key, path) = (self.trajectory.0.clone(), key.clone(), path.clone());
                    move |store| store.prepare(&actor, &key, operation, &path)
                })
                .await?
            }
        };
        // Managed writes must not reconfigure Claude Code, Git hooks, or MCP execution.
        // Claude loads instruction files implicitly, outside the file-tool observation path.
        let moved_from = match &pin.basis {
            appa_eventlog::files::PinnedBasis::Move { source, .. } => Some(source.path.as_str()),
            _ => None,
        };
        if operation != appa_eventlog::files::FileOperation::Read
            && std::iter::once(pin.path.as_str())
                .chain(moved_from)
                .flat_map(|path| path.split('/'))
                .any(|part| {
                    matches!(
                        part,
                        ".claude" | ".git" | ".mcp.json" | ".appa" | "CLAUDE.md" | "CLAUDE.local.md"
                    )
                })
        {
            files
                .store(&self.inner.store, &self.root)
                .map_err(crate::api::files::refused)?
                .cancel(&self.trajectory.0, &key)
                .map_err(crate::api::files::refused)?;
            return Err(crate::api::files::refused(
                "execution-control files are not writable in file-tracking mode",
            ));
        }
        let basis = pin.file_basis();
        let decision = self.propose_tool_call(call, call_id, spawn, prompt, Some(basis)).await;
        match &decision {
            Ok(ToolCallDecision::Allow { dispatch, .. }) => {
                if dispatch != &expected {
                    return Err(crate::api::files::refused("dispatch changed while reserving the file"));
                }
                let log = self.inner.log(&self.root)?;
                let view = policy.rebuild_view(&log)?;
                let label = policy.engine().file_output_label(&view, dispatch)?;
                files
                    .store(&self.inner.store, &self.root)
                    .map_err(crate::api::files::refused)?
                    .bind(&self.trajectory.0, &key, dispatch, &label)
                    .map_err(crate::api::files::refused)?;
            }
            _ => {
                files
                    .store(&self.inner.store, &self.root)
                    .map_err(crate::api::files::refused)?
                    .cancel(&self.trajectory.0, &key)
                    .map_err(crate::api::files::refused)?;
            }
        }
        decision
    }

    pub(crate) async fn propose_with_basis(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        basis: appa_engine::value::FileBasis,
    ) -> Result<ToolCallDecision, EventError> {
        self.propose_tool_call(call, call_id, None, None, Some(basis)).await
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
    pub(super) async fn propose_tool_call(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
        file_basis: Option<appa_engine::value::FileBasis>,
    ) -> Result<ToolCallDecision, EventError> {
        let decision = self
            .propose_once(call.clone(), call_id.clone(), spawn, prompt.clone(), file_basis.clone())
            .await?;
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
            Ok(RemedyDecision::Authorized { .. }) => self.propose_once(call, call_id, spawn, prompt, file_basis).await,
            Ok(_) => Ok(noted(UNFIT_AHEAD, apply)),
            Err(error) => {
                tracing::debug!(trajectory = %self.trajectory.0, %error, "the declaration made ahead was not applied");
                Ok(noted(UNFIT_AHEAD, apply))
            }
        }
    }

    pub(super) async fn propose_once(
        &self,
        call: ProposedCall,
        call_id: Option<String>,
        spawn: Option<SpawnKind>,
        prompt: Option<PromptKey>,
        file_basis: Option<appa_engine::value::FileBasis>,
    ) -> Result<ToolCallDecision, EventError> {
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
                    if let Some(basis) = &file_basis {
                        evidence.push(ExternalEvidence::File { basis: basis.clone() });
                    }
                    Ok(EngineEvent::ModelResponse {
                        call: call.clone(),
                        evidence,
                        entropy: fresh_entropy(),
                        spawn,
                        replay: context.replay_batch(&call, call_id.as_deref())?,
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
