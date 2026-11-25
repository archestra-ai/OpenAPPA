//! The hook dispatcher: one canonical wire event in, one wire decision
//! out; between them, one typed `HookEvent` and one `HookDecision`.

use appa_engine::label::ReaderId;
use appa_engine::value::DispatchId as EngineDispatchId;
use appa_runtime_api::{
    Accepted, Actor, Adapter, HookDecision, HookEvent, OpenCalls, Outcomes, ParseRefusal, PeerAddress, PeerFrame,
    PrincipalSource, PromptKey, ProposedCall, RetryHint, Ruling, SessionSettings, SessionTitle, SpawnKind, SpawnRef,
    StartKind, ToolOutcome, TrajectoryId, WireDecision, WireEvent,
};

use crate::api::peer::{PeerSend, Received, SEND_MESSAGE};
use crate::api::{
    ChildReturnDecision, Deciding, EmbeddedHookOutcome, EmbeddedPresentationOptions, EventError, LateOpen,
    LaunchedStart, Quoted, RemedyPresentation, Runtime, Session, SpawnResultDecision, Stop, ToolCallDecision,
    ToolResultDecision, is_control_tool,
};
use crate::engine::{MissingIn, ValueKind};
use appa_eventlog::EndKind;

fn wire(decision: &HookDecision) -> serde_json::Value {
    serde_json::to_value(WireDecision::of(decision)).expect("a wire decision serializes")
}

/// One hook's answer: the status the hook command turns into an exit code, and the body it
/// prints. A non-2xx status makes the command exit 2, which blocks the action — hooks fail
/// closed.
type Answered = (u16, serde_json::Value);

/// How long after its hook arrived a single spawn proposed while another waits for its child
/// is still decided again rather than refused as outstanding. Half the hook's authorization
/// budget, so the decision that releases it still fits in the other half.
const SPAWN_HOLD: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(feature = "daemon")]
const _: () = assert!(2 * SPAWN_HOLD.as_secs() <= crate::hook_client::AUTHORIZATION_BUDGET.as_secs());

/// One hook call: validate the canonical wire, take in what the event observed, check what
/// it names, dispatch, and record its outcome. Each step either hands the next one an event
/// or answers the hook itself.
#[tracing::instrument(target = "appa_telemetry", name = "appa.hook", skip_all)]
pub async fn answer(runtime: &Runtime, adapter: &Adapter, principal: PrincipalSource, body: &[u8]) -> Answered {
    let Accepted {
        event,
        names_children,
        inventory,
    } = match accepted(runtime, adapter, principal, body) {
        Ok(accepted) => accepted,
        Err(answered) => return answered,
    };
    // No family is known yet for either refusal, and filing under the host id would invent one.
    let unfiled = |event: &HookEvent, error| {
        let (kind, tool) = hook_shape(event);
        runtime.record(None, bare_hook(kind, crate::events::HookOutcome::Refused, tool));
        (409, wire(&refuse(error)))
    };
    // A launched start decides from this alias lookup through the identity it records.
    let deciding = match &event {
        HookEvent::SessionStart {
            launch: Some(launch),
            settings,
            ..
        } => match runtime.decide_launch(launch.clone(), settings) {
            Ok(deciding) => Some(deciding),
            Err(error) => return unfiled(&event, error),
        },
        _ => None,
    };
    let (event, names_children) = match runtime.alias_of(hook_root(&event)) {
        Ok(family) => continued(event, names_children, family),
        Err(error) => return unfiled(&event, error),
    };
    let root = hook_root(&event).clone();
    // The dispatcher refuses a call with an empty id; nothing that call carries lands first.
    let refused_at_dispatch = matches!(&event, HookEvent::ToolCall { call_id: Some(id), .. } if id.is_empty());
    if let Some(inventory) = inventory
        && !refused_at_dispatch
        && let Err(answered) = observed(runtime, adapter, &event, &root, &inventory)
    {
        return answered;
    }
    if !refused_at_dispatch && let Some(answered) = names_a_childs_transcript(runtime, &event, &root, &names_children) {
        return answered;
    }
    let presentation = EmbeddedPresentationOptions {
        // These v1 plugins have no checked child-return lifecycle. Do not advise a model
        // to delegate on the assumption that the child's labels are inherited.
        supports_delegation: !matches!(
            adapter.name,
            appa_runtime_api::AdapterName::Amp | appa_runtime_api::AdapterName::OpenCode
        ),
        yell_tool: served_yell_tool(runtime),
        ..EmbeddedPresentationOptions::default()
    };
    let call_ids = match adapter.name {
        appa_runtime_api::AdapterName::ClaudeCode => ToolCallIds::Named,
        _ => ToolCallIds::Unnamed,
    };
    let handled = match (event, deciding) {
        // Dispatched in place, so nothing awaits while the start's launch is deciding.
        (
            HookEvent::SessionStart {
                root,
                principal,
                address,
                title,
                start,
                settings,
                ..
            },
            deciding,
        ) => {
            let (mut dispatch, mut shown) = (None, None);
            let decision = Dispatcher {
                runtime,
                dispatch: &mut dispatch,
                presentation: &mut shown,
                options: &presentation,
                call_ids,
            }
            .session_start(root, principal, settings, address, title, start, deciding);
            handled((crate::events::HookKind::SessionStart, None), dispatch, shown, decision)
        }
        (event, None) => handle_internal(runtime, event, presentation, call_ids).await,
        (_, Some(_)) => unreachable!("only a session start carries a launch"),
    };
    runtime.record(Some(&root), handled.event);
    let status = match handled.decision {
        HookDecision::Refuse { .. } => 409,
        _ => 200,
    };
    (status, wire(&handled.decision))
}

/// The event to dispatch, or the answer that ends the hook here. Every way a hook can end
/// leaves exactly one entry, including the three below that never reach the dispatcher.
/// Those are the answers a reader is most likely to be confused by: nothing happened, and
/// the trajectory's facts say nothing about why. None of them has an actor yet, so they are
/// recorded deployment-wide.
fn accepted(
    runtime: &Runtime,
    adapter: &Adapter,
    principal: PrincipalSource,
    body: &[u8],
) -> Result<Accepted, Answered> {
    use crate::events::{HookKind, HookOutcome};

    match WireEvent::read(body).and_then(|event| event.into_event(adapter, principal)) {
        Ok(Some(accepted)) => Ok(accepted),
        Ok(None) => {
            runtime.record(None, bare_hook(HookKind::Ignored, HookOutcome::Ignored, None));
            Err((200, wire(&HookDecision::Ack)))
        }
        Err(ParseRefusal::Unreadable { detail }) => {
            runtime.record(None, bare_hook(HookKind::Unparsable, HookOutcome::Unreadable, None));
            Err((400, serde_json::json!({ "error": detail })))
        }
        Err(ParseRefusal::Malformed { detail }) => {
            runtime.record(None, bare_hook(HookKind::Unparsable, HookOutcome::Malformed, None));
            Err((409, serde_json::json!({ "error": detail })))
        }
    }
}

/// The event as the family its host id continues: an aliased host id names that family's
/// root, and its children are that family's children.
fn continued(
    event: HookEvent,
    names_children: Vec<TrajectoryId>,
    family: Option<TrajectoryId>,
) -> (HookEvent, Vec<TrajectoryId>) {
    let host = hook_root(&event).clone();
    let Some(family) = family else {
        return (event, names_children);
    };
    let id = |id: TrajectoryId| match id.0.strip_prefix(&host.0) {
        Some("") => family.clone(),
        Some(rest) if rest.starts_with(':') => TrajectoryId(format!("{}{rest}", family.0)),
        _ => id,
    };
    let actor = |actor: Actor| Actor {
        root: id(actor.root),
        child: actor.child.map(id),
    };
    let event = match event {
        HookEvent::SessionStart {
            root,
            principal,
            address,
            title,
            start,
            launch,
            settings,
        } => HookEvent::SessionStart {
            root: id(root),
            principal,
            address,
            title,
            start,
            launch,
            settings,
        },
        HookEvent::Prompt {
            actor: who,
            text,
            settles,
            peer,
            title,
        } => HookEvent::Prompt {
            actor: actor(who),
            text,
            settles,
            peer,
            title,
        },
        HookEvent::TurnEnd { actor: who, open_calls } => HookEvent::TurnEnd {
            actor: actor(who),
            open_calls,
        },
        HookEvent::ToolCall {
            actor: who,
            call,
            call_id,
            spawn,
            prompt,
            ruling,
            declares,
        } => HookEvent::ToolCall {
            actor: actor(who),
            call,
            call_id,
            spawn,
            prompt,
            ruling,
            declares,
        },
        HookEvent::SpawnResume {
            actor: who,
            call,
            child,
        } => HookEvent::SpawnResume {
            actor: actor(who),
            call,
            child: id(child),
        },
        HookEvent::ToolResult {
            actor: who,
            call,
            call_id,
            outcome,
        } => HookEvent::ToolResult {
            actor: actor(who),
            call,
            call_id,
            outcome,
        },
        HookEvent::SpawnResult {
            actor: who,
            call,
            call_id,
            outcome,
            child,
            value,
        } => HookEvent::SpawnResult {
            actor: actor(who),
            call,
            call_id,
            outcome,
            child: child.map(id),
            value,
        },
        HookEvent::ChildStart { root, child, spawn } => HookEvent::ChildStart {
            root: id(root),
            child: id(child),
            spawn,
        },
        HookEvent::ChildTurnEnd {
            root,
            child,
            open_calls,
        } => HookEvent::ChildTurnEnd {
            root: id(root),
            child: id(child),
            open_calls,
        },
        HookEvent::ChildEnd {
            root,
            child,
            value,
            open_calls,
        } => HookEvent::ChildEnd {
            root: id(root),
            child: id(child),
            value,
            open_calls,
        },
        HookEvent::ChildReturn { root, child, value } => HookEvent::ChildReturn {
            root: id(root),
            child: id(child),
            value,
        },
    };
    (event, names_children.into_iter().map(id).collect())
}

