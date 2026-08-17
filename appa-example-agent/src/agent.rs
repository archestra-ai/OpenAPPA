//! The loop: inference, tool calls, children, and the runtime gate in
//! front of every flow.

use std::collections::VecDeque;
use std::sync::Arc;

use appa_runtime_api::{
    Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, SpawnBinding, SpawnRef, ToolOutcome, TrajectoryId,
};
use appa_runtime_v2::api::{OfferId, RemedyOutcome, Runtime};
use appa_runtime_v2::hooks;
use serde_json::value::RawValue;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::budget::{Exhausted, Limits, RunBudget};
use crate::provider::OpenAiCompatible;
use crate::record::{CallId, Record, Recorded};
use crate::tools::{CONTROL_TOOL, ToolCatalogue, ToolShim};
use crate::wire::{ChatCompletionRequest, WireMessage, WireToolCall};

/// The transcript's opening messages: the host's own configuration,
/// never the policy's.
#[derive(Clone, Debug, Default)]
pub struct TranscriptHead(Vec<WireMessage>);

impl TranscriptHead {
    pub fn new(messages: Vec<WireMessage>) -> Self {
        TranscriptHead(messages)
    }
}

/// What one trajectory has said and been told, in provider order.
#[derive(Clone, Debug, Default)]
pub struct Transcript(Vec<WireMessage>);