/// Take in what a start or a call observed of the harness's tools. A child's start is
/// checked against what the family already bound before anything is observed, because a
/// subagent cannot rebind a name its parent's session decided. A session the inventory
/// arrives before is created with it rather than after it, so no trajectory exists without
/// what it observed; a child never creates one, since its family's own start does.
fn observed(
    runtime: &Runtime,
    adapter: &Adapter,
    event: &HookEvent,
    root: &TrajectoryId,
    inventory: &appa_runtime_api::inventory::ToolInventory,
) -> Result<(), Answered> {
    if matches!(event, HookEvent::ChildStart { .. }) {
        let checked = runtime.check_inventory(root, *adapter, inventory).and_then(|report| {
            if report.is_valid() {
                Ok(())
            } else {
                let mut errors = report.errors;
                errors.extend(report.tools.into_iter().filter_map(|tool| match tool.status {
                    crate::tool_validation::ToolStatus::Invalid { reason } => Some(format!("{}: {reason}", tool.tool)),
                    _ => None,
                }));
                Err(EventError::InventoryRefused(errors.join("; ")))
            }
        });
        if let Err(error) = checked {
            return Err(refused_hook(runtime, root, event, error));
        }
    }
    let actor = match event {
        HookEvent::SessionStart { root, .. } => Actor {
            root: root.clone(),
            child: None,
        },
        HookEvent::ChildStart { root, child, .. } => Actor {
            root: root.clone(),
            child: Some(child.clone()),
        },
        HookEvent::ToolCall { actor, .. } => actor.clone(),
        _ => unreachable!("wire validation restricts inventory to starts and tool calls"),
    };
    // A start's principal and settings are held to their shape and to the root they continue
    // before the inventory lands, so a refused start leaves no root behind and changes no
    // stored one.
    let (principal, settings) = match event {
        HookEvent::SessionStart {
            principal, settings, ..
        } => (
            session_principal(principal.as_deref()).map_err(|error| refused_hook(runtime, root, event, error))?,
            settings.clone(),
        ),
        _ => (None, SessionSettings::default()),
    };
    let continued = |session: Session| {
        continues_for(&session, principal.as_ref(), &settings)?;
        runtime.observe_inventory(&actor, *adapter, inventory)
    };
    let observed = match runtime.session(root, root) {
        Ok(session) => continued(session),
        Err(EventError::UnknownTrajectory) if actor.child.is_none() => {
            match runtime.create_session_with_inventory(
                root.clone(),
                inventory.clone(),
                principal.clone(),
                settings.clone(),
            ) {
                Ok(_) => runtime.observe_inventory(&actor, *adapter, inventory),
                Err(EventError::TrajectoryExists) => runtime.session(root, root).and_then(continued),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    };
    match observed {
        Ok(()) => Ok(()),
        Err(error) => Err(refused_hook(runtime, root, event, error)),
    }
}

/// One hook refused before it was dispatched, recorded against the trajectory it was about.
/// A proposal refused on a fact it could not obtain is denied on it, as at dispatch.
fn refused_hook(runtime: &Runtime, root: &TrajectoryId, event: &HookEvent, error: EventError) -> Answered {
    let (kind, tool) = hook_shape(event);
    let (status, decision) = match (event, error.missing_fact()) {
        (HookEvent::ToolCall { .. }, Some(retry)) => (200, missing_call(retry)),
        _ => (409, refuse(error)),
    };
    runtime.record(Some(root), bare_hook(kind, hook_result(&decision).0, tool));
    (status, wire(&decision))
}

/// A call whose arguments name a child of this family is refused before it runs: a
/// subagent's words reach its parent through the checked return only.
fn names_a_childs_transcript(
    runtime: &Runtime,
    event: &HookEvent,
    root: &TrajectoryId,
    names_children: &[TrajectoryId],
) -> Option<Answered> {
    let HookEvent::ToolCall { actor, call, .. } = event else {
        return None;
    };
    let named = runtime
        .opened_among(&actor.root, names_children)
        .and_then(|child| match child {
            Some(child) => runtime
                .malformed_proposal(&actor.root, call)
                .map(|malformed| Some((child, malformed))),
            None => Ok(None),
        });
    let (status, decision) = match named {
        Ok(Some((_, Some(feedback)))) => (200, deny_call(feedback, Outcomes::Integrity)),
        Ok(Some((child, None))) => {
            tracing::debug!(root = %actor.root.0, child = %child.0, "a call names a family child's transcript");
            (200, denied(NAMED_TRANSCRIPT.to_string(), Outcomes::HardBlock))
        }
        Ok(None) => return None,
        Err(error) => (409, refuse(error)),
    };
    let (outcome, offers) = hook_result(&decision);
    runtime.record(
        Some(root),
        crate::events::RuntimeEvent::Hook {
            event: crate::events::HookKind::ToolCall,
            tool: Some(call.tool.clone()),
            dispatch: None,
            outcome,
            offers,
        },
    );
    Some((status, wire(&decision)))
}

/// One hook's diagnostic entry with nothing to say beyond how it ended: no dispatch was
/// opened and no offer was made. Recorded deployment-wide where the hook ended before an
/// actor existed, and against the trajectory where one did.
fn bare_hook(
    event: crate::events::HookKind,
    outcome: crate::events::HookOutcome,
    tool: Option<String>,
) -> crate::events::RuntimeEvent {
    crate::events::RuntimeEvent::Hook {
        event,
        tool,
        dispatch: None,
        outcome,
        offers: Vec::new(),
    }
}

/// The root this event's diagnostic entry is filed under.
///
/// The *root*, never the acting trajectory: the event log is keyed by family, because that is
/// the unit a report is about and the unit its per-list bound must apply to. Filing a
/// subagent's hooks under the subagent would put them outside the family's own account and
/// leave `recent_root` naming something no log can be read for.
fn hook_root(event: &HookEvent) -> &TrajectoryId {
    match event {
        HookEvent::SessionStart { root, .. } => root,
        HookEvent::ChildStart { root, .. }
        | HookEvent::ChildEnd { root, .. }
        | HookEvent::ChildTurnEnd { root, .. }
        | HookEvent::ChildReturn { root, .. } => root,
        HookEvent::Prompt { actor, .. }
        | HookEvent::TurnEnd { actor, .. }
        | HookEvent::ToolCall { actor, .. }
        | HookEvent::SpawnResume { actor, .. }
        | HookEvent::ToolResult { actor, .. }
        | HookEvent::SpawnResult { actor, .. } => &actor.root,
    }
}

/// The one return a subagent whose return APPA cannot check may end on. It carries nothing,
/// so letting its stop through ends the subagent rather than holding it in a stop no return
/// can pass.
pub const WITHHELD_RETURN: &str = "[OpenAPPA withheld this subagent's return]";

pub(crate) fn is_withheld_return(said: Option<&str>) -> bool {
    said.map(str::trim) == Some(WITHHELD_RETURN)
}

pub(crate) fn withheld_return_instruction(cause: &str) -> String {
    format!(
        "APPA cannot check this subagent's return ({cause}). Stop now: reply with exactly this line and nothing \
         else: {WITHHELD_RETURN}"
    )
}

/// What a cleared session is told when its family could not start over.
const CLEAR_KEPT: &str = "APPA kept this session's label across /clear: a subagent or a call it started is still \
                          running or owed. Run /clear again after it finishes to start with a clean label.";

/// The runtime's reporting tool as a policy names it.
#[cfg(feature = "daemon")]
const YELL_TOOL: &str = "mcp/appa/yell";

/// The reporting tool a block names. The runtime's MCP server serves `yell` only when the
/// deployment lets an agent report, so a block names it only then, spelled the way this
/// host calls that server. A build without the server names none.
#[cfg(feature = "daemon")]
fn served_yell_tool(runtime: &Runtime) -> Option<String> {
    runtime.agent_yell().then(|| runtime.model_spelling(YELL_TOOL))
}

#[cfg(not(feature = "daemon"))]
fn served_yell_tool(_runtime: &Runtime) -> Option<String> {
    None
}

/// A subagent's words reach its parent through the checked return only; a call that
/// names the file the harness keeps them in is refused before it runs.
const NAMED_TRANSCRIPT: &str = "this call names a subagent's transcript or output file; a subagent's words \
                                reach this session only through its checked return";

/// Dispatch one typed event and answer it, without the wire around it: no body to validate,
/// no inventory to take in, and no entry recorded.
///
/// The wrapper every adapter, `replay`, and the MCP endpoint calls: it drops the diagnostic
/// entry that [`handle_internal`] also builds. Only `answer` — the one path a live harness
/// reaches — keeps it.
pub async fn handle(runtime: &Runtime, event: HookEvent) -> HookDecision {
    handle_internal(
        runtime,
        event,
        EmbeddedPresentationOptions::default(),
        ToolCallIds::Unnamed,
    )
    .await
    .decision
}

/// Dispatches one hook for an embedded host, returning a typed `EmbeddedHookOutcome`.
pub async fn handle_embedded(runtime: &Runtime, event: HookEvent) -> EmbeddedHookOutcome {
    handle_embedded_with_options(runtime, event, EmbeddedPresentationOptions::default()).await
}

/// Dispatches an embedded hook with request-scoped presentation options.
pub async fn handle_embedded_with_options(
    runtime: &Runtime,
    event: HookEvent,
    presentation: EmbeddedPresentationOptions,
) -> EmbeddedHookOutcome {
    let handled = handle_internal(runtime, event, presentation, ToolCallIds::Unnamed).await;
    EmbeddedHookOutcome {
        decision: handled.decision,
        presentation: handled.presentation,
    }
}

/// One dispatched hook: the decision the adapters render, and the entry the runtime keeps
/// about it.
///
/// The two travel together because the dispatch id belongs to exactly one of them.
/// `HookDecision` is the public adapter wire type and carries no id — an adapter must not
/// see one — but a diagnostic that cannot tie a hook to the dispatch it opened cannot be
/// correlated with the trajectory's own facts. So the id is picked up here, where the
/// release is still in scope, and never crosses the adapter boundary.
pub(crate) struct Handled {
    pub(crate) decision: HookDecision,
    pub(crate) presentation: Option<RemedyPresentation>,
    pub(crate) event: crate::events::RuntimeEvent,
}

/// Whether the host's MCP client names, in each tool call it sends this runtime, the call id its
/// hook carried. Claude Code's does, in the request's `_meta`. A declaration ahead of the spawn
/// is then vouched under that one call, so two sessions declaring alike never share its key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolCallIds {
    Named,
    Unnamed,
}

pub(crate) async fn handle_internal(
    runtime: &Runtime,
    event: HookEvent,
    presentation_options: EmbeddedPresentationOptions,
    call_ids: ToolCallIds,
) -> Handled {
    let shape = hook_shape(&event);
    let mut dispatch = None;
    let mut presentation = None;
    let decision = dispatch_event(
        runtime,
        event,
        &mut dispatch,
        &mut presentation,
        &presentation_options,
        call_ids,
    )
    .await;
    handled(shape, dispatch, presentation, decision)
}

fn handled(
    (kind, tool): (crate::events::HookKind, Option<String>),
    dispatch: Option<EngineDispatchId>,
    presentation: Option<RemedyPresentation>,
    decision: HookDecision,
) -> Handled {
    let presentation = presentation.or_else(|| presentation_for(&decision));
    let (outcome, mut offers) = hook_result(&decision);
    if let Some(presentation) = &presentation {
        offers = presentation.offers.iter().map(|offer| offer.id.clone()).collect();
    }
    Handled {
        presentation,
        event: crate::events::RuntimeEvent::Hook {
            event: kind,
            tool,
            dispatch,
            outcome,
            offers,
        },
        decision,
    }
}

/// Which hook this is, and the tool it names where it names one.
fn hook_shape(event: &HookEvent) -> (crate::events::HookKind, Option<String>) {
    use crate::events::HookKind;
    match event {
        HookEvent::SessionStart { .. } => (HookKind::SessionStart, None),
        HookEvent::Prompt { .. } => (HookKind::Prompt, None),
        HookEvent::TurnEnd { .. } => (HookKind::TurnEnd, None),
        HookEvent::ToolCall { call, .. } => (HookKind::ToolCall, Some(call.tool.clone())),
        HookEvent::SpawnResume { call, .. } => (HookKind::SpawnResume, Some(call.tool.clone())),
        HookEvent::ToolResult { call, .. } => (HookKind::ToolResult, Some(call.tool.clone())),
        HookEvent::ChildStart { .. } => (HookKind::ChildStart, None),
        HookEvent::ChildEnd { .. } | HookEvent::ChildTurnEnd { .. } => (HookKind::ChildEnd, None),
        HookEvent::ChildReturn { .. } => (HookKind::ChildReturn, None),
        HookEvent::SpawnResult { call, .. } => (HookKind::SpawnResult, Some(call.tool.clone())),
    }
}

/// How it was answered. `offers` are the remedy ids a block named, so a report can say
/// which way out was on the table.
fn hook_result(decision: &HookDecision) -> (crate::events::HookOutcome, Vec<String>) {
    use crate::events::HookOutcome;
    match decision {
        HookDecision::Ack
        | HookDecision::Context { .. }
        | HookDecision::ChildReturn { .. }
        | HookDecision::DeliverValue { .. } => (HookOutcome::Acked, Vec::new()),
        HookDecision::AllowCall { .. } => (HookOutcome::Allowed, Vec::new()),
        HookDecision::PassControl => (HookOutcome::PassControl, Vec::new()),
        HookDecision::DenyCall { offers, .. } => (
            HookOutcome::Denied,
            offers.iter().map(|offered| offered.id.clone()).collect(),
        ),
        HookDecision::Block { .. } | HookDecision::ReplaceOutput { .. } => (HookOutcome::Blocked, Vec::new()),
        HookDecision::Refuse { .. } => (HookOutcome::Refused, Vec::new()),
        HookDecision::Held { .. } => (HookOutcome::Held, Vec::new()),
        HookDecision::DeferReturn => (HookOutcome::Deferred, Vec::new()),
    }
}

/// Dispatch one typed event to its session and fold the outcome into
/// one decision. The dispatcher holds nothing between calls; every id
/// it needs is in the event or in the runtime's persistence.
async fn dispatch_event(
    runtime: &Runtime,
    event: HookEvent,
    dispatch: &mut Option<EngineDispatchId>,
    presentation: &mut Option<RemedyPresentation>,
    presentation_options: &EmbeddedPresentationOptions,
    call_ids: ToolCallIds,
) -> HookDecision {
    let mut dispatcher = Dispatcher {
        runtime,
        dispatch,
        presentation,
        options: presentation_options,
        call_ids,
    };
    match event {
        HookEvent::SessionStart {
            root,
            principal,
            address,
            title,
            start,
            launch,
            settings,
        } => {
            // As in `answer`, a host id another family continues under starts that family.
            let decided = launch
                .map(|launch| runtime.decide_launch(launch, &settings))
                .transpose()
                .and_then(|deciding| Ok((runtime.alias_of(&root)?, deciding)));
            match decided {
                Ok((family, deciding)) => dispatcher.session_start(
                    family.unwrap_or(root),
                    principal,
                    settings,
                    address,
                    title,
                    start,
                    deciding,
                ),
                Err(error) => refuse(error),
            }
        }
        HookEvent::Prompt {
            actor,
            text,
            settles,
            peer,
            title,
        } => dispatcher.prompt(actor, text, settles, peer, title),
        HookEvent::TurnEnd { actor, open_calls } => dispatcher.turn_end(actor, open_calls).await,
        HookEvent::ToolCall {
            actor,
            call,
            call_id,
            spawn,
            prompt,
            ruling,
            declares,
        } => {
            // Only a single spawn declares its return: a declaration on any other call, the
            // control call included, is refused here, before the call writes anything.
            let proposing = match (spawn, declares) {
                (spawn, None) => Proposing::Plain(spawn),
                (Some(SpawnKind::Single), Some(declares)) if !is_control_tool(&call.tool) => {
                    Proposing::Declaring(declares)
                }
                (_, Some(_)) => return refuse(EventError::DeclaresOffSpawn),
            };
            dispatcher
                .tool_call(actor, call, call_id, proposing, prompt, ruling)
                .await
        }
        HookEvent::SpawnResume { actor, call, child } => dispatcher.spawn_resume(actor, call, child).await,
        HookEvent::ToolResult {
            actor,
            call,
            call_id,
            outcome,
        } => dispatcher.tool_result(actor, call, call_id, outcome).await,
        HookEvent::SpawnResult {
            actor,
            call,
            call_id,
            outcome,
            child,
            value,
        } => {
            dispatcher
                .spawn_result(actor, call, call_id, outcome, child, value)
                .await
        }
        HookEvent::ChildStart { root, child, spawn } => dispatcher.child_start(root, child, spawn),
        HookEvent::ChildEnd {
            root,
            child,
            value,
            open_calls,
        } => dispatcher.child_end(root, child, value, open_calls).await,
        HookEvent::ChildTurnEnd {
            root,
            child,
            open_calls,
        } => dispatcher.child_turn_end(root, child, open_calls).await,
        HookEvent::ChildReturn { root, child, value } => dispatcher.child_return(root, child, value).await,
    }
}

/// What a proposed call starts as the host identified it, and the return a single spawn
/// declares with it.
#[derive(Debug, Clone)]
enum Proposing {
    Plain(Option<SpawnKind>),
    Declaring(appa_runtime_api::DeclaredReturn),
}

impl Proposing {
    fn spawn(&self) -> Option<SpawnKind> {
        match self {
            Proposing::Plain(spawn) => *spawn,
            Proposing::Declaring(_) => Some(SpawnKind::Single),
        }
    }
}

/// One event's dispatch: the runtime it runs against, what it may write back to the caller
/// besides its decision, and how an embedded harness wants a presentation rendered. It
/// lives for one event, so every method below takes exactly that hook's own payload.
struct Dispatcher<'a> {
    runtime: &'a Runtime,
    /// The engine dispatch a released call opened.
    dispatch: &'a mut Option<EngineDispatchId>,
    /// What a denial or a replacement carries for an embedded harness.
    presentation: &'a mut Option<RemedyPresentation>,
    options: &'a EmbeddedPresentationOptions,
    call_ids: ToolCallIds,
}

impl Dispatcher<'_> {
    /// A start that names the address its launcher bound makes the family reachable by peer
    /// messages. An identity that did not land leaves the family unreachable, which only
    /// refuses sends to it, so the start goes on. A launched start opens the family its
    /// launch runs, which a host id the launch continues under does not change; it holds
    /// its launch's `deciding` through the identity it records.
    #[allow(clippy::too_many_arguments)]
    fn session_start(
        &mut self,
        root: TrajectoryId,
        principal: Option<String>,
        settings: SessionSettings,
        address: Option<PeerAddress>,
        title: Option<SessionTitle>,
        start: Option<StartKind>,
        launch: Option<Deciding<'_>>,
    ) -> HookDecision {
        let runtime = self.runtime;
        let launched = match (&principal, &launch) {
            (_, None) => Ok(LaunchedStart::Opened(root)),
            (None, Some(deciding)) => runtime.launched_start(&root, start, &settings, deciding),
            (Some(_), Some(_)) => Err(EventError::PrincipalMismatch),
        };
        let launched = match launched {
            Ok(launched) => launched,
            Err(error) => return refuse(error),
        };
        let root = launched.root().clone();
        let opened = session_principal(principal.as_deref())
            .and_then(|principal| open_or_reopen(runtime, &root, principal, &settings, self.options.clone()));
        if opened.is_ok()
            && let Err(error) = runtime.record_peer_identity(&root, address.as_ref(), title.as_ref())
        {
            tracing::warn!(root = %root.0, %error, "the session's peer identity was not recorded");
        }
        // The rest only reads, and another start of the launch need not wait on it.
        drop(launch);
        let context = [matches!(launched, LaunchedStart::ClearKept(_)).then_some(CLEAR_KEPT.to_owned())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        match opened.and_then(|_| runtime.live(&root, &root)) {
            Ok(()) if context.is_empty() => HookDecision::Ack,
            Ok(()) => HookDecision::Context {
                text: context.join("\n\n"),
            },
            Err(error) => refuse(error),
        }
    }

    /// The prompt text is not an engine event: nothing is reported, no fact is recorded,
    /// and offer freshness stays the engine's judgment. The prompt is only marked as the
    /// sign that the previous turn is over; a queued message arrives here while its turn's
    /// call still runs, so the call is settled at the first proposal of the new turn, when
    /// that result is in.
    ///
    /// The mark gates nothing, so this answers `Ack` whether or not it landed: a mark that
    /// did not land leaves the interrupted call open until the turn ends, which is what
    /// happens anyway when no prompt hook arrives at all.
    /// A prompt that reports a background call finished also closes that call.
    ///
    /// A peer frame is not the user's turn: it may arrive in the middle of one, so it leaves
    /// no mark. It enters only when it does not narrow the session; otherwise it is held and
    /// the prompt blocked, and a message that can be neither refuses the prompt.
    fn prompt(
        &mut self,
        actor: Actor,
        text: String,
        settles: Option<String>,
        peer: Option<PeerFrame>,
        title: Option<SessionTitle>,
    ) -> HookDecision {
        if title.is_some()
            && let Err(error) = self.runtime.record_peer_identity(&actor.root, None, title.as_ref())
        {
            tracing::warn!(root = %actor.root.0, %error, "the session's title was not recorded");
        }
        let decision = match peer {
            Some(frame) => match self.runtime.receive_peer(&actor.root, &frame, &text) {
                Ok(Received::Direct) => HookDecision::Ack,
                Ok(Received::Held(notice)) => HookDecision::Block {
                    reason: format!(
                        "[appa] Peer message {} is held: taking it in would narrow this session, or no \
                         protected sender stands behind it. The model can read it with read_peer_message.",
                        notice.id.as_str()
                    ),
                    outcomes: None,
                },
                Err(error) => {
                    tracing::warn!(root = %actor.root.0, %error, "the peer message was neither admitted nor held");
                    return refuse(error);
                }
            },
            None => {
                if let Err(error) = self.runtime.record_prompt(&actor) {
                    tracing::warn!(root = %actor.root.0, %error, "the prompt left no mark");
                }
                HookDecision::Ack
            }
        };
        if let Some(call_id) = settles
            && let Err(error) = self.runtime.record_call_settled(&actor, call_id)
        {
            tracing::warn!(root = %actor.root.0, %error, "the finished call was not recorded");
        }
        with_peer_notices(self.runtime, &actor, decision)
    }

    /// A turn end gates nothing, so it answers `Ack` whatever happens. The refusal families
    /// both mean "do not end the turn" on this hook, which would hold the harness in a turn
    /// it has finished; a close that failed leaves the call open and the next proposal
    /// refuses on its own.
    async fn turn_end(&mut self, actor: Actor, open_calls: OpenCalls) -> HookDecision {
        match on_actor(self.runtime, &actor, MissingStart::Refuse, self.options, |session| {
            let open_calls = open_calls.clone();
            async move {
                match &open_calls {
                    OpenCalls::Abandon => session.on_turn_end().await.map(Stop::Taken),
                    OpenCalls::Hold { answered } => session.on_turn_end_holding(answered).await,
                }
            }
        })
        .await
        {
            Ok(Stop::Held(running)) => return HookDecision::Held { running },
            Ok(Stop::Taken(())) => {}
            Err(error) => {
                tracing::warn!(root = %actor.root.0, %error, "the turn end closed no abandoned call");
            }
        }
        if let Err(error) = self.runtime.record_turn_end(&actor, EndKind::Boundary) {
            tracing::warn!(root = %actor.root.0, %error, "the turn's end was not recorded");
        }
        HookDecision::Ack
    }

    async fn tool_call(
        &mut self,
        actor: Actor,
        call: ProposedCall,
        call_id: Option<String>,
        proposing: Proposing,
        prompt: Option<PromptKey>,
        ruling: Option<Ruling>,
    ) -> HookDecision {
        // No host call id is empty; refused before the settle below closes anything.
        if call_id.as_deref() == Some("") {
            return fold(EventError::CallIdReused, Refusal::Deny);
        }
        let hold_until = tokio::time::Instant::now() + SPAWN_HOLD;
        let missing_start = MissingStart::OpenLate {
            prompt: prompt.as_ref(),
        };
        if self.runtime.prompted(&actor) {
            // The first proposal after a prompt that no turn end preceded:
            // the user interrupted the previous turn, and whatever it left
            // open is settled before this turn's first call, control tools
            // included, so a vouch this turn records is never released here.
            // A settle that does not complete writes nothing, so the mark survives and
            // the next proposal tries the same close again.
            if let Err(error) = on_actor(
                self.runtime,
                &actor,
                missing_start,
                self.options,
                |session| async move { session.on_turn_end().await },
            )
            .await
            {
                return fold(error, Refusal::Deny);
            }
            // A housekeeping end: it settles what the interrupted turn left open, and leaves
            // the return this turn declared ahead to the spawn it was declared for.
            if let Err(error) = self.runtime.record_turn_end(&actor, EndKind::Housekeeping) {
                return fold(error, Refusal::Deny);
            }
        }
        if is_control_tool(&call.tool) {
            let named = call_id.as_deref().filter(|_| self.call_ids == ToolCallIds::Named);
            return control_call(self.runtime, &actor, &call, named, ruling);
        }
        let sent = match call.tool == SEND_MESSAGE {
            true => match self.runtime.peer_send(&actor.root, &call) {
                Ok(PeerSend::Judge { digest }) => Some(digest),
                Ok(PeerSend::Refused { feedback }) => return deny(feedback),
                Err(error) => return fold(error, Refusal::Deny),
            },
            false => None,
        };
        let propose = || {
            on_actor(self.runtime, &actor, missing_start, self.options, |session| {
                let call = call.clone();
                let call_id = call_id.clone();
                let prompt = prompt.clone();
                let proposing = proposing.clone();
                async move {
                    match proposing {
                        Proposing::Plain(spawn) => session.on_tool_call_identified(call, call_id, spawn, prompt).await,
                        Proposing::Declaring(declares) => {
                            session.on_spawn_declaring(call, call_id, prompt, declares).await
                        }
                    }
                }
            })
        };
        let decided = match (proposing.spawn(), propose().await) {
            (Some(SpawnKind::Single), Err(EventError::SpawnOutstanding)) => {
                self.runtime.hold_spawn(&actor.root, hold_until, propose).await
            }
            (_, decided) => decided,
        };
        match decided {
            Ok(ToolCallDecision::Allow {
                spawn,
                dispatch: opened,
            }) => {
                // The send is recorded before it is released: a message that arrives with no
                // record is held, which is not how a released send should land.
                if let Some(digest) = sent
                    && let Err(error) = self.runtime.record_peer_sent(&actor, digest, opened.clone())
                {
                    return refuse(error);
                }
                *self.dispatch = Some(opened);
                vouch_call(self.runtime, &actor, &call);
                HookDecision::AllowCall { spawn }
            }
            Ok(ToolCallDecision::Deny {
                feedback,
                outcomes,
                offers,
                display,
                review,
            }) => {
                let feedback = match sent.is_some().then(|| self.runtime.peer_hint(&actor.root, &call)) {
                    Some(Ok(Some(address))) => format!(
                        "{feedback}\n[appa] A protected session under this policy uses that name. To send \
                         at this session's label, set `to` to its address: {address}"
                    ),
                    _ => feedback,
                };
                *self.presentation = Some(RemedyPresentation {
                    feedback: feedback.clone(),
                    outcomes: Some(outcomes),
                    offers: offers.clone(),
                    review: review.clone(),
                    display,
                });
                HookDecision::DenyCall {
                    feedback,
                    offers,
                    review,
                    outcomes: Some(outcomes),
                }
            }
            Err(error) => fold(error, Refusal::Deny),
        }
    }

    async fn spawn_resume(&mut self, actor: Actor, call: ProposedCall, child: TrajectoryId) -> HookDecision {
        match on_actor(self.runtime, &actor, MissingStart::Refuse, self.options, |session| {
            let (call, child) = (call.clone(), child.clone());
            async move { session.on_spawn_resume(call, child) }
        })
        .await
        {
            Ok(()) => {
                // The resume settled what the prompt interrupted, and only that: the
                // turn's standing survives a resume it approved.
                if self.runtime.prompted(&actor)
                    && let Err(error) = self.runtime.record_prompt_settled(&actor)
                {
                    tracing::warn!(root = %actor.root.0, %error, "the resumed prompt's mark stands");
                }
                HookDecision::Ack
            }
            Err(error) => fold(error, Refusal::Settle),
        }
    }

    async fn tool_result(
        &mut self,
        actor: Actor,
        call: ProposedCall,
        call_id: Option<String>,
        outcome: ToolOutcome,
    ) -> HookDecision {
        if is_control_tool(&call.tool) {
            tracing::debug!(trajectory = %actor.root.0, "control tool outcome absorbed");
            return HookDecision::Ack;
        }
        let failed = matches!(outcome, ToolOutcome::Failure { .. });
        match on_actor(self.runtime, &actor, MissingStart::Refuse, self.options, |session| {
            let (call, call_id, outcome) = (call.clone(), call_id.clone(), outcome.clone());
            async move { session.on_tool_result_identified(call, call_id, outcome).await }
        })
        .await
        {
            // A failed call's hook carries no context, so a notice waits for the next one.
            Ok(decision) if !failed => {
                with_peer_notices(self.runtime, &actor, outcome_decision(decision, self.presentation))
            }
            Ok(decision) => outcome_decision(decision, self.presentation),
            Err(error) => fold(error, Refusal::Settle),
        }
    }

    async fn spawn_result(
        &mut self,
        actor: Actor,
        call: ProposedCall,
        call_id: Option<String>,
        outcome: ToolOutcome,
        child: Option<TrajectoryId>,
        value: Option<String>,
    ) -> HookDecision {
        let said = value.clone();
        match on_actor(self.runtime, &actor, MissingStart::Refuse, self.options, |session| {
            let (call, call_id, outcome, child, value) = (
                call.clone(),
                call_id.clone(),
                outcome.clone(),
                child.clone(),
                value.clone(),
            );
            async move {
                session
                    .on_spawn_result_identified(call, call_id, outcome, child, value)
                    .await
            }
        })
        .await
        {
            Ok(SpawnResultDecision::Return(decision)) => return_decision(said, decision),
            Ok(SpawnResultDecision::Outcome(decision)) => outcome_decision(decision, self.presentation),
            Err(error) => fold(error, Refusal::Settle),
        }
    }

    /// The child is told what its return must look like where the fork's policy shapes it;
    /// a return that crosses as spoken needs no word.
    fn child_start(&mut self, root: TrajectoryId, child: TrajectoryId, spawn: SpawnRef) -> HookDecision {
        let root = match self
            .runtime
            .session_with_presentation(&root, &root, self.options.clone())
        {
            Ok(session) => session,
            Err(error) => return refuse(error),
        };
        match root.start_child(child, spawn) {
            Ok((_, Some(text))) => HookDecision::Context { text },
            Ok((_, None)) => HookDecision::Ack,
            Err(error) => refuse(error),
        }
    }

    async fn child_turn_end(&mut self, root: TrajectoryId, child: TrajectoryId, open_calls: OpenCalls) -> HookDecision {
        match on_child(
            self.runtime,
            &root,
            &child,
            MissingStart::Refuse,
            self.options,
            |session| {
                let open_calls = open_calls.clone();
                async move {
                    match &open_calls {
                        OpenCalls::Abandon => session.on_child_turn_end().await.map(Stop::Taken),
                        OpenCalls::Hold { answered } => session.on_child_turn_end_holding(answered).await,
                    }
                }
            },
        )
        .await
        {
            Ok(Stop::Held(running)) => HookDecision::Held { running },
            Ok(Stop::Taken(decision)) => {
                let actor = Actor {
                    root,
                    child: Some(child),
                };
                if let Err(error) = self.runtime.record_turn_end(&actor, EndKind::Boundary) {
                    tracing::warn!(%error, "the empty child turn's end was not recorded");
                }
                return_decision(None, decision)
            }
            Err(error) => refuse(error),
        }
    }

    /// The subagent's return is checked here and blocked when it may not cross; a block
    /// keeps the subagent running until it returns what may. A child the family never saw
    /// start is blocked too: its return has no fork to cross on.
    async fn child_end(
        &mut self,
        root: TrajectoryId,
        child: TrajectoryId,
        value: Option<String>,
        open_calls: OpenCalls,
    ) -> HookDecision {
        let said = value.clone();
        match on_child(
            self.runtime,
            &root,
            &child,
            MissingStart::Refuse,
            self.options,
            |session| {
                let value = value.clone();
                let open_calls = open_calls.clone();
                async move {
                    match &open_calls {
                        OpenCalls::Abandon => session.on_child_end(value).await.map(Stop::Taken),
                        OpenCalls::Hold { answered } => session.on_child_end_holding(value, answered).await,
                    }
                }
            },
        )
        .await
        {
            Ok(Stop::Held(running)) => HookDecision::Held { running },
            Ok(Stop::Taken(decision)) => {
                // The child is finished unless it is being asked to return something
                // else, so what it still stands behind ends here: its vouches must not
                // outlive its return, and its parent's turn end is not its own.
                if !matches!(decision, ChildReturnDecision::Blocked { .. }) {
                    let actor = Actor {
                        root: root.clone(),
                        child: Some(child.clone()),
                    };
                    if let Err(error) = self.runtime.record_turn_end(&actor, EndKind::Boundary) {
                        tracing::warn!(root = %root.0, child = %child.0, %error, "the child's end was not recorded");
                    }
                }
                return_decision(said, decision)
            }
            Err(error) if error.is_operational() => refuse(error),
            Err(_) if is_withheld_return(said.as_deref()) => HookDecision::Ack,
            Err(error) => block(withheld_return_instruction(&error.to_string()), Outcomes::Integrity),
        }
    }

    /// A return the subagent makes without ending: it crosses exactly as a stop's return would,
    /// and the subagent runs on whatever the answer.
    async fn child_return(&mut self, root: TrajectoryId, child: TrajectoryId, value: String) -> HookDecision {
        let said = Some(value.clone());
        match on_child(
            self.runtime,
            &root,
            &child,
            MissingStart::Refuse,
            self.options,
            |session| {
                let value = value.clone();
                async move { session.on_child_return(value).await }
            },
        )
        .await
        {
            Ok(decision) => return_decision(said, decision),
            Err(error) => fold(error, Refusal::Block),
        }
    }
}

fn outcome_decision(
    decision: ToolResultDecision,
    embedded_presentation: &mut Option<RemedyPresentation>,
) -> HookDecision {
    match decision {
        ToolResultDecision::Keep => HookDecision::Ack,
        ToolResultDecision::Deliver { value } => HookDecision::DeliverValue { value },
        ToolResultDecision::Replace {
            placeholder,
            presentation,
        } => {
            let outcomes = presentation.as_ref().and_then(|presentation| presentation.outcomes);
            *embedded_presentation = presentation;
            HookDecision::ReplaceOutput {
                output: placeholder,
                outcomes,
            }
        }
    }
}

fn presentation_for(decision: &HookDecision) -> Option<RemedyPresentation> {
    match decision {
        HookDecision::DenyCall {
            feedback,
            offers,
            review,
            outcomes,
        } => Some(RemedyPresentation {
            feedback: feedback.clone(),
            outcomes: *outcomes,
            offers: offers.clone(),
            review: review.clone(),
            display: Vec::new(),
        }),
        _ => None,
    }
}

/// A crossing as spoken answers with no opinion. What crosses otherwise — the canonical
/// form of a shaped return, a sanitizer's derivation — goes back as the value the child
/// must return, or the harness deliver, for it to reach the parent.
fn return_decision(said: Option<String>, decision: ChildReturnDecision) -> HookDecision {
    match decision {
        ChildReturnDecision::Returned { value } if said.as_deref() != Some(value.as_str()) => {
            HookDecision::ChildReturn { value }
        }
        ChildReturnDecision::Staged { value } => HookDecision::ChildReturn { value },
        ChildReturnDecision::Returned { .. } | ChildReturnDecision::NoValue => HookDecision::Ack,
        ChildReturnDecision::Deferred => HookDecision::DeferReturn,
        ChildReturnDecision::Blocked { feedback, outcomes } => block(feedback, outcomes),
    }
}

/// The host's named principal, held to the one shape a principal takes.
fn session_principal(spelling: Option<&str>) -> Result<Option<ReaderId>, EventError> {
    spelling
        .map(|spelling| {
            appa_engine::audience::session_principal(spelling)
                .ok_or_else(|| EventError::MalformedPrincipal(spelling.to_string()))
        })
        .transpose()
}

/// Open the root for `principal`, pinning `settings`, or reopen it when the principal it
/// opened for is the one named. A reopen that names none continues under the principal the
/// opening pinned, and every reopen under the settings it pinned.
fn open_or_reopen(
    runtime: &Runtime,
    root: &appa_runtime_api::TrajectoryId,
    principal: Option<ReaderId>,
    settings: &SessionSettings,
    presentation: EmbeddedPresentationOptions,
) -> Result<Session, EventError> {
    let session = match runtime.session_with_presentation(root, root, presentation.clone()) {
        Err(EventError::UnknownTrajectory) => {
            match runtime.create_pinned_session(root.clone(), principal.clone(), settings.clone()) {
                Ok(_) | Err(EventError::TrajectoryExists) => {
                    runtime.session_with_presentation(root, root, presentation)?
                }
                Err(error) => return Err(error),
            }
        }
        reopened => reopened?,
    };
    continues_for(&session, principal.as_ref(), settings)?;
    Ok(session)
}

/// A start that names a principal continues a session only when that session opened for it,
/// and one that names a line variant only when the session opened under no other one.
fn continues_for(
    session: &Session,
    principal: Option<&ReaderId>,
    settings: &SessionSettings,
) -> Result<(), EventError> {
    if let Some(named) = principal
        && session.principal()?.as_ref() != Some(named)
    {
        return Err(EventError::PrincipalMismatch);
    }
    match &settings.line_variant {
        Some(named) if !session.settings()?.continued_by(Some(named)) => Err(EventError::LineVariantMismatch),
        _ => Ok(()),
    }
}

/// `named` is the call's id where the host's MCP client names it to the tool as well.
fn control_call(
    runtime: &Runtime,
    actor: &Actor,
    call: &ProposedCall,
    named: Option<&str>,
    ruling: Option<Ruling>,
) -> HookDecision {
    let quoted = match quoted_offer(call) {
        Ok(Some(Quoted::Offer(quoted))) => quoted,
        // A decision on a deferred return passes on the caller's ownership of that return, read
        // again where the decision lands. Nothing is vouched: no offer stands behind it.
        Ok(Some(Quoted::Return(reference))) => {
            return match runtime.owns_return(actor, &reference) {
                true => HookDecision::PassControl,
                false => deny("this offer no longer stands; re-propose the call".to_string()),
            };
        }
        Ok(None) => {
            // A declaration ahead of the spawn quotes no offer: its arguments and the call name
            // it. It can be a root's first call, which opens the root as any root call does.
            if let Ok(args) = serde_json::from_str::<crate::api::ExecuteRemedyPlanArgs>(call.arguments.get()) {
                if actor.child.is_none()
                    && let Err(error) = open_or_reopen(
                        runtime,
                        &actor.root,
                        None,
                        &SessionSettings::default(),
                        EmbeddedPresentationOptions::default(),
                    )
                {
                    return fold(error, Refusal::Deny);
                }
                // Unlike an offer's vouch, a declaration's that did not land is reported: its
                // execution would otherwise answer as one no hook vouched for.
                if let Err(error) = runtime.record_vouch(&crate::api::declaration_key(&args, named), actor, ruling) {
                    return fold(error, Refusal::Deny);
                }
            }
            tracing::debug!(trajectory = %actor.root.0, "control tool quotes no offer id");
            return HookDecision::PassControl;
        }
        Err(reason) => {
            tracing::debug!(trajectory = %actor.root.0, ?reason, "control tool quotes a malformed offer id");
            return deny("this offer no longer stands; re-propose the call".to_string());
        }
    };
    let acting = actor.child.clone().unwrap_or_else(|| actor.root.clone());
    match runtime.resolve_in(&actor.root, &quoted) {
        Some((_, pursuer)) if pursuer == acting => {
            runtime.vouch(&crate::api::PermitKey::offer(&quoted), actor, ruling);
            tracing::debug!(trajectory = %acting.0, "control tool names an offer this trajectory pursues");
            HookDecision::PassControl
        }
        _ => {
            tracing::debug!(trajectory = %acting.0, "control tool refused: no such offer here");
            deny("this offer no longer stands; re-propose the call".to_string())
        }
    }
}