impl Transcript {
    pub fn messages(&self) -> &[WireMessage] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(value: impl Into<String>) -> Self {
        ToolName(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgumentKey(String);

impl ArgumentKey {
    pub fn new(value: impl Into<String>) -> Self {
        ArgumentKey(value.into())
    }
}

/// The host's spawn tool: releasing it opens a child trajectory.
/// It is an ordinary registered contract — the runtime
/// checks it like any call — and this names which one the agent acts
/// on, and which argument carries the errand.
#[derive(Clone, Debug)]
pub struct SpawnTool {
    pub name: ToolName,
    pub errand: ArgumentKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Answer(String),
    Stopped(StopReason),
}

/// Why a run stopped without an answer. Every variant is fail-closed:
/// the agent stops rather than proceeding past something it cannot
/// account for.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StopReason {
    #[error("the run was cancelled")]
    Cancelled,
    #[error("the run exhausted its budget")]
    BudgetExhausted,
    #[error("inference failed: {0}")]
    InferenceFailed(String),
    #[error("the runtime refused the run: {0}")]
    Refused(String),
}

/// A serial agent over one runtime and one provider.
///
/// It owns everything the runtime deliberately does not: the
/// transcript, the tool catalogue, the budget, and the parent/child
/// stack. The runtime owns the decisions.
pub struct Agent {
    runtime: Arc<Runtime>,
    provider: OpenAiCompatible,
    tools: ToolShim,
    catalogue: ToolCatalogue,
    head: TranscriptHead,
    spawn: Option<SpawnTool>,
    limits: Limits,
    observer: Option<tokio::sync::mpsc::Sender<Recorded>>,
}

impl Agent {
    pub fn new(runtime: Arc<Runtime>, provider: OpenAiCompatible, tools: ToolShim, catalogue: ToolCatalogue) -> Self {
        Agent {
            runtime,
            provider,
            tools,
            catalogue,
            head: TranscriptHead::default(),
            spawn: None,
            limits: Limits::default(),
            observer: None,
        }
    }

    pub fn with_head(mut self, head: TranscriptHead) -> Self {
        self.head = head;
        self
    }

    /// Without a spawn tool the agent never opens a child, whatever
    /// the policy allows.
    pub fn with_spawn_tool(mut self, spawn: SpawnTool) -> Self {
        self.spawn = Some(spawn);
        self
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Send every [`Recorded`] line to this channel as well as to the
    /// console target. A host that renders its agent's progress needs
    /// the records typed; one that only logs does not install this.
    pub fn with_observer(mut self, observer: tokio::sync::mpsc::Sender<Recorded>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Drive one run to its end. `root` is this run's trajectory id: a
    /// reused id MUST NOT continue another trajectory's history,
    /// so a host mints a fresh one per run.
    pub async fn run(&self, root: TrajectoryId, task: impl Into<String>, cancel: CancellationToken) -> Outcome {
        self.turn(root, &mut Transcript::default(), task, cancel).await
    }

    /// Take one turn on a trajectory the host keeps across turns.
    pub async fn turn(
        &self,
        root: TrajectoryId,
        transcript: &mut Transcript,
        task: impl Into<String>,
        cancel: CancellationToken,
    ) -> Outcome {
        let mut run = Run {
            agent: self,
            root: root.clone(),
            budget: RunBudget::new(self.limits.clone()),
            cancel,
        };
        let task = task.into();

        if let Err(stop) = run.expect_ack(HookEvent::SessionStart { root: root.clone() }).await {
            return Outcome::Stopped(stop);
        }
        let prompt = HookEvent::Prompt {
            actor: Actor {
                root: root.clone(),
                child: None,
            },
            text: task.clone(),
        };
        if let Err(stop) = run.expect_ack(prompt).await {
            return Outcome::Stopped(stop);
        }

        let mut opening = std::mem::take(&mut transcript.0);
        opening.push(WireMessage::user(task));
        let (result, messages) = run.drive(Frame::root(root, opening)).await;
        transcript.0 = messages;
        match result {
            Ok(answer) => {
                transcript.0.push(WireMessage::assistant(answer.clone()));
                Outcome::Answer(answer)
            }
            Err(stop) => Outcome::Stopped(stop),
        }
    }
}

struct Frame {
    id: TrajectoryId,
    depth: u32,
    transcript: Vec<WireMessage>,
    pending: VecDeque<WireToolCall>,
}

impl Frame {
    fn root(id: TrajectoryId, transcript: Vec<WireMessage>) -> Self {
        Frame {
            id,
            depth: 0,
            transcript,
            pending: VecDeque::new(),
        }
    }

    fn child(&self, id: TrajectoryId, errand: String) -> Self {
        Frame {
            id,
            depth: self.depth + 1,
            transcript: vec![WireMessage::user(errand)],
            pending: VecDeque::new(),
        }
    }
}

struct Suspended {
    frame: Frame,
    call: ProposedCall,
    reply_to: String,
}

struct Run<'a> {
    agent: &'a Agent,
    root: TrajectoryId,
    budget: RunBudget,
    cancel: CancellationToken,
}

impl Run<'_> {
    async fn drive(&mut self, root_frame: Frame) -> (Result<String, StopReason>, Vec<WireMessage>) {
        let mut frame = root_frame;
        let mut parents: Vec<Suspended> = Vec::new();

        let result = loop {
            if self.cancel.is_cancelled() {
                break Err(StopReason::Cancelled);
            }
            if self.budget.deadline_elapsed() {
                break Err(StopReason::BudgetExhausted);
            }

            let Some(call) = frame.pending.pop_front() else {
                let message = match self.infer(&frame).await {
                    Ok(message) => message,
                    Err(stop) => break Err(stop),
                };
                let calls = message.tool_calls.clone().unwrap_or_default();
                if calls.is_empty() {
                    match parents.pop() {
                        None => {
                            let answer = message.content.unwrap_or_default();
                            self.record(&frame, Record::Answers { text: answer.clone() }).await;
                            break Ok(answer);
                        }
                        Some(mut parent) => {
                            let crossed = self.return_to_parent(&mut parent, &frame, message.content).await;
                            frame = parent.frame;
                            if let Err(stop) = crossed {
                                break Err(stop);
                            }
                        }
                    }
                    continue;
                }
                if let Some(said) = message.content.as_ref().filter(|said| !said.trim().is_empty()) {
                    self.record(&frame, Record::Says { text: said.clone() }).await;
                }
                frame
                    .transcript
                    .push(WireMessage::assistant_tool_calls(message.content, calls.clone()));
                frame.pending.extend(calls);
                continue;
            };

            if self.budget.charge_tool_call() == Err(Exhausted) {
                break Err(StopReason::BudgetExhausted);
            }
            match self.answer_call(&mut frame, &call).await {
                Ok(Answered::Reply(text)) => frame.transcript.push(WireMessage::tool_result(&call.id, text)),
                Ok(Answered::Spawned {
                    child,
                    call: spawn,
                    errand,
                }) => {
                    let opened = frame.child(child, errand.clone());
                    parents.push(Suspended {
                        frame,
                        call: spawn,
                        reply_to: call.id.clone(),
                    });
                    frame = opened;
                    let depth = frame.depth;
                    self.record(&frame, Record::Forked { depth, errand }).await;
                }
                Err(stop) => break Err(stop),
            }
        };

        let root = match parents.into_iter().next() {
            Some(outermost) => outermost.frame,
            None => frame,
        };
        (result, root.transcript)
    }

    async fn record(&self, frame: &Frame, record: Record) {
        if record.is_console_line() {
            tracing::debug!(target: "appa::decision", "appa: [{}] {}", frame.id.0, record.one_line());
        }
        if let Some(observer) = &self.agent.observer {
            let _ = observer
                .send(Recorded {
                    trajectory: frame.id.clone(),
                    record,
                })
                .await;
        }
    }

    async fn answer_call(&mut self, frame: &mut Frame, call: &WireToolCall) -> Result<Answered, StopReason> {
        let Ok(arguments) = RawValue::from_string(call.function.arguments.clone()) else {
            return Ok(Answered::Reply(
                "The arguments were not valid JSON, so the call was not made. Send the arguments as a JSON object."
                    .to_string(),
            ));
        };
        let proposed = ProposedCall {
            tool: call.function.name.clone(),
            arguments,
        };
        let id = CallId(call.id.clone());

        self.record(
            frame,
            Record::Proposes {
                call: id.clone(),
                tool: proposed.tool.clone(),
                arguments: proposed.arguments.get().to_string(),
            },
        )
        .await;
        let event = HookEvent::ToolCall {
            actor: self.actor(frame),
            call: proposed.clone(),
            spawn: self.marks_spawn(&proposed),
        };
        match hooks::handle(&self.agent.runtime, event).await {
            HookDecision::AllowCall { spawn } => self.run_released(frame, &id, proposed, spawn).await,
            HookDecision::DenyCall { feedback } => {
                self.record(
                    frame,
                    Record::Blocked {
                        call: id,
                        tool: proposed.tool.clone(),
                        feedback: feedback.clone(),
                    },
                )
                .await;
                Ok(Answered::Reply(feedback))
            }
            HookDecision::PassControl => self.execute_remedy(frame, &id, &proposed).await,
            HookDecision::Refuse { detail } => Err(StopReason::Refused(detail)),
            other => Err(unexpected("a proposed call", &other)),
        }
    }

    fn marks_spawn(&self, call: &ProposedCall) -> bool {
        self.agent.spawn.as_ref().is_some_and(|spawn| spawn.name.0 == call.tool)
    }

    async fn run_released(
        &mut self,
        frame: &mut Frame,
        id: &CallId,
        call: ProposedCall,
        binding: Option<SpawnBinding>,
    ) -> Result<Answered, StopReason> {
        if let Some(spawn) = self.agent.spawn.as_ref().filter(|spawn| spawn.name.0 == call.tool) {
            let errand = errand_of(&call, &spawn.errand);
            return self.open_child(frame, id, call, errand, binding).await;
        }
        let outcome = self.agent.tools.run(&call).await;
        self.report(frame, id, call, outcome).await.map(Answered::Reply)
    }

    async fn open_child(
        &mut self,
        frame: &mut Frame,
        id: &CallId,
        call: ProposedCall,
        errand: String,
        binding: Option<SpawnBinding>,
    ) -> Result<Answered, StopReason> {
        let Some(binding) = binding else {
            let outcome = ToolOutcome::Failure {
                message: "no child was opened: this spawn prepared no fork — the deployment does not \
                          control subagent context, or the call was a sanitizer's substitution"
                    .to_string(),
            };
            return self.report(frame, id, call, outcome).await.map(Answered::Reply);
        };
        if self.budget.charge_fork(frame.depth) == Err(Exhausted) {
            let outcome = ToolOutcome::Failure {
                message: "no child was opened: this run's fork budget is spent".to_string(),
            };
            return self.report(frame, id, call, outcome).await.map(Answered::Reply);
        }
        let child = TrajectoryId(format!("{}:c{}", self.root.0, self.budget.forks()));
        let event = HookEvent::ChildStart {
            root: self.root.clone(),
            child: child.clone(),
            spawn: SpawnRef::Binding(binding),
        };
        self.expect_ack(event).await?;
        Ok(Answered::Spawned { child, call, errand })
    }

    async fn return_to_parent(
        &mut self,
        parent: &mut Suspended,
        child: &Frame,
        said: Option<String>,
    ) -> Result<(), StopReason> {
        let event = HookEvent::ChildEnd {
            root: self.root.clone(),
            child: child.id.clone(),
            value: said.clone(),
        };
        let (outcome, crossed) = match hooks::handle(&self.agent.runtime, event).await {
            HookDecision::Ack => (spawn_closed(), said),
            HookDecision::ChildReturn { value } => (spawn_closed(), Some(value)),
            HookDecision::Block { reason } => {
                self.record(&parent.frame, Record::ReturnBlocked { reason: reason.clone() })
                    .await;
                (
                    ToolOutcome::Failure {
                        message: reason.clone(),
                    },
                    Some(reason),
                )
            }
            HookDecision::Refuse { detail } => return Err(StopReason::Refused(detail)),
            other => return Err(unexpected("a child return", &other)),
        };
        let id = CallId(parent.reply_to.clone());
        let call = parent.call.clone();
        let closed = self.report(&mut parent.frame, &id, call, outcome).await?;
        parent
            .frame
            .transcript
            .push(WireMessage::tool_result(&parent.reply_to, crossed.unwrap_or(closed)));
        Ok(())
    }

    async fn report(
        &mut self,
        frame: &mut Frame,
        id: &CallId,
        call: ProposedCall,
        outcome: ToolOutcome,
    ) -> Result<String, StopReason> {
        let raw = match &outcome {
            ToolOutcome::Success {
                body: OutcomeBody::Available(body),
            } => body.clone(),
            ToolOutcome::Success {
                body: OutcomeBody::Unavailable,
            } => String::new(),
            ToolOutcome::Failure { message } => message.clone(),
            ToolOutcome::Indeterminate => "The tool's outcome is unknown; it may have run.".to_string(),
        };
        let event = HookEvent::ToolResult {
            actor: self.actor(frame),
            call,
            outcome,
        };
        match hooks::handle(&self.agent.runtime, event).await {
            // The output crosses as produced.
            HookDecision::Ack => {
                self.record(
                    frame,
                    Record::Admitted {
                        call: id.clone(),
                        body: raw.clone(),
                    },
                )
                .await;
                Ok(raw)
            }
            HookDecision::ReplaceOutput { output } => {
                self.record(
                    frame,
                    Record::Substituted {
                        call: id.clone(),
                        body: output.clone(),
                    },
                )
                .await;
                Ok(output)
            }
            HookDecision::Block { reason } => {
                self.record(
                    frame,
                    Record::OutputBlocked {
                        call: id.clone(),
                        reason: reason.clone(),
                    },
                )
                .await;
                Ok(reason)
            }
            HookDecision::Refuse { detail } => Err(StopReason::Refused(detail)),
            other => Err(unexpected("a tool outcome", &other)),
        }
    }

    async fn execute_remedy(
        &mut self,
        frame: &mut Frame,
        id: &CallId,
        call: &ProposedCall,
    ) -> Result<Answered, StopReason> {
        let offer = serde_json::from_str::<serde_json::Value>(call.arguments.get())
            .ok()
            .and_then(|arguments| arguments.get("offer_id")?.as_str().map(str::to_string));
        let Some(offer) = offer else {
            return Ok(Answered::Reply(format!(
                "{CONTROL_TOOL} needs an offer_id, quoted exactly as the feedback surfaced it."
            )));
        };
        let acting = self.actor(frame);
        let reply = match self.agent.runtime.execute_remedy(&acting, OfferId(offer)).await {
            RemedyOutcome::Authorized { call } => {
                self.record(
                    frame,
                    Record::OfferTaken {
                        detail: format!("{} may now run", call.tool),
                    },
                )
                .await;
                format!(
                    "Authorized. Propose the {} call again with exactly these arguments; \
                     it will run without a new check: {}",
                    call.tool,
                    call.arguments.get(),
                )
            }
            RemedyOutcome::Substituted { call } => {
                self.record(
                    frame,
                    Record::OfferTaken {
                        detail: format!("{} runs with the sanitizer's replacement", call.tool),
                    },
                )
                .await;
                return self.run_substituted(frame, id, call).await;
            }
            RemedyOutcome::Returned { value } => {
                self.record(
                    frame,
                    Record::OfferTaken {
                        detail: "a value crossed".to_string(),
                    },
                )
                .await;
                value
            }
            RemedyOutcome::Declined { feedback } | RemedyOutcome::NoAnswer { feedback } => {
                self.record(
                    frame,
                    Record::OfferRefused {
                        feedback: feedback.clone(),
                    },
                )
                .await;
                feedback
            }
            RemedyOutcome::Refused { detail } => {
                self.record(
                    frame,
                    Record::OfferRefused {
                        feedback: detail.clone(),
                    },
                )
                .await;
                detail
            }
        };
        Ok(Answered::Reply(reply))
    }

    async fn run_substituted(
        &mut self,
        frame: &mut Frame,
        id: &CallId,
        call: ProposedCall,
    ) -> Result<Answered, StopReason> {
        let event = HookEvent::ToolCall {
            actor: self.actor(frame),
            call: call.clone(),
            spawn: self.marks_spawn(&call),
        };
        match hooks::handle(&self.agent.runtime, event).await {
            HookDecision::AllowCall { spawn } => self.run_released(frame, id, call, spawn).await,
            HookDecision::DenyCall { feedback } => Ok(Answered::Reply(feedback)),
            HookDecision::Refuse { detail } => Err(StopReason::Refused(detail)),
            other => Err(unexpected("a substituted call", &other)),
        }
    }

    async fn infer(&mut self, frame: &Frame) -> Result<WireMessage, StopReason> {
        if self.budget.charge_inference() == Err(Exhausted) {
            return Err(StopReason::BudgetExhausted);
        }
        let mut messages = self.agent.head.0.clone();
        messages.extend(frame.transcript.iter().cloned());
        let request = ChatCompletionRequest {
            model: String::new(),
            messages,
            tools: Some(self.agent.catalogue.advertised().to_vec()),
        };
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(StopReason::Cancelled),
            result = tokio::time::timeout(self.budget.remaining(), self.agent.provider.complete(request)) => {
                match result {
                    Ok(Ok(message)) => Ok(message),
                    Ok(Err(error)) => Err(StopReason::InferenceFailed(error.to_string())),
                    Err(_) => Err(StopReason::BudgetExhausted),
                }
            }
        }
    }

    async fn expect_ack(&self, event: HookEvent) -> Result<(), StopReason> {
        match hooks::handle(&self.agent.runtime, event).await {
            HookDecision::Ack => Ok(()),
            HookDecision::Refuse { detail } => Err(StopReason::Refused(detail)),
            other => Err(unexpected("a lifecycle event", &other)),
        }
    }

    fn actor(&self, frame: &Frame) -> Actor {
        Actor {
            root: self.root.clone(),
            child: (frame.id != self.root).then(|| frame.id.clone()),
        }
    }
}

enum Answered {
    Reply(String),
    Spawned {
        child: TrajectoryId,
        call: ProposedCall,
        errand: String,
    },
}

fn errand_of(call: &ProposedCall, key: &ArgumentKey) -> String {
    serde_json::from_str::<serde_json::Value>(call.arguments.get())
        .ok()
        .and_then(|arguments| Some(arguments.get(&key.0)?.as_str()?.to_string()))
        .unwrap_or_else(|| call.arguments.get().to_string())
}

fn spawn_closed() -> ToolOutcome {
    ToolOutcome::Success {
        body: OutcomeBody::Unavailable,
    }
}

fn unexpected(event: &str, decision: &HookDecision) -> StopReason {
    StopReason::Refused(format!(
        "{event} was answered with {decision:?}, which it cannot deliver"
    ))
}