/// Record who is making a released call to a tool this runtime serves — `yell`, or one of
/// the management tools — so the tool, whose MCP request names no session, acts for the
/// session the hook saw rather than whichever one this machine ran most recently.
///
/// Only a released call: the vouch is what lets the tool act at all, so a call the policy
/// blocked must leave nothing behind for the tool to spend. Unlike the control tool, these
/// are ordinary checked calls — flows like any other, and the policy decides them first. A
/// lookalike on another server is an ordinary tool this never vouches for.
fn vouch_call(runtime: &Runtime, actor: &Actor, call: &ProposedCall) {
    let Some(key) = crate::api::call_key(call) else {
        return;
    };
    runtime.vouch(&key, actor, None);
    tracing::debug!(trajectory = %actor.root.0, tool = %call.tool, "vouched for this trajectory");
}

fn quoted_offer(call: &ProposedCall) -> Result<Option<Quoted>, crate::api::OfferIdRefusal> {
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(call.arguments.get()) else {
        return Ok(None);
    };
    let Some(offer_id) = arguments.get("offer_id").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    Quoted::parse(offer_id).map(Some)
}

/// What a child's event does when the family has not opened that child.
/// A subagent's first tool call can overtake its start hook, so the
/// call opens the child against the one spawn in flight. Every other
/// event of an unopened child is refused: a return or a turn end claims
/// nothing a missing start could supply, and opening a child on its end
/// would let a stop the family never saw start cross a value.
#[derive(Clone, Copy)]
enum MissingStart<'a> {
    /// A fan-out spawn's child names the prompt its spawn was released under.
    OpenLate {
        prompt: Option<&'a PromptKey>,
    },
    Refuse,
}

async fn on_actor<T, Run>(
    runtime: &Runtime,
    actor: &Actor,
    missing_start: MissingStart<'_>,
    presentation: &EmbeddedPresentationOptions,
    event: impl Fn(Session) -> Run,
) -> Result<T, EventError>
where
    Run: Future<Output = Result<T, EventError>>,
{
    match &actor.child {
        Some(child) => on_child(runtime, &actor.root, child, missing_start, presentation, event).await,
        None => {
            event(open_or_reopen(
                runtime,
                &actor.root,
                None,
                &SessionSettings::default(),
                presentation.clone(),
            )?)
            .await
        }
    }
}

async fn on_child<T, Run>(
    runtime: &Runtime,
    root: &TrajectoryId,
    child: &TrajectoryId,
    missing_start: MissingStart<'_>,
    presentation: &EmbeddedPresentationOptions,
    event: impl Fn(Session) -> Run,
) -> Result<T, EventError>
where
    Run: Future<Output = Result<T, EventError>>,
{
    // A child names a family that exists: its root opened before any spawn could start it.
    let root_session = runtime.session_with_presentation(root, root, presentation.clone())?;
    match (
        event(runtime.session_with_presentation(root, child, presentation.clone())?).await,
        missing_start,
    ) {
        (Err(EventError::SpawnNotTaken), MissingStart::OpenLate { prompt }) => {
            // `AlreadyOpen` means the start hook landed between the two
            // attempts; the event now finds its child, so it is not
            // refused for a race the harness has already resolved.
            match root_session.open_late(child.clone(), prompt)? {
                LateOpen::Opened => {
                    tracing::debug!(root = %root.0, child = %child.0, "a child event arrived before its start: opened the child late");
                }
                LateOpen::AlreadyOpen => {
                    tracing::debug!(root = %root.0, child = %child.0, "the child's start landed while its event was in flight");
                }
            }
            event(runtime.session_with_presentation(root, child, presentation.clone())?).await
        }
        (outcome, _) => outcome,
    }
}

/// Which refusal a policy error becomes at the hook that met it. An operational error is
/// neither: nothing was decided there, so it refuses whatever the hook was.
enum Refusal {
    /// The call has not run: it is denied, and the feedback reaches the model.
    Deny,
    /// The return is already in hand: it is blocked out of the model's way, and the child
    /// may return again.
    Block,
    /// The host settles what it holds on a block: a result's dispatch, or a resumed spawn. A
    /// fact a retry could still obtain is refused instead, so the hook can be sent again.
    Settle,
}

fn fold(error: EventError, refusal: Refusal) -> HookDecision {
    match (error.missing_fact(), error.is_operational(), refusal) {
        (Some(retry), _, Refusal::Deny) => missing_call(retry),
        (Some(retry), _, Refusal::Block) => missing_return(retry),
        (_, true, _) => refuse(error),
        (_, false, Refusal::Deny) => deny(error.to_string()),
        (_, false, Refusal::Block | Refusal::Settle) => block(error.to_string(), Outcomes::Integrity),
    }
}

/// A refusal of this dispatcher's own that the model will read names APPA
/// once, here, so a session can tell a runtime refusal from its harness's;
/// a block is marked by the adapter's withheld-result rendering instead.
fn deny(feedback: String) -> HookDecision {
    denied(feedback, Outcomes::Integrity)
}

fn denied(feedback: String, outcomes: Outcomes) -> HookDecision {
    deny_call(format!("[appa] {feedback}"), outcomes)
}

fn deny_call(feedback: String, outcomes: Outcomes) -> HookDecision {
    HookDecision::DenyCall {
        feedback,
        offers: Vec::new(),
        review: Vec::new(),
        outcomes: Some(outcomes),
    }
}

/// A call denied on a fact not obtained, in the one text a call reads.
fn missing_call(retry: RetryHint) -> HookDecision {
    deny_call(MissingIn::Call.model_text().to_string(), Outcomes::MissingFact(retry))
}

/// A return blocked on a fact not obtained, in the one text a return reads.
fn missing_return(retry: RetryHint) -> HookDecision {
    block(
        MissingIn::Value(ValueKind::Return).model_text().to_string(),
        Outcomes::MissingFact(retry),
    )
}

fn block(reason: String, outcomes: Outcomes) -> HookDecision {
    HookDecision::Block {
        reason,
        outcomes: Some(outcomes),
    }
}

/// An acknowledged root event carries the notice of peer messages held for the session.
/// Only the root is told: the messages are the root's, and a subagent's context is not.
fn with_peer_notices(runtime: &Runtime, actor: &Actor, decision: HookDecision) -> HookDecision {
    if decision != HookDecision::Ack || actor.child.is_some() {
        return decision;
    }
    match runtime.peer_notices(&actor.root) {
        Ok(Some(text)) => HookDecision::Context { text },
        Ok(None) => decision,
        Err(error) => {
            tracing::warn!(root = %actor.root.0, %error, "held peer messages were not announced");
            decision
        }
    }
}

/// A hook refused. A refusal on a fact not obtained says only that: what failed, and how,
/// stays in the diagnostics, since a harness may show the detail to its model.
fn refuse(error: EventError) -> HookDecision {
    let retry = error.missing_fact();
    let detail = match retry {
        Some(_) => {
            tracing::info!(%error, "a hook was refused on a fact not obtained");
            UNOBTAINED_REFUSAL.to_string()
        }
        None => error.to_string(),
    };
    HookDecision::Refuse {
        class: error.class(),
        detail,
        retry,
    }
}

const UNOBTAINED_REFUSAL: &str = "[appa] this hook was not decided: a fact it needs could not be obtained.";

#[cfg(test)]
mod tests {
    use super::*;
    use appa_runtime_api::SpawnRef;

    use crate::api::{OfferId, Runtime};
    use crate::config::Config;

    /// The event the served runtime reads from one Claude Code hook body: parsed by the
    /// codec and identified on the wire, exactly as [`answer`] does it, so a test can put the
    /// same event in front of the dispatcher and read what it recorded.
    fn through_the_codec(hook: &serde_json::Value) -> Option<HookEvent> {
        let body = serde_json::to_vec(hook).expect("the fixture serializes");
        let event = (appa_adapter_claude_code::codec().parse)(&body).expect("the fixture parses")?;
        let wire = WireEvent::from_event(appa_runtime_api::AdapterName::ClaudeCode, &event).expect("translates");
        let accepted = wire
            .into_event(&appa_adapter_claude_code::adapter(), PrincipalSource::None)
            .expect("the wire event is accepted")?;
        Some(accepted.event)
    }

    fn json(event: &serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(event).expect("the fixture serializes")
    }

    /// The client side of the wire, as `appa hook` runs it: the
    /// Claude Code hook JSON these tests are written in is translated onto the wire,
    /// and the wire decision is rendered back into Claude Code's hook answer.
    async fn hook(runtime: &Runtime, claude_hook_json: &[u8]) -> (u16, serde_json::Value) {
        let codec = appa_adapter_claude_code::codec();
        let event = match (codec.parse)(claude_hook_json) {
            Ok(Some(event)) => event,
            Ok(None) => return (200, serde_json::json!({})),
            Err(ParseRefusal::Unreadable { detail }) => return (400, serde_json::json!({ "error": detail })),
            Err(ParseRefusal::Malformed { detail }) => return (409, serde_json::json!({ "error": detail })),
        };
        let wire = WireEvent::from_event(appa_runtime_api::AdapterName::ClaudeCode, &event).expect("translates");
        let body = serde_json::to_vec(&wire).expect("serializes");
        let (status, answer) = answer(
            runtime,
            &appa_adapter_claude_code::adapter(),
            PrincipalSource::None,
            &body,
        )
        .await;
        match serde_json::from_value::<WireDecision>(answer.clone()).map(WireDecision::into_decision) {
            Ok(Ok(decision)) => (status, (codec.render)(claude_hook_json, &event, &decision)),
            _ => (status, answer),
        }
    }

    fn config() -> Config {
        let text = r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "host/claude-code/Bash"

            [[policy.tool]]
            name = "host/claude-code/Write"

            [[policy.tool]]
            name = "host/claude-code/AskUserQuestion"

            [[policy.tool]]
            name = "host/claude-code/Task"

            [[policy.tool]]
            name = "host/claude-code/Agent"

            [[policy.tool]]
            name = "host/claude-code/Read"

            [policy.deployment]
            context_control = true

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Config::load(&path).expect("the minimal fixture validates")
    }

    fn open_runtime(dir: &tempfile::TempDir) -> Runtime {
        Runtime::open(config(), dir.path().join("appa.db"), None).expect("the fixture deployment opens")
    }

    fn launch(token: &str) -> appa_runtime_api::LaunchStart {
        appa_runtime_api::LaunchStart {
            launch: appa_runtime_api::LaunchToken::parse(token).expect("the token parses"),
            forked_from: None,
        }
    }

    /// Claude Code's start of `session` from `source` inside `launch`, as the dispatcher reads it.
    fn launched(session: &str, source: &str, launch: appa_runtime_api::LaunchStart) -> HookEvent {
        let host = serde_json::json!({"hook_event_name": "SessionStart", "session_id": session, "source": source});
        let Some(HookEvent::SessionStart {
            root,
            principal,
            address,
            title,
            start,
            launch: None,
            settings,
        }) = through_the_codec(&host)
        else {
            panic!("a session start parses as one");
        };
        HookEvent::SessionStart {
            root,
            principal,
            address,
            title,
            start,
            launch: Some(launch),
            settings,
        }
    }

    /// The live path: the start on the wire, answered as `appa hook` posts it.
    async fn answered_start(runtime: &Runtime, event: &HookEvent) -> HookDecision {
        let wire = WireEvent::from_event(appa_runtime_api::AdapterName::ClaudeCode, event).expect("translates");
        let body = serde_json::to_vec(&wire).expect("serializes");
        let (_, answer) = answer(
            runtime,
            &appa_adapter_claude_code::adapter(),
            PrincipalSource::None,
            &body,
        )
        .await;
        serde_json::from_value::<WireDecision>(answer)
            .expect("a wire decision")
            .into_decision()
            .expect("a decision")
    }

    fn session_of(runtime: &Runtime, event: &HookEvent) -> Result<Session, EventError> {
        let root = hook_root(event);
        runtime.session(root, root)
    }

    fn contended(decision: &HookDecision) -> bool {
        matches!(
            decision,
            HookDecision::Refuse {
                class: appa_runtime_api::RefusalClass::LaunchDeciding,
                ..
            }
        )
    }

    /// While a start of a launch decides, another start of it is refused as contended and
    /// opens nothing, on the wire and embedded alike. Another launch starts alongside, and
    /// the launch starts once the first start is done.
    #[tokio::test]
    async fn a_start_overlapping_its_launchs_deciding_start_is_refused_and_opens_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let first = launch("first-launch");
        let deciding = runtime
            .decide_launch(first.clone(), &Default::default())
            .expect("the first start decides");
        assert!(matches!(
            runtime.decide_launch(first.clone(), &Default::default()),
            Err(EventError::LaunchDeciding)
        ));

        let wired = launched("0f1e2d3c-4b5a-4968-8776-655443322110", "startup", first.clone());
        assert!(contended(&answered_start(&runtime, &wired).await));
        assert!(matches!(
            session_of(&runtime, &wired),
            Err(EventError::UnknownTrajectory)
        ));
        let embedded = launched("1a2b3c4d-5e6f-4a0b-9c1d-2e3f4a5b6c7d", "startup", first);
        assert!(contended(&handle(&runtime, embedded.clone()).await));
        assert!(matches!(
            session_of(&runtime, &embedded),
            Err(EventError::UnknownTrajectory)
        ));

        let other = launched(
            "2b3c4d5e-6f7a-4b1c-8d2e-3f4a5b6c7d8e",
            "startup",
            launch("other-launch"),
        );
        assert_eq!(answered_start(&runtime, &other).await, HookDecision::Ack);
        assert!(session_of(&runtime, &other).is_ok());

        drop(deciding);
        assert_eq!(answered_start(&runtime, &wired).await, HookDecision::Ack);
        assert!(session_of(&runtime, &wired).is_ok());
    }

    /// A launched start takes its launch's guard before the alias lookup reads anything:
    /// while another start of the launch decides, it is refused as contended even where the
    /// lookup itself would refuse.
    #[tokio::test]
    async fn a_launched_start_is_contended_before_its_alias_lookup() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for (session, token) in [
            ("3c4d5e6f-7a8b-4c2d-9e3f-4a5b6c7d8e9f", "first-launch"),
            ("4d5e6f7a-8b9c-4d3e-8f4a-5b6c7d8e9fa0", "second-launch"),
        ] {
            let start = launched(session, "startup", launch(token));
            assert_eq!(answered_start(&runtime, &start).await, HookDecision::Ack);
        }
        let compacted = launched(
            "5e6f7a8b-9c0d-4e4f-9a5b-6c7d8e9fa0b1",
            "compact",
            launch("first-launch"),
        );
        assert_eq!(answered_start(&runtime, &compacted).await, HookDecision::Ack);
        let second = runtime
            .decide_launch(launch("second-launch"), &Default::default())
            .expect("the second launch decides");
        runtime
            .launched_start(
                hook_root(&compacted),
                Some(StartKind::Compact),
                &Default::default(),
                &second,
            )
            .expect("the compacted id aliases the second family too");
        drop(second);
        assert!(matches!(
            runtime.alias_of(hook_root(&compacted)),
            Err(EventError::EngineInvariant(_))
        ));

        let _deciding = runtime
            .decide_launch(launch("first-launch"), &Default::default())
            .expect("the first launch decides");
        assert!(contended(&answered_start(&runtime, &compacted).await));
    }

    fn spawn_call() -> crate::api::ProposedCall {
        crate::api::ProposedCall {
            tool: "host/claude-code/Task".to_string(),
            arguments: crate::api::raw(serde_json::json!({"prompt": "look it up"})),
            cwd: None,
        }
    }

    /// Declare the one return route the fixture offers — as spoken, floored at the
    /// parent's current label — for the offer the blocked spawn surfaced.
    async fn declare_return(runtime: &Runtime, root: &TrajectoryId, offers: &[appa_runtime_api::OfferedRemedy]) {
        let offer = offers
            .iter()
            .find(|offer| offer.returns == Some(appa_runtime_api::OfferedReturn::AsSpoken))
            .expect("the menu offers the bare floor");
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let arguments = crate::engine::RemedyArguments {
            label: Some(crate::engine::LabelSpelling::default()),
            return_schema: None,
        };
        let outcome = runtime
            .execute_remedy_with(&actor, OfferId(offer.id.clone()), arguments)
            .await;
        assert!(
            matches!(outcome, crate::api::RemedyOutcome::Authorized { .. }),
            "the declaration approves the spawn, got {outcome:?}"
        );
    }

    /// The marked spawn's round trip: blocked on the return menu, declared, released.
    async fn declared_spawn(runtime: &Runtime, root: &TrajectoryId) -> appa_runtime_api::SpawnBinding {
        let spawn = || HookEvent::ToolCall {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: spawn_call(),
            call_id: None,
            spawn: Some(appa_runtime_api::SpawnKind::Single),
            prompt: None,
            ruling: None,
            declares: None,
        };
        let HookDecision::DenyCall { offers, .. } = handle(runtime, spawn()).await else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(runtime, root, &offers).await;
        let released = handle(runtime, spawn()).await;
        let HookDecision::AllowCall { spawn: Some(binding) } = released else {
            panic!("the declared spawn must release with its binding, got {released:?}");
        };
        binding
    }

    const CONTROL_TOOL_FIXTURE_NAME: &str = "mcp__appa__execute_remedy_plan";

    fn fixtures() -> Vec<serde_json::Value> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hooks.jsonl");
        std::fs::read_to_string(path)
            .expect("the recorded hook fixtures are readable")
            .lines()
            .map(|line| serde_json::from_str(line).expect("each fixture line is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn the_recorded_session_replays_end_to_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for event in fixtures() {
            let name = event["hook_event_name"].as_str().expect("each fixture names its hook");
            let control = event["tool_name"] == CONTROL_TOOL_FIXTURE_NAME;
            let spawn = name == "PreToolUse" && event["tool_name"] == "Agent" && event.get("agent_id").is_none();

            let body = serde_json::to_vec(&event).expect("the fixture re-serializes");
            if spawn {
                // The parent's spawn blocks on the return menu; the model declares and
                // proposes the spawn again.
                let (status, answer) = hook(&runtime, &body).await;
                assert_eq!(status, 200, "hook {name} refused: {answer}");
                assert_eq!(
                    answer["hookSpecificOutput"]["permissionDecision"], "deny",
                    "a marked spawn blocks until its return is declared"
                );
                let root = TrajectoryId(format!("cc:{}", event["session_id"].as_str().expect("a session id")));
                let quoted = runtime
                    .minted_offers(&root, &root)
                    .into_iter()
                    .next()
                    .expect("the block surfaced the return declaration");
                let offers = vec![appa_runtime_api::OfferedRemedy {
                    id: quoted.0,
                    returns: Some(appa_runtime_api::OfferedReturn::AsSpoken),
                    input_sanitizer: None,
                }];
                declare_return(&runtime, &root, &offers).await;
            }
            let (status, answer) = hook(&runtime, &body).await;

            assert_eq!(status, 200, "hook {name} refused: {answer}");
            match name {
                "PreToolUse" => {
                    // The wire shape is what the harness reads; the reason is what it shows
                    // the model, in whatever words the runtime chose.
                    let slot = answer
                        .as_object()
                        .expect("a hook answer is an object")
                        .get("hookSpecificOutput")
                        .and_then(serde_json::Value::as_object)
                        .expect("a pre-use answer renders inside hookSpecificOutput");
                    assert_eq!(
                        answer.as_object().expect("an object").keys().collect::<Vec<_>>(),
                        ["hookSpecificOutput"],
                        "a pre-use answer carries nothing beside it: {answer}",
                    );
                    assert_eq!(
                        slot.keys().collect::<Vec<_>>(),
                        ["hookEventName", "permissionDecision", "permissionDecisionReason"],
                        "a pre-use answer carries exactly these three fields: {answer}",
                    );
                    assert_eq!(slot["hookEventName"], "PreToolUse", "{answer}");
                    assert_eq!(
                        slot["permissionDecision"],
                        if control { "deny" } else { "allow" },
                        "a stale control offer is denied and every recorded call is released: {answer}",
                    );
                    assert!(
                        !slot["permissionDecisionReason"]
                            .as_str()
                            .expect("the reason is a string")
                            .is_empty(),
                        "the answer says why: {answer}",
                    );
                }
                other => assert_eq!(
                    answer,
                    serde_json::json!({}),
                    "the {other} hook answers with no opinion",
                ),
            }
        }
    }

    /// On this hook every refusal family means "do not end the turn",
    /// so a turn end answers with no opinion whatever the close did —
    /// including for a session the runtime never opened.
    #[tokio::test]
    async fn a_turn_end_answers_with_no_opinion_even_over_an_unknown_session() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for name in ["Stop", "StopFailure", "SubagentStop"] {
            let mut body = serde_json::json!({
                "hook_event_name": name,
                "session_id": "never-opened",
                "last_assistant_message": "the summary",
            });
            if name == "SubagentStop" {
                body["agent_id"] = serde_json::Value::String("a1".to_string());
            }
            let (status, answer) = hook(&runtime, &json(&body)).await;
            assert_eq!(
                (status, answer),
                (200, serde_json::json!({})),
                "the {name} hook blocked"
            );
        }
    }

    /// The wedge this hook exists to clear: a released call the harness
    /// never ran refuses every later proposal until the turn ends.
    #[tokio::test]
    async fn a_turn_end_frees_a_trajectory_the_missing_outcome_wedged() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let propose = |command: &str| {
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_name": "Bash",
                "tool_input": {"command": command},
            })
        };
        let released = hook(&runtime, &json(&propose("ls"))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        // The harness refused it at its permission prompt: no outcome hook fires.
        let wedged = hook(&runtime, &json(&propose("echo hi"))).await;
        assert_eq!(wedged.1["hookSpecificOutput"]["permissionDecision"], "deny");

        let stop = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"});
        assert_eq!(hook(&runtime, &json(&stop)).await, (200, serde_json::json!({})),);

        let freed = hook(&runtime, &json(&propose("echo hi"))).await;
        assert_eq!(freed.1["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    fn bash_call(command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": command},
        })
    }

    /// The same proposal with the host's own id for the call, which is how Claude Code
    /// reports one: an identified call names its own dispatch, so another identified call
    /// being open does not refuse it.
    fn identified_bash_call(command: &str, call_id: &str) -> serde_json::Value {
        let mut call = bash_call(command);
        call["tool_use_id"] = serde_json::Value::String(call_id.to_string());
        call
    }

    fn bash_result(command: &str) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"stdout": "done"},
        })
    }

    fn prompt() -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "s1",
            "prompt": "never mind, list the files",
        })
    }

    fn closed_as_unknown(runtime: &Runtime) -> bool {
        runtime
            .audit(&appa_runtime_api::TrajectoryId("cc:s1".to_string()))
            .expect("the audit reads")
            .iter()
            .any(|entry| {
                matches!(
                    &entry.event,
                    crate::engine::AuditEvent::Closed {
                        outcome: crate::engine::DispatchOutcome::Unknown
                    }
                )
            })
    }

    /// The user pressed Esc while the command ran: Claude Code sends no
    /// outcome hook for the call and no `Stop` hook for the turn. The
    /// next prompt is the first hook to arrive; the new turn's first
    /// proposal closes the call as unreported and releases.
    #[tokio::test]
    async fn the_first_call_after_a_prompt_frees_a_call_an_interrupt_left_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &json(&bash_call("ping -c 30 127.0.0.1"))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        let actor = Actor {
            root: appa_runtime_api::TrajectoryId("cc:s1".to_string()),
            child: None,
        };
        let quoted = crate::api::PermitKey::offer(&OfferId("0ffe000000000001".to_string()));
        runtime.vouch(&quoted, &actor, None);

        // Interrupted: neither PostToolUse nor Stop arrives.
        assert_eq!(hook(&runtime, &json(&prompt())).await, (200, serde_json::json!({})));
        assert!(!closed_as_unknown(&runtime), "the prompt itself records nothing");

        let freed = hook(&runtime, &json(&bash_call("ls"))).await;
        assert_eq!(freed.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(closed_as_unknown(&runtime), "the interrupted call closed as unreported");
        assert_eq!(
            runtime.take_vouched(&quoted),
            Err(crate::api::Unvouched::Nobody),
            "the interrupted turn's vouch is released"
        );

        let late = hook(&runtime, &json(&bash_result("ping -c 30 127.0.0.1"))).await;
        assert_eq!(late.1["decision"], "block", "a result for the closed call is refused");
    }

    /// A proposal with an empty call id is refused before the settle of an interrupted turn
    /// writes: the call the interrupt left open stays open, and the prompt's mark stays set.
    /// The runtime's own control tool is refused alike.
    #[tokio::test]
    async fn an_empty_call_id_is_refused_before_it_settles_an_interrupted_turn() {
        let mut control = bash_call("ls");
        control["tool_name"] = serde_json::Value::String(CONTROL_TOOL_FIXTURE_NAME.to_string());
        control["tool_input"] = serde_json::json!({});
        control["tool_use_id"] = serde_json::Value::String(String::new());
        for proposal in [identified_bash_call("ls", ""), control] {
            let dir = tempfile::tempdir().expect("a temp dir is creatable");
            let runtime = open_runtime(&dir);
            let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
            let actor = Actor {
                root: root.clone(),
                child: None,
            };
            let released = hook(&runtime, &json(&bash_call("ping -c 30 127.0.0.1"))).await;
            assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
            assert_eq!(hook(&runtime, &json(&prompt())).await, (200, serde_json::json!({})));
            let before = runtime.audit(&root);
            let basis = runtime.log_basis(&root);

            let refused = hook(&runtime, &json(&proposal)).await;
            assert_eq!(
                refused.1["hookSpecificOutput"]["permissionDecision"], "deny",
                "{proposal}"
            );
            assert_eq!(runtime.audit(&root), before, "the refusal appended nothing: {proposal}");
            assert_eq!(runtime.log_basis(&root), basis, "the log took no append: {proposal}");
            assert!(
                !closed_as_unknown(&runtime),
                "the interrupted call stays open: {proposal}"
            );
            assert!(
                runtime.prompted(&actor),
                "the prompt's mark survives for the next proposal: {proposal}"
            );
        }
    }

    /// A served proposal with an empty call id is refused before the inventory it carries
    /// lands: an open trajectory's log takes no append, and a missing root is not created.
    #[tokio::test]
    async fn an_empty_call_id_is_refused_before_its_inventory_is_observed() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let opening = br#"{"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"open","inventory":{"tools":[{"name":"Bash","tool":"Bash"}],"sources":[{"server":"builtin","status":"complete","dynamic":false}]}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, opening).await;
        assert_eq!(status, 200, "the session opens: {reply}");
        let open = TrajectoryId("cc:open".to_string());
        let basis = runtime.log_basis(&open);

        for (root_id, root) in [
            ("open", open.clone()),
            ("absent", TrajectoryId("cc:absent".to_string())),
        ] {
            let proposal = format!(
                r#"{{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"{root_id}","call_id":"","tool":"Read","spawn":false,"arguments":{{"file_path":"a"}},"inventory":{{"tools":[{{"name":"Read","tool":"Read"}}],"sources":[{{"server":"builtin","status":"complete","dynamic":false}}]}}}}"#
            );
            let (_, reply) = answer(&runtime, &adapter, PrincipalSource::None, proposal.as_bytes()).await;
            assert_eq!(reply["decision"], "deny_call", "{root_id}: {reply}");
            assert_eq!(
                runtime.log_basis(&open),
                basis,
                "{root_id}: the refusal appended nothing"
            );
            assert_eq!(runtime.status(&root).is_some(), root == open, "{root_id}: {reply}");
        }
    }

    /// A malformed proposal is an integrity refusal even when it also names a child's
    /// transcript: the guard reads only a call the dispatcher would judge.
    #[tokio::test]
    async fn an_empty_call_id_outranks_a_named_transcript() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let root = TrajectoryId("cc:open".to_string());
        let spawn = || HookEvent::ToolCall {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: spawn_call(),
            call_id: Some("toolu-spawn".to_string()),
            spawn: Some(appa_runtime_api::SpawnKind::Single),
            prompt: None,
            ruling: None,
            declares: None,
        };
        let HookDecision::DenyCall { offers, .. } = handle(&runtime, spawn()).await else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(&runtime, &root, &offers).await;
        let HookDecision::AllowCall { spawn: Some(binding) } = handle(&runtime, spawn()).await else {
            panic!("the declared spawn releases with its binding");
        };
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: TrajectoryId("cc:open:a1".to_string()),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;
        let read = r#""tool":"Read","arguments":{"file_path":"/home/u/subagents/agent-a1.jsonl"}"#;
        let untargeted_send = r#""tool":"SendMessage","arguments":{"message":"/home/u/subagents/agent-a1.jsonl"}"#;
        for (call_id, call, deciding) in [
            ("toolu-1", read, "hard_block"),
            ("", read, "integrity"),
            ("toolu-2", untargeted_send, "integrity"),
        ] {
            let proposal = format!(
                r#"{{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"open","call_id":"{call_id}",{call},"spawn":false}}"#
            );
            let (_, reply) = answer(&runtime, &adapter, PrincipalSource::None, proposal.as_bytes()).await;
            assert_eq!(reply["decision"], "deny_call", "{call_id:?}: {reply}");
            assert_eq!(reply["deciding"], deciding, "{call_id:?}: {reply}");
        }
    }

    /// A background subagent runs past its parent's turn. Neither the turn end nor the
    /// settle at the next prompt's first call closes its identified spawn, so the return the
    /// subagent crosses later still reaches the parent through the spawn's result.
    #[tokio::test]
    async fn the_next_prompts_first_call_leaves_a_running_subagents_spawn_open() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        let child = TrajectoryId("cc:s1:a1".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let spawn = || HookEvent::ToolCall {
            actor: actor.clone(),
            call: spawn_call(),
            call_id: Some("toolu-spawn".to_string()),
            spawn: Some(appa_runtime_api::SpawnKind::Single),
            prompt: None,
            ruling: None,
            declares: None,
        };
        let HookDecision::DenyCall { offers, .. } = handle(&runtime, spawn()).await else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(&runtime, &root, &offers).await;
        let HookDecision::AllowCall { spawn: Some(binding) } = handle(&runtime, spawn()).await else {
            panic!("the declared spawn releases with its binding");
        };
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: child.clone(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;

        assert_eq!(
            handle(
                &runtime,
                HookEvent::TurnEnd {
                    actor: actor.clone(),
                    open_calls: appa_runtime_api::OpenCalls::Abandon
                }
            )
            .await,
            HookDecision::Ack
        );
        handle(
            &runtime,
            HookEvent::Prompt {
                actor: actor.clone(),
                text: "anything new?".into(),
                settles: None,
                peer: None,
                title: None,
            },
        )
        .await;
        let next = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: actor.clone(),
                call: crate::api::ProposedCall {
                    tool: "host/claude-code/Bash".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                    cwd: None,
                },
                call_id: Some("toolu-ls".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert_eq!(next, HookDecision::AllowCall { spawn: None });
        assert!(!runtime.prompted(&actor), "the first call settled the prompt");
        assert_eq!(
            runtime.open_dispatches(&root, &root).len(),
            2,
            "the settle left the running subagent's spawn open beside the new call"
        );

        handle(
            &runtime,
            HookEvent::ChildEnd {
                root: root.clone(),
                child: child.clone(),
                value: Some("found it".to_string()),
                open_calls: appa_runtime_api::OpenCalls::Abandon,
            },
        )
        .await;
        let returned = handle(
            &runtime,
            HookEvent::SpawnResult {
                actor: actor.clone(),
                call: spawn_call(),
                call_id: Some("toolu-spawn".to_string()),
                outcome: appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available(r#"{"agentId":"a1"}"#.to_string()),
                },
                child: Some(child),
                value: Some("found it".to_string()),
            },
        )
        .await;
        assert_eq!(
            returned,
            HookDecision::Ack,
            "the return replays through the spawn's result"
        );
        assert_eq!(runtime.open_dispatches(&root, &root).len(), 1, "the spawn closed");
    }

    fn bash(command: &str) -> crate::api::ProposedCall {
        crate::api::ProposedCall {
            tool: "host/claude-code/Bash".to_string(),
            arguments: crate::api::raw(serde_json::json!({ "command": command })),
            cwd: None,
        }
    }

    fn held_answering(ids: &[&str]) -> appa_runtime_api::OpenCalls {
        appa_runtime_api::OpenCalls::Hold {
            answered: ids.iter().map(|id| id.to_string()).collect(),
        }
    }

    /// An inference proxy sees a side request in a subagent's session (a progress label)
    /// while the subagent's command still runs, and reads its text-only answer as the
    /// subagent's stop. That step carries no result for the running command, so the stop is
    /// held: the command stays open, its real result is admitted when it arrives, and the
    /// subagent's next stop crosses as usual.
    #[tokio::test]
    async fn a_stop_that_leaves_a_call_unanswered_is_held_and_the_call_stays_reportable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        let child = TrajectoryId("cc:s1:a1".to_string());
        let child_actor = Actor {
            root: root.clone(),
            child: Some(child.clone()),
        };
        let binding = declared_spawn(&runtime, &root).await;
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: child.clone(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;
        let released = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: child_actor.clone(),
                call: bash("pytest"),
                call_id: Some("toolu-pytest".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert_eq!(released, HookDecision::AllowCall { spawn: None });

        let label = handle(
            &runtime,
            HookEvent::ChildEnd {
                root: root.clone(),
                child: child.clone(),
                value: Some("Running auth module tests".to_string()),
                open_calls: held_answering(&[]),
            },
        )
        .await;
        assert_eq!(
            label,
            HookDecision::Held {
                running: vec!["toolu-pytest".to_string()]
            }
        );
        assert_eq!(
            runtime.open_dispatches(&root, &child).len(),
            1,
            "the command stays open"
        );

        let result = handle(
            &runtime,
            HookEvent::ToolResult {
                actor: child_actor.clone(),
                call: bash("pytest"),
                call_id: Some("toolu-pytest".to_string()),
                outcome: appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available("3 passed".to_string()),
                },
            },
        )
        .await;
        assert_eq!(result, HookDecision::Ack, "the real result is admitted");

        let end = handle(
            &runtime,
            HookEvent::ChildEnd {
                root,
                child,
                value: Some("all tests pass".to_string()),
                open_calls: held_answering(&["toolu-pytest"]),
            },
        )
        .await;
        assert_eq!(end, HookDecision::Ack, "the subagent's real stop crosses");
    }

    /// A turn end never closes a spawn whose subagent runs on past it, so that spawn never
    /// holds the turn end either, answered or not.
    #[tokio::test]
    async fn a_running_subagents_spawn_does_not_hold_its_parents_turn_end() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let spawn = || HookEvent::ToolCall {
            actor: actor.clone(),
            call: spawn_call(),
            call_id: Some("toolu-spawn".to_string()),
            spawn: Some(appa_runtime_api::SpawnKind::Single),
            prompt: None,
            ruling: None,
            declares: None,
        };
        let HookDecision::DenyCall { offers, .. } = handle(&runtime, spawn()).await else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(&runtime, &root, &offers).await;
        let HookDecision::AllowCall { spawn: Some(binding) } = handle(&runtime, spawn()).await else {
            panic!("the declared spawn releases with its binding");
        };
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: TrajectoryId("cc:s1:a1".to_string()),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;

        let end = handle(
            &runtime,
            HookEvent::TurnEnd {
                actor,
                open_calls: held_answering(&[]),
            },
        )
        .await;
        assert_eq!(end, HookDecision::Ack);
        assert_eq!(
            runtime.open_dispatches(&root, &root).len(),
            1,
            "the running subagent's spawn stays open"
        );
    }

    /// Under a hold, a turn end waits for a call the stopping step did not answer, and
    /// closes as unreported a call it answered that no outcome reported, exactly as an
    /// abandoning turn end does.
    #[tokio::test]
    async fn a_held_turn_end_waits_only_for_unanswered_calls() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let released = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: actor.clone(),
                call: bash("sleep 30"),
                call_id: Some("toolu-sleep".to_string()),
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert_eq!(released, HookDecision::AllowCall { spawn: None });

        let side = handle(
            &runtime,
            HookEvent::TurnEnd {
                actor: actor.clone(),
                open_calls: held_answering(&["toolu-earlier"]),
            },
        )
        .await;
        assert_eq!(
            side,
            HookDecision::Held {
                running: vec!["toolu-sleep".to_string()]
            }
        );
        assert_eq!(
            runtime.open_dispatches(&root, &root).len(),
            1,
            "the held stop closed nothing"
        );

        let end = handle(
            &runtime,
            HookEvent::TurnEnd {
                actor,
                open_calls: held_answering(&["toolu-sleep"]),
            },
        )
        .await;
        assert_eq!(end, HookDecision::Ack);
        assert!(
            runtime.open_dispatches(&root, &root).is_empty(),
            "an answered call no outcome reported closes as unreported"
        );
    }

    /// The user typed while the command ran: Claude Code queues the message
    /// and fires the prompt hook at once, then reports the command's
    /// outcome when it finishes. The result must land on the still-open
    /// call, and the next turn's first proposal then releases freely.
    #[tokio::test]
    async fn a_prompt_queued_behind_a_running_call_keeps_its_result() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &json(&bash_call("sleep 30"))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        assert_eq!(hook(&runtime, &json(&prompt())).await, (200, serde_json::json!({})));
        assert_eq!(
            hook(&runtime, &json(&bash_result("sleep 30"))).await,
            (200, serde_json::json!({})),
            "the running call's result lands on its open dispatch",
        );
        assert!(!closed_as_unknown(&runtime), "nothing was closed as unreported");

        let next = hook(&runtime, &json(&bash_call("ls"))).await;
        assert_eq!(next.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert!(!closed_as_unknown(&runtime));
        let repeated = hook(&runtime, &json(&bash_result("sleep 30"))).await;
        assert_eq!(
            repeated.1["decision"], "block",
            "a second report of the same result is refused"
        );
    }

    /// A prompt gates nothing, so a store that refuses every append still acknowledges it.
    /// The mark is what does not land, and losing it costs only the early settle: the next
    /// proposal is decided as any proposal is, and the interrupted call is closed at the
    /// turn's end instead.
    #[tokio::test]
    async fn a_prompt_acknowledges_over_a_store_that_cannot_append() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(
            &runtime,
            &json(&identified_bash_call("ping -c 30 127.0.0.1", "toolu_1")),
        )
        .await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        runtime.store().fail_commit_after(0);

        assert_eq!(hook(&runtime, &json(&prompt())).await, (200, serde_json::json!({})));
        runtime.store().fail_commit_after(u64::MAX - 1);

        let next = hook(&runtime, &json(&identified_bash_call("ls", "toolu_2"))).await;
        assert_eq!(
            next.1["hookSpecificOutput"]["permissionDecision"], "allow",
            "the prompt left no mark, so the proposal is decided as any proposal is"
        );
        assert!(!closed_as_unknown(&runtime), "nothing settled the interrupted call yet");

        let stop = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"});
        assert_eq!(hook(&runtime, &json(&stop)).await, (200, serde_json::json!({})));
        assert!(
            closed_as_unknown(&runtime),
            "the turn's end closed what the prompt left open"
        );
    }

    /// A turn end settles the prompt's mark too: a proposal in the turn
    /// after a normally ended one closes nothing.
    #[tokio::test]
    async fn a_turn_end_settles_the_prompt_mark() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        assert_eq!(hook(&runtime, &json(&prompt())).await, (200, serde_json::json!({})));
        let stop = serde_json::json!({"hook_event_name": "Stop", "session_id": "s1"});
        assert_eq!(hook(&runtime, &json(&stop)).await, (200, serde_json::json!({})));

        let released = hook(&runtime, &json(&bash_call("sleep 30"))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            hook(&runtime, &json(&bash_result("sleep 30"))).await,
            (200, serde_json::json!({}))
        );
        assert!(!closed_as_unknown(&runtime));
    }

    /// Every refusal of the dispatcher's own that reaches the model on the
    /// deny wire names APPA, whichever event error produced it.
    #[tokio::test]
    async fn a_deny_of_the_dispatchers_own_names_appa() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let released = hook(&runtime, &json(&bash_call("sleep 30"))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        let denied = hook(&runtime, &json(&bash_call("ls"))).await;
        assert_eq!(denied.1["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = denied.1["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("a deny carries its reason");
        assert!(reason.starts_with("[appa] "), "the deny reads: {reason}");
    }

    #[tokio::test]
    async fn a_deny_renders_in_the_pre_tool_use_wire() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        // A declared tool with non-object arguments is a malformed call: the engine
        // answers it as model feedback, and the feedback rides the deny wire.
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": ["not", "an", "object"],
            "tool_use_id": "toolu_1",
        });
        let (status, answer) = hook(&runtime, &json(&event)).await;
        assert_eq!(status, 200);
        let rendered = answer["hookSpecificOutput"]
            .as_object()
            .expect("a deny renders inside hookSpecificOutput");
        assert_eq!(
            rendered.keys().collect::<Vec<_>>(),
            ["hookEventName", "permissionDecision", "permissionDecisionReason"],
            "the deny wire carries exactly these three fields: {answer}",
        );
        assert_eq!(rendered["hookEventName"], "PreToolUse");
        assert_eq!(rendered["permissionDecision"], "deny");
        assert!(
            !rendered["permissionDecisionReason"]
                .as_str()
                .expect("the reason is a string")
                .is_empty(),
            "the deny carries the engine's feedback to the model",
        );
    }

    #[tokio::test]
    async fn an_event_error_renders_as_a_deny() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let body = serde_json::to_vec(&event).expect("serializes");
        assert_eq!(hook(&runtime, &body).await.0, 200);

        let second = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Write",
            "tool_input": {"file_path": "/tmp/x", "content": "y"},
        });
        let (status, answer) = hook(&runtime, &json(&second)).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer["hookSpecificOutput"]["permissionDecision"], "deny",
            "lifecycle misuse renders as a deny",
        );
    }

    #[tokio::test]
    async fn a_plain_success_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let pre = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "cat secret.txt"},
        });
        hook(&runtime, &json(&pre)).await;

        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "cat secret.txt"},
            "tool_response": {"content": "the secret"},
        });
        let (status, answer) = hook(&runtime, &json(&post)).await;
        assert_eq!(status, 200, "the outcome refused: {answer}");
        assert_eq!(answer, serde_json::json!({}), "a plain success answers with no opinion");
    }

    #[tokio::test]
    async fn a_mid_run_input_rewrite_still_matches_its_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let questions = serde_json::json!({"questions": [{"question": "Declare the tool?"}]});
        let pre = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "AskUserQuestion",
            "tool_input": questions,
        });
        let (status, answer) = hook(&runtime, &json(&pre)).await;
        assert_eq!(status, 200, "the release refused: {answer}");

        let post = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "AskUserQuestion",
            "tool_input": {
                "questions": [{"question": "Declare the tool?"}],
                "answers": {"Declare the tool?": "No, skip it"},
            },
            "tool_response": {"answers": {"Declare the tool?": "No, skip it"}},
        });
        let (status, answer) = hook(&runtime, &json(&post)).await;
        assert_eq!(status, 200, "the rewritten report refused: {answer}");
        assert_eq!(answer, serde_json::json!({}), "the rewritten report lands");

        let next = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let (_, answer) = hook(&runtime, &json(&next)).await;
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    /// A declaration's vouch that does not land refuses its control call, so the execution
    /// after it never answers as one no hook vouched for.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_declaration_vouch_that_does_not_land_refuses_the_control_call() {
        use crate::api::{ExecuteRemedyPlanArgs, Holder, LabelArgs, declaration_key};
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let control = async |label: serde_json::Value| {
            let event = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_name": CONTROL_TOOL_FIXTURE_NAME,
                "tool_input": { "label": label },
            });
            let parsed = through_the_codec(&event).expect("the control call parses");
            handle_internal(
                &runtime,
                parsed,
                EmbeddedPresentationOptions::default(),
                ToolCallIds::Unnamed,
            )
            .await
            .decision
        };
        // Another declaration's control call opens the root first.
        assert!(matches!(
            control(serde_json::json!({ "trust": "trusted" })).await,
            HookDecision::PassControl
        ));
        let args = || ExecuteRemedyPlanArgs {
            offer_id: None,
            label: Some(LabelArgs {
                trust: None,
                audience: None,
            }),
            return_schema: None,
        };
        runtime.store().fail_commit_after(0);
        let refused = control(serde_json::json!({})).await;
        assert!(!matches!(refused, HookDecision::PassControl), "{refused:?}");
        assert_eq!(
            runtime
                .discover_holder(&declaration_key(&args(), None))
                .expect("the discovery reads"),
            Holder::Nobody,
            "nothing was vouched"
        );
        assert!(matches!(
            control(serde_json::json!({})).await,
            HookDecision::PassControl
        ));
        let declared = runtime.execute_remedy_plan(args(), None, None).await;
        assert!(!declared.is_error, "{}", declared.text);
        assert_eq!(
            runtime
                .discover_holder(&declaration_key(&args(), None))
                .expect("the discovery reads"),
            Holder::Nobody,
            "the declaration spent the vouch"
        );
    }

    #[tokio::test]
    async fn the_control_tool_opens_no_dispatch() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": CONTROL_TOOL_FIXTURE_NAME,
            "tool_input": {},
        });
        let (status, answer) = hook(&runtime, &json(&event)).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "allow");

        // The control call is passed through rather than released: the engine never sees it,
        // so its entry names no dispatch, where an ordinary call's names the one it opened.
        let parsed = through_the_codec(&event).expect("the control call parses");
        let control = handle_internal(
            &runtime,
            parsed,
            EmbeddedPresentationOptions::default(),
            ToolCallIds::Unnamed,
        )
        .await;
        assert!(
            matches!(control.decision, HookDecision::PassControl),
            "the control tool is passed through, not checked: {:?}",
            control.decision
        );
        assert!(
            matches!(control.event, crate::events::RuntimeEvent::Hook { dispatch: None, .. }),
            "a passed-through call opens no dispatch: {:?}",
            control.event
        );

        let call = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": {"command": "ls"},
        });
        let ordinary = handle_internal(
            &runtime,
            through_the_codec(&call).expect("the call parses"),
            EmbeddedPresentationOptions::default(),
            ToolCallIds::Unnamed,
        )
        .await;
        assert!(
            matches!(ordinary.decision, HookDecision::AllowCall { .. }),
            "an ordinary call in the same session is released: {:?}",
            ordinary.decision
        );
        assert!(
            matches!(
                ordinary.event,
                crate::events::RuntimeEvent::Hook { dispatch: Some(_), .. }
            ),
            "a released call opens one: {:?}",
            ordinary.event
        );
    }

    /// The wire carries the host's raw spelling; the served adapter identifies which
    /// call is the control tool. A lookalike on another server, and the bare name
    /// a host tool could take, both identify an ordinary tool nothing covers.
    #[tokio::test]
    async fn a_lookalike_control_tool_is_checked() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        for (raw, canonical) in [
            ("mcp__evil__execute_remedy_plan", "mcp/evil/execute_remedy_plan"),
            (
                "mcp__appa-guide__execute_remedy_plan",
                "mcp/appa-guide/execute_remedy_plan",
            ),
            ("execute_remedy_plan", "host/claude-code/execute_remedy_plan"),
        ] {
            let event = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "s1",
                "tool_name": raw,
                "tool_input": {"offer_id": "whatever"},
            });
            let (status, answer) = hook(&runtime, &json(&event)).await;
            // Not the exemption's allow: the lookalike reaches the engine, and nothing
            // covers the name, so the refusal is typed and rides the error wire.
            assert_eq!(status, 409, "{raw} must reach the engine, not the exemption: {answer}");
            assert!(
                answer["error"]
                    .as_str()
                    .is_some_and(|detail| detail.contains(canonical)),
                "the refusal names the identified tool: {answer}"
            );
        }
    }

    /// The wire is one protocol and one adapter: an event under another protocol, or
    /// for another host, is refused before any session is touched.
    #[tokio::test]
    async fn another_protocol_or_another_adapters_event_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let foreign_protocol = br#"{"protocol":2,"adapter":"claude-code","event":"session_start","root_id":"s1"}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, foreign_protocol).await;
        assert_eq!(status, 409, "{reply}");
        assert!(reply["error"].is_string(), "{reply}");

        let foreign_adapter = br#"{"protocol":1,"adapter":"kagent","event":"session_start","root_id":"s1"}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, foreign_adapter).await;
        assert_eq!(status, 409, "{reply}");
        assert!(reply["error"].is_string(), "{reply}");
        assert!(
            runtime.status(&TrajectoryId("cc:s1".to_string())).is_none()
                && runtime.status(&TrajectoryId("kagent:s1".to_string())).is_none(),
            "a refused envelope opens nothing"
        );

        let served = br#"{"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"s1"}"#;
        assert_eq!(answer(&runtime, &adapter, PrincipalSource::None, served).await.0, 200);
        assert!(runtime.status(&TrajectoryId("cc:s1".to_string())).is_some());
    }

    /// A hook refused while its host id is resolved still leaves its one entry, filed
    /// deployment-wide: no family is known yet to file it under.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_hook_refused_while_resolving_its_host_id_leaves_a_deployment_entry() {
        use crate::events::{HookOutcome, RecordedEvent, RuntimeEvent};
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        // An earlier family, whose account shows the deployment-wide entries after its start.
        let earlier = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s0","tool":"Bash","spawn":false,"arguments":{"command":"ls"}}"#;
        assert_eq!(answer(&runtime, &adapter, PrincipalSource::None, earlier).await.0, 200);
        let call = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s1","tool":"Bash","spawn":false,"arguments":{"command":"ls"}}"#;
        runtime.store().fail_next_reads(1);
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, call).await;
        assert_eq!(status, 409, "{reply}");
        let unresolved = runtime.recorded_events(&TrajectoryId("cc:s1".to_string()));
        assert!(unresolved.entries.is_empty(), "no family is invented: {unresolved:?}");
        let recorded = runtime.recorded_events(&TrajectoryId("cc:s0".to_string()));
        let refused = |entry: &RecordedEvent| {
            matches!(
                entry.event,
                RuntimeEvent::Hook {
                    outcome: HookOutcome::Refused,
                    ..
                }
            )
        };
        assert!(!recorded.entries.iter().any(refused), "{recorded:?}");
        assert_eq!(
            recorded.deployment.iter().filter(|entry| refused(entry)).count(),
            1,
            "{recorded:?}"
        );
    }

    /// Whether a call is a spawn comes from tool identification, never from the
    /// wire: a `spawn` claim on an ordinary tool releases it as an ordinary call, and
    /// the spawn tool is held on the return menu with no claim at all.
    #[tokio::test]
    async fn a_wire_spawn_claim_is_ignored_and_the_spawn_is_identified() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let claimed = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s1","tool":"Bash","spawn":true,"arguments":{"command":"ls"}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, claimed).await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(reply["decision"], "allow_call", "{reply}");
        assert!(reply.get("spawn_binding").is_none(), "no fork was prepared: {reply}");

        let identified = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s2","tool":"Agent","spawn":false,"arguments":{"prompt":"go"}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, identified).await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(
            reply["decision"], "deny_call",
            "the spawn is held on the return menu: {reply}"
        );
        assert!(
            reply["offers"].as_array().is_some_and(|offers| !offers.is_empty()),
            "{reply}"
        );
    }

    /// A ruling is a person's answer the harness obtained through its own
    /// review channel, and the runtime spends it as the human authority's.
    /// Claude Code reviews through no channel of its own, so a control call
    /// posted under it carrying a ruling — which any local process that
    /// reaches the loopback endpoint could spell — is refused at the
    /// envelope and vouches nothing. kagent, which does review, is admitted.
    #[tokio::test]
    async fn a_ruling_under_a_host_that_reviews_through_no_channel_of_its_own_is_refused() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let quoted = OfferId("0ffe000000000001".to_string());

        let forged = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s1","tool":"mcp__appa__execute_remedy_plan","arguments":{"offer_id":"0ffe000000000001"},"ruling":"approve"}"#;
        let (status, reply) = answer(
            &runtime,
            &appa_adapter_claude_code::adapter(),
            PrincipalSource::None,
            forged,
        )
        .await;
        assert_eq!(status, 409, "a forged ruling must refuse: {reply}");
        assert!(reply["error"].is_string(), "{reply}");
        assert_eq!(
            runtime.take_vouched(&crate::api::PermitKey::offer(&quoted)),
            Err(crate::api::Unvouched::Nobody),
            "the refused envelope recorded no reviewer's answer"
        );

        let ruled = br#"{"protocol":1,"adapter":"kagent","event":"tool_call","root_id":"s1","tool":"appa:execute_remedy_plan","arguments":{"offer_id":"0ffe000000000001"},"ruling":"approve"}"#;
        let (status, reply) = answer(&runtime, &appa_adapter_kagent::adapter(), PrincipalSource::None, ruled).await;
        assert_eq!(status, 200, "kagent's own review channel carries a ruling: {reply}");

        let control = br#"{"protocol":1,"adapter":"claude-code","event":"tool_call","root_id":"s1","tool":"mcp__appa__execute_remedy_plan","arguments":{}}"#;
        let (status, reply) = answer(
            &runtime,
            &appa_adapter_claude_code::adapter(),
            PrincipalSource::None,
            control,
        )
        .await;
        assert_eq!(status, 200, "{reply}");
        assert_eq!(
            reply["decision"], "pass_control",
            "an ordinary control call still passes control: {reply}"
        );
    }

    #[tokio::test]
    async fn a_control_tools_post_hook_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "tool_name": "mcp__appa__execute_remedy_plan",
            "tool_input": {"offer_id": "0ffe000000000001"},
            "tool_response": {"content": "Authorized."},
        });
        let (status, answer) = hook(&runtime, &json(&event)).await;
        assert_eq!(status, 200);
        assert_eq!(answer, serde_json::json!({}));
    }

    #[tokio::test]
    async fn a_control_call_answers_without_a_session_lookup() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);

        let start = serde_json::json!({
            "hook_event_name": "SubagentStart",
            "session_id": "s1",
            "agent_id": "a1",
        });
        assert_eq!(
            hook(&runtime, &json(&start)).await.0,
            409,
            "an uncorrelated subagent start refuses",
        );

        let control = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": CONTROL_TOOL_FIXTURE_NAME,
            "tool_input": {"offer_id": "0ffe000000000001"},
        });
        let (status, answer) = hook(&runtime, &json(&control)).await;
        assert_eq!(status, 200);
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny");

        let outcome = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "session_id": "s1",
            "agent_id": "a1",
            "tool_name": CONTROL_TOOL_FIXTURE_NAME,
            "tool_input": {"offer_id": "0ffe000000000001"},
            "tool_response": {"ok": true},
        });
        let (status, answer) = hook(&runtime, &json(&outcome)).await;
        assert_eq!(status, 200);
        assert_eq!(
            answer,
            serde_json::json!({}),
            "the control outcome is absorbed without a session lookup"
        );
    }

    #[tokio::test]
    async fn a_child_return_that_crosses_unchanged_answers_with_no_opinion() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());

        let binding = declared_spawn(&runtime, &root).await;
        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildStart {
                    root: root.clone(),
                    child: child.clone(),
                    spawn: SpawnRef::Binding(binding),
                },
            )
            .await,
            HookDecision::Ack,
        );

        // A subagent's standing must not outlive its return: its parent's turn end is not
        // its own, so nothing else would ever end it.
        let quoted = crate::api::PermitKey::offer(&OfferId("0ffe000000000001".to_string()));
        runtime.vouch(
            &quoted,
            &Actor {
                root: root.clone(),
                child: Some(child.clone()),
            },
            None,
        );

        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildEnd {
                    root,
                    child,
                    value: Some("the report, nothing sensitive".to_string()),
                    open_calls: appa_runtime_api::OpenCalls::Abandon,
                },
            )
            .await,
            HookDecision::Ack,
            "an unchanged crossing needs no answer",
        );
        assert_eq!(
            runtime.take_vouched(&quoted),
            Err(crate::api::Unvouched::Nobody),
            "the child's return ended what it still stood behind"
        );
    }

    /// A start that carries the host's tool inventory opens its trajectory with that
    /// inventory, and a later one extends it. The two paths differ in more than their
    /// spelling: an unknown root is created together with what it observed, while a known
    /// one only observes, and only a root may do either — a child's start has no inventory
    /// of its own to open a session with. Nothing else in this suite reaches this path, so
    /// a session created without its inventory would otherwise be found in production.
    #[tokio::test]
    async fn a_start_carrying_an_inventory_opens_its_trajectory_with_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let adapter = appa_adapter_claude_code::adapter();
        let root = TrajectoryId("cc:inv".to_string());

        let opening = br#"{"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"inv","inventory":{"tools":[{"name":"Bash","tool":"Bash"}],"sources":[{"server":"builtin","status":"complete","dynamic":false}]}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, opening).await;
        assert_eq!(status, 200, "an opening inventory is admitted: {reply}");
        assert!(
            runtime.status(&root).is_some(),
            "the trajectory is created together with what it observed",
        );

        let later = br#"{"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"inv","inventory":{"tools":[{"name":"Read","tool":"Read"}],"sources":[{"server":"builtin","status":"complete","dynamic":false}]}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, later).await;
        assert_eq!(status, 200, "a later observation extends the open one: {reply}");

        // What proves the opening inventory was kept rather than dropped on the floor: a
        // name cannot be rebound to another tool, which is only decidable against what the
        // trajectory already observed.
        let rebinding = br#"{"protocol":1,"adapter":"claude-code","event":"session_start","root_id":"inv","inventory":{"tools":[{"name":"Bash","tool":"Read"}],"sources":[{"server":"builtin","status":"complete","dynamic":false}]}}"#;
        let (status, reply) = answer(&runtime, &adapter, PrincipalSource::None, rebinding).await;
        assert_eq!(
            status, 409,
            "rebinding a name the opening inventory bound is refused: {reply}",
        );

        // The trajectory is live either way: the inventory is observed beside the session,
        // never instead of opening it.
        let call = hook(
            &runtime,
            br#"{"hook_event_name":"PreToolUse","session_id":"inv","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        )
        .await;
        assert_eq!(call.0, 200, "the opened trajectory decides calls: {:?}", call.1);
    }

    /// What a child's stop can be answered with, and what it can never be answered with. A
    /// stop is where a return crosses, and no hook rewrites what a subagent delivers: the
    /// runtime can acknowledge it, substitute the bytes the child must echo, hold it, or
    /// refuse operationally — but it never replaces or delivers an output there, because
    /// that is the parent's result channel, not the child's. The adapter relies on this: a
    /// replacement rendered on a stop would be dropped by the harness with the return still
    /// in front of the parent.
    #[tokio::test]
    async fn a_child_end_is_answered_only_as_a_return_a_hold_or_a_refusal() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());

        let binding = declared_spawn(&runtime, &root).await;
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: child.clone(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;

        let ends = [
            ("a crossing return", Some("the report".to_string())),
            ("a return of nothing", None),
        ];
        for (name, value) in ends {
            let decision = handle(
                &runtime,
                HookEvent::ChildEnd {
                    root: root.clone(),
                    child: child.clone(),
                    value,
                    open_calls: appa_runtime_api::OpenCalls::Abandon,
                },
            )
            .await;
            assert!(
                matches!(
                    decision,
                    HookDecision::Ack
                        | HookDecision::ChildReturn { .. }
                        | HookDecision::Block { .. }
                        | HookDecision::Refuse { .. }
                ),
                "{name} answered with {decision:?}, which a stop has no channel for",
            );
        }

        // An unopened child's stop claims a return no start could supply.
        let stranger = handle(
            &runtime,
            HookEvent::ChildEnd {
                root: root.clone(),
                child: appa_runtime_api::TrajectoryId("cc:s1:nobody".to_string()),
                value: Some("a return from nowhere".to_string()),
                open_calls: appa_runtime_api::OpenCalls::Abandon,
            },
        )
        .await;
        assert!(
            matches!(
                stranger,
                HookDecision::Ack
                    | HookDecision::ChildReturn { .. }
                    | HookDecision::Block { .. }
                    | HookDecision::Refuse { .. }
            ),
            "an unopened child's stop answered with {stranger:?}",
        );
    }

    /// The operational path out of a child's stop. A store that cannot append decides
    /// nothing about the flow, so the answer is a refusal the harness reports rather than
    /// feedback the model reads as a judgement about its return.
    #[tokio::test]
    async fn a_child_end_over_a_store_that_cannot_append_refuses() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());

        let binding = declared_spawn(&runtime, &root).await;
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: child.clone(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;

        runtime.store().fail_commit_after(0);
        let decision = handle(
            &runtime,
            HookEvent::ChildEnd {
                root,
                child,
                value: Some("the report".to_string()),
                open_calls: appa_runtime_api::OpenCalls::Abandon,
            },
        )
        .await;
        runtime.store().fail_commit_after(u64::MAX - 1);
        assert!(
            matches!(decision, HookDecision::Refuse { .. }),
            "an operational failure refuses rather than answering the model: {decision:?}",
        );
    }

    #[tokio::test]
    async fn a_native_spawn_resume_keeps_the_dispatch_across_an_approval_prompt() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        handle(
            &runtime,
            HookEvent::SessionStart {
                root: root.clone(),
                principal: None,
                address: None,
                title: None,
                launch: None,
                start: None,
                settings: Default::default(),
            },
        )
        .await;
        let binding = declared_spawn(&runtime, &root).await;
        let child = TrajectoryId("cc:s1:c1".to_string());
        handle(
            &runtime,
            HookEvent::ChildStart {
                root: root.clone(),
                child: child.clone(),
                spawn: SpawnRef::Binding(binding),
            },
        )
        .await;
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let original = runtime.open_dispatches(&root, &root)[0].id.clone();
        let quoted = crate::api::PermitKey::offer(&OfferId("0ffe000000000001".to_string()));
        runtime.vouch(&quoted, &actor, None);
        handle(
            &runtime,
            HookEvent::Prompt {
                actor: actor.clone(),
                text: "approve".into(),
                settles: None,
                peer: None,
                title: None,
            },
        )
        .await;
        let wrong = handle(
            &runtime,
            HookEvent::SpawnResume {
                actor: actor.clone(),
                call: spawn_call(),
                child: TrajectoryId("cc:s1:other".into()),
            },
        )
        .await;
        assert!(matches!(wrong, HookDecision::Block { .. }));
        let resumed = handle(
            &runtime,
            HookEvent::SpawnResume {
                actor: actor.clone(),
                call: spawn_call(),
                child,
            },
        )
        .await;
        assert_eq!(resumed, HookDecision::Ack);
        assert_eq!(runtime.open_dispatches(&root, &root)[0].id, original);
        assert!(!runtime.prompted(&actor), "successful resume settled the prompt marker");
        assert_eq!(
            runtime.take_vouched(&quoted),
            Ok((actor, None)),
            "a resume settles the prompt and nothing else: the turn's standing survives it"
        );
    }

    #[tokio::test]
    async fn an_uncorrelated_child_return_blocks_rather_than_refusing() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());

        let decision = handle(
            &runtime,
            HookEvent::ChildEnd {
                root: root.clone(),
                child: appa_runtime_api::TrajectoryId("cc:s1:a1".to_string()),
                value: Some("late".to_string()),
                open_calls: appa_runtime_api::OpenCalls::Abandon,
            },
        )
        .await;
        assert!(
            matches!(decision, HookDecision::Block { .. }),
            "an uncorrelated child blocks; only an operational failure refuses. Got {decision:?}",
        );
    }

    #[tokio::test]
    async fn a_spawn_result_on_an_unforked_call_answers_as_an_ordinary_outcome() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let call = || crate::api::ProposedCall {
            tool: "host/claude-code/Agent".to_string(),
            arguments: crate::api::raw(serde_json::json!({"prompt": "list files"})),
            cwd: None,
        };
        let result = || HookEvent::SpawnResult {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: call(),
            call_id: None,
            outcome: appa_runtime_api::ToolOutcome::Success {
                body: appa_runtime_api::OutcomeBody::Available(r#"{"agentId":"a1"}"#.to_string()),
            },
            child: Some(appa_runtime_api::TrajectoryId("cc:s1:a1".to_string())),
            value: Some("one file".to_string()),
        };

        let decision = handle(&runtime, result()).await;
        assert!(matches!(decision, HookDecision::Block { .. }), "got {decision:?}");

        let released = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: None,
                },
                call: call(),
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert_eq!(released, HookDecision::AllowCall { spawn: None });
        assert_eq!(handle(&runtime, result()).await, HookDecision::Ack);
    }

    #[tokio::test]
    async fn a_child_event_before_its_start_opens_the_child_to_the_one_spawn_in_flight() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = appa_runtime_api::TrajectoryId("cc:s1".to_string());
        let child = appa_runtime_api::TrajectoryId("cc:s1:a1".to_string());
        declared_spawn(&runtime, &root).await;

        let decision = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: Some(child.clone()),
                },
                call: crate::api::ProposedCall {
                    tool: "host/claude-code/Bash".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                    cwd: None,
                },
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert_eq!(decision, HookDecision::AllowCall { spawn: None });
        let forked = runtime
            .audit(&root)
            .expect("the audit reads")
            .into_iter()
            .filter(|entry| matches!(entry.event, crate::api::AuditEvent::Forked { .. }))
            .count();
        assert_eq!(forked, 1, "the late open bound the one spawn in flight");

        assert_eq!(
            handle(
                &runtime,
                HookEvent::ChildStart {
                    root: root.clone(),
                    child: child.clone(),
                    spawn: SpawnRef::InFlight,
                },
            )
            .await,
            HookDecision::Ack,
        );

        let denied = handle(
            &runtime,
            HookEvent::ToolCall {
                actor: Actor {
                    root: root.clone(),
                    child: Some(appa_runtime_api::TrajectoryId("cc:s1:a2".to_string())),
                },
                call: crate::api::ProposedCall {
                    tool: "host/claude-code/Bash".to_string(),
                    arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                    cwd: None,
                },
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
                declares: None,
            },
        )
        .await;
        assert!(matches!(denied, HookDecision::DenyCall { .. }), "got {denied:?}");
    }

    #[tokio::test]
    async fn an_operational_failure_refuses_instead_of_answering_the_model() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        runtime.store().fail_commit_after(0);
        let (status, answer) = hook(&runtime, &json(&bash_call("ls"))).await;
        assert_eq!(status, 409, "the harness must fail closed on a storage failure");
        assert!(
            answer.get("error").is_some(),
            "an operational failure renders as a refusal, never as model-facing feedback: {answer}",
        );
    }

    /// A refusal carries the class of the fault that caused it, so a host routes on the
    /// class and never on the detail's wording.
    #[tokio::test]
    async fn a_refusal_names_the_class_of_its_fault() {
        use appa_runtime_api::RefusalClass;

        let root = TrajectoryId("cc:s1".to_string());
        let actor = Actor {
            root: root.clone(),
            child: None,
        };
        let ls = || HookEvent::ToolCall {
            actor: actor.clone(),
            call: crate::api::ProposedCall {
                tool: "host/claude-code/Bash".to_string(),
                arguments: crate::api::raw(serde_json::json!({"command": "ls"})),
                cwd: None,
            },
            call_id: None,
            spawn: None,
            prompt: None,
            ruling: None,
            declares: None,
        };
        let started = |runtime: &Runtime| {
            runtime.create_session(root.clone(), None).expect("a fresh id opens");
        };
        let class = |decision: HookDecision| match decision {
            HookDecision::Refuse { class, .. } => class,
            other => panic!("expected a refusal, got {other:?}"),
        };

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        started(&runtime);
        runtime.store().corrupt_batch(&root, 0, b"not json at all");
        assert_eq!(class(handle(&runtime, ls()).await), RefusalClass::UntrustedLog);

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        started(&runtime);
        runtime.store().forget_policy_files();
        assert_eq!(class(handle(&runtime, ls()).await), RefusalClass::PolicyUnavailable);

        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        started(&runtime);
        let unprepared = HookEvent::ChildStart {
            root: root.clone(),
            child: TrajectoryId("cc:s1:a1".to_string()),
            spawn: SpawnRef::Binding(appa_runtime_api::SpawnBinding("no-such-fork".to_string())),
        };
        assert_eq!(class(handle(&runtime, unprepared).await), RefusalClass::SpawnNotTaken);
    }

    #[tokio::test]
    async fn an_unreadable_hook_event_is_a_400() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let (status, answer) = hook(&runtime, b"not json").await;
        assert_eq!(status, 400);
        assert!(
            answer["error"]
                .as_str()
                .expect("the refusal names its cause")
                .starts_with("unreadable hook event: "),
        );
    }

    #[tokio::test]
    async fn a_malformed_hook_event_is_a_409() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let event = serde_json::json!({"hook_event_name": "PreToolUse", "session_id": "s1"});
        let (status, answer) = hook(&runtime, &json(&event)).await;
        assert_eq!(status, 409);
        assert_eq!(answer, serde_json::json!({"error": "PreToolUse without a tool call"}));
    }

    /// A deployment that declares the reporting tool, so a proposal of it is released rather
    /// than refused as undeclared.
    fn yelling_runtime(dir: &tempfile::TempDir) -> Runtime {
        let text = r#"
            [policy]
            version = 2

            [[policy.tool]]
            name = "mcp/appa/yell"

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#;
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        Runtime::open(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
        )
        .expect("the fixture deployment opens")
    }

    #[cfg(feature = "daemon")]
    /// The block's reason the model reads for a spawn proposed before its return is declared,
    /// under a deployment that does or does not let an agent report.
    async fn spawn_block_reason(agent_yell: bool) -> String {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let text = format!(
            r#"
            [reporting]
            agent_yell = {agent_yell}

            [policy]
            version = 2

            [[policy.tool]]
            name = "host/claude-code/Agent"

            [policy.deployment]
            context_control = true

            [externals]
            timeout_ms = 1000
            max_body_bytes = 4096
        "#
        );
        let path = dir.path().join("appa.toml");
        std::fs::write(&path, text).expect("the fixture writes");
        // Served as Claude Code, so tool names come back the way that host spells them.
        let runtime = Runtime::open_served(
            Config::load(&path).expect("the fixture validates"),
            dir.path().join("appa.db"),
            None,
            appa_adapter_claude_code::adapter(),
        )
        .expect("the fixture deployment opens");
        let spawn = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "Agent",
            "tool_input": {"prompt": "look it up"},
        });
        let (status, answer) = hook(&runtime, &json(&spawn)).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["hookSpecificOutput"]["permissionDecision"], "deny", "{answer}");
        answer["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .expect("a block carries its reason")
            .to_string()
    }

    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn a_block_points_at_yell_only_when_the_agent_may_report() {
        let reporting = spawn_block_reason(true).await;
        assert!(
            reporting.contains("Report:\n  If a plan makes no sense") && reporting.contains("call mcp__appa__yell."),
            "a block names the reporting tool as Claude Code calls it: {reporting}"
        );
        let silent = spawn_block_reason(false).await;
        assert!(
            !silent.contains("Report:") && !silent.contains("yell"),
            "a deployment that does not serve yell never mentions it: {silent}"
        );
    }

    fn yell_call(message: &str, with_trajectory: bool) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "tool_name": "mcp__appa__yell",
            "tool_input": {"message": message, "with_trajectory": with_trajectory},
        })
    }

    fn ticket(message: &str, with_trajectory: bool) -> crate::api::PermitKey {
        crate::yell::YellArgs {
            message: message.to_string(),
            with_trajectory,
        }
        .ticket()
    }

    /// An MCP request names no session, so the report a `yell` builds is about whatever the
    /// hook before it attested. Two sessions on one machine are what makes this matter.
    #[tokio::test]
    async fn a_released_yell_vouches_for_the_trajectory_that_made_it() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = yelling_runtime(&dir);
        let released = hook(&runtime, &json(&yell_call("the hook blocked", true))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        assert_eq!(
            runtime.take_vouched(&ticket("the hook blocked", true)),
            Ok((
                Actor {
                    root: TrajectoryId("cc:s1".to_string()),
                    child: None,
                },
                None
            ))
        );
    }

    /// The vouch is what lets a report be built at all, so a call the policy refused must
    /// leave nothing behind for the tool to spend.
    #[tokio::test]
    async fn a_blocked_yell_vouches_for_nobody() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        // The default fixture declares no `yell` and has no wildcard: undeclared is refused.
        let runtime = open_runtime(&dir);
        let refused = hook(&runtime, &json(&yell_call("the hook blocked", true))).await;
        assert_ne!(refused.1["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            runtime.take_vouched(&ticket("the hook blocked", true)),
            Err(crate::api::Unvouched::Nobody)
        );
    }

    /// The standing is for the call the hook saw. A tool that then reports something else is
    /// spending a vouch that was never given for it.
    #[tokio::test]
    async fn a_yell_vouch_answers_only_the_call_it_was_given_for() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = yelling_runtime(&dir);
        let released = hook(&runtime, &json(&yell_call("the hook blocked", true))).await;
        assert_eq!(released.1["hookSpecificOutput"]["permissionDecision"], "allow");

        let nobody = Err(crate::api::Unvouched::Nobody);
        assert_eq!(runtime.take_vouched(&ticket("the hook blocked", false)), nobody);
        assert_eq!(runtime.take_vouched(&ticket("something else", true)), nobody);
        assert!(runtime.take_vouched(&ticket("the hook blocked", true)).is_ok());
    }

    /// One turn's standing, like the control tool's: the harness may decline the call after
    /// the hook released it, and nothing later may spend what that turn left behind.
    #[tokio::test]
    async fn an_unspent_yell_vouch_does_not_outlive_its_turn() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = yelling_runtime(&dir);
        hook(&runtime, &json(&yell_call("the hook blocked", true))).await;

        let actor = Actor {
            root: TrajectoryId("cc:s1".to_string()),
            child: None,
        };
        crate::hooks::handle(
            &runtime,
            HookEvent::TurnEnd {
                actor,
                open_calls: appa_runtime_api::OpenCalls::Abandon,
            },
        )
        .await;
        assert_eq!(
            runtime.take_vouched(&ticket("the hook blocked", true)),
            Err(crate::api::Unvouched::Nobody)
        );
    }

    /// One of a turn's parallel single spawns, as Claude Code proposes it: its own host call id.
    fn parallel_spawn(root: &TrajectoryId, prompt: &str, call_id: &str) -> HookEvent {
        HookEvent::ToolCall {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            call: crate::api::ProposedCall {
                tool: "host/claude-code/Task".to_string(),
                arguments: crate::api::raw(serde_json::json!({ "prompt": prompt })),
                cwd: None,
            },
            call_id: Some(call_id.to_string()),
            spawn: Some(SpawnKind::Single),
            prompt: None,
            ruling: None,
            declares: None,
        }
    }

    /// Declare a spawn's return, so its next proposal is the one that may release.
    async fn declared(runtime: &Runtime, root: &TrajectoryId, prompt: &str) {
        let HookDecision::DenyCall { offers, .. } =
            handle(runtime, parallel_spawn(root, prompt, &format!("{prompt}-menu"))).await
        else {
            panic!("a marked spawn blocks until its return is declared");
        };
        declare_return(runtime, root, &offers).await;
    }

    fn started(root: &TrajectoryId, child: &str) -> HookEvent {
        HookEvent::ChildStart {
            root: root.clone(),
            child: TrajectoryId(child.to_string()),
            spawn: SpawnRef::InFlight,
        }
    }

    fn forks(runtime: &Runtime, root: &TrajectoryId) -> usize {
        runtime
            .audit(root)
            .expect("the audit reads")
            .into_iter()
            .filter(|entry| matches!(entry.event, crate::api::AuditEvent::Forked { .. }))
            .count()
    }

    #[tokio::test]
    async fn a_parallel_spawn_is_held_until_the_earlier_one_binds_its_child() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        declared(&runtime, &root, "first").await;
        declared(&runtime, &root, "second").await;
        let HookDecision::AllowCall { spawn: Some(_) } = handle(&runtime, parallel_spawn(&root, "first", "a")).await
        else {
            panic!("the first spawn releases");
        };

        let mut second = std::pin::pin!(handle(&runtime, parallel_spawn(&root, "second", "b")));
        tokio::select! {
            biased;
            decided = &mut second => panic!("the second spawn is held while the first waits, got {decided:?}"),
            () = std::future::ready(()) => {}
        }
        assert_eq!(handle(&runtime, started(&root, "cc:s1:a1")).await, HookDecision::Ack);
        let HookDecision::AllowCall { spawn: Some(_) } = second.await else {
            panic!("the held spawn releases once the first child is bound");
        };
        assert_eq!(
            handle(&runtime, started(&root, "cc:s1:a2")).await,
            HookDecision::Ack,
            "the second child binds the one spawn in flight"
        );
        assert_eq!(forks(&runtime, &root), 2);
    }

    #[tokio::test]
    async fn a_held_spawn_is_released_when_the_earlier_spawn_fails_before_its_child_starts() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        declared(&runtime, &root, "first").await;
        declared(&runtime, &root, "second").await;
        let HookDecision::AllowCall { spawn: Some(_) } = handle(&runtime, parallel_spawn(&root, "first", "a")).await
        else {
            panic!("the first spawn releases");
        };

        let mut second = std::pin::pin!(handle(&runtime, parallel_spawn(&root, "second", "b")));
        tokio::select! {
            biased;
            decided = &mut second => panic!("the second spawn is held while the first waits, got {decided:?}"),
            () = std::future::ready(()) => {}
        }
        let HookEvent::ToolCall { actor, call, .. } = parallel_spawn(&root, "first", "a") else {
            unreachable!("a spawn is a tool call");
        };
        let failed = HookEvent::ToolResult {
            actor,
            call,
            call_id: Some("a".to_string()),
            outcome: ToolOutcome::Failure {
                message: "the subagent did not start".to_string(),
            },
        };
        assert_eq!(handle(&runtime, failed).await, HookDecision::Ack);
        let HookDecision::AllowCall { spawn: Some(_) } = second.await else {
            panic!("the held spawn releases once the first stops waiting for a child");
        };
        assert_eq!(handle(&runtime, started(&root, "cc:s1:a2")).await, HookDecision::Ack);
        assert_eq!(forks(&runtime, &root), 1);
    }

    /// A host settles a result's dispatch on a block, so a result the log could not take is
    /// refused rather than blocked, and the same report lands once the log is free.
    #[tokio::test]
    async fn a_contended_result_is_refused_and_stays_reportable() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let actor = Actor {
            root: TrajectoryId("cc:s1".to_string()),
            child: None,
        };
        let call = crate::api::ProposedCall {
            tool: "host/claude-code/Bash".to_string(),
            arguments: crate::api::raw(serde_json::json!({"command": "true"})),
            cwd: None,
        };
        let proposed = HookEvent::ToolCall {
            actor: actor.clone(),
            call: call.clone(),
            call_id: Some("toolu-1".to_string()),
            spawn: None,
            prompt: None,
            ruling: None,
            declares: None,
        };
        assert!(matches!(
            handle(&runtime, proposed).await,
            HookDecision::AllowCall { .. }
        ));
        let reported = || HookEvent::ToolResult {
            actor: actor.clone(),
            call: call.clone(),
            call_id: Some("toolu-1".to_string()),
            outcome: ToolOutcome::Success {
                body: appa_runtime_api::OutcomeBody::Available("ok".to_string()),
            },
        };
        runtime.store().contend_next_appends(u64::MAX);
        let contended = handle(&runtime, reported()).await;
        runtime.store().contend_next_appends(0);
        assert!(
            matches!(
                contended,
                HookDecision::Refuse {
                    class: appa_runtime_api::RefusalClass::Contended,
                    ..
                }
            ),
            "{contended:?}"
        );
        assert_eq!(handle(&runtime, reported()).await, HookDecision::Ack);
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_spawn_is_refused_as_outstanding_when_the_earlier_one_never_binds() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let runtime = open_runtime(&dir);
        let root = TrajectoryId("cc:s1".to_string());
        declared(&runtime, &root, "first").await;
        declared(&runtime, &root, "second").await;
        let HookDecision::AllowCall { spawn: Some(_) } = handle(&runtime, parallel_spawn(&root, "first", "a")).await
        else {
            panic!("the first spawn releases");
        };

        let held_since = tokio::time::Instant::now();
        assert_eq!(
            handle(&runtime, parallel_spawn(&root, "second", "b")).await,
            deny(EventError::SpawnOutstanding.to_string())
        );
        assert!(held_since.elapsed() >= SPAWN_HOLD);

        assert_eq!(handle(&runtime, started(&root, "cc:s1:a1")).await, HookDecision::Ack);
        let HookDecision::AllowCall { spawn: Some(_) } = handle(&runtime, parallel_spawn(&root, "second", "b2")).await
        else {
            panic!("the refused spawn consumed nothing: proposed again, it releases");
        };
    }
}
