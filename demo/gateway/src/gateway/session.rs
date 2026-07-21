//! The per-MCP-session decision core: one trajectory, mediated tool calls,
//! soft blocks, and the escalation loop. No MCP types here — the gateway
//! binary renders [`Outcome`]s into tool results and supplies the human
//! approval callback; tests drive this module directly.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use appa_core::{
    ArgumentTree, AuthorityName, BlockReason, EmissionPursuit, EmissionRequest, ExecutionToken, FlowOutcome,
    FlowPermit, OpaqueValue, Pursuit, Ruling, StallCause, ToolName, ToolRequest, UserId, ValueId, Violation,
};

use crate::gateway::config::{GatewayConfig, ToolSim};

const MAX_REMEDY_STEPS: usize = 32;
const MAX_APPROVAL_ROUNDS: usize = 32;

/// How one mediated call (or escalation) settled. Rendering to MCP text
/// happens at the edge; tests assert on these.
#[derive(Debug)]
pub enum Outcome {
    Executed { tool: ToolName, result: String },
    SoftBlocked {
        tool: ToolName,
        violations: Vec<Violation>,
        recipients: BTreeSet<UserId>,
    },
    TerminalBlocked {
        tool: ToolName,
        reason: BlockReason,
        violations: Vec<Violation>,
    },
    Granted { tool: ToolName, result: String },
    Denied { tool: ToolName, reason: String },
    EscalationUnavailable { tool: ToolName },
    NothingPending,
    RemedyStalled {
        tool: ToolName,
        violations: Vec<Violation>,
        cause: StallCause,
    },
    ExecutorFailed { tool: ToolName, reason: String },
    BadArguments { tool: ToolName, reason: String },
    UnknownTool { tool: String },
    Refused { tool: ToolName, reason: String },
    Responded { rendered: String },
    ResponseBlocked { reason: String, violations: Vec<Violation> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingWire {
    tool: ToolName,
    args: BTreeMap<String, String>,
    recipients: BTreeSet<UserId>,
}

pub struct Session {
    config: Arc<GatewayConfig>,
    trajectory: appa_core::Trajectory,
    context: BTreeSet<ValueId>,
    pending_wire: Option<PendingWire>,
}

impl Session {
    pub fn new(config: Arc<GatewayConfig>) -> Self {
        Self {
            config,
            trajectory: appa_core::Trajectory::new(),
            context: BTreeSet::new(),
            pending_wire: None,
        }
    }

    fn shared(&self) -> Arc<GatewayConfig> {
        Arc::clone(&self.config)
    }

    pub fn call_tool(&mut self, tool: &str, args: &serde_json::Map<String, serde_json::Value>) -> Outcome {
        let Some(sim) = self.config.tools.get(&ToolName::new(tool)).cloned() else {
            return Outcome::UnknownTool { tool: tool.to_owned() };
        };
        let args = match wire_args(&sim, args) {
            Ok(args) => args,
            Err(reason) => {
                return Outcome::BadArguments {
                    tool: sim.name.clone(),
                    reason,
                };
            }
        };
        let recipients = recipients_of(&sim, &args);

        if self.trajectory.pending_action().is_some() {
            let matches = self
                .pending_wire
                .as_ref()
                .is_some_and(|wire| wire.tool == sim.name && wire.args == args);
            if matches {
                let request = self
                    .trajectory
                    .pending_action()
                    .expect("pending action checked above")
                    .original()
                    .clone();
                return self.settle(&sim, request, recipients);
            }
            self.trajectory
                .abandon_pending()
                .expect("the gateway settles dispatches synchronously, so no released action lingers");
            self.pending_wire = None;
        }

        let mut leaves = BTreeMap::new();
        for (name, value) in &args {
            match self.trajectory.admit_model_output(
                OpaqueValue::new(value),
                self.context.clone(),
                self.context.clone(),
            ) {
                Ok(id) => {
                    leaves.insert(name.clone(), ArgumentTree::Value(id));
                }
                Err(unknown) => {
                    return Outcome::Refused {
                        tool: sim.name.clone(),
                        reason: format!("context value vanished: {unknown}"),
                    };
                }
            }
        }
        let request = ToolRequest::new(sim.name.clone(), ArgumentTree::object(leaves), self.context.clone());
        let outcome = self.settle(&sim, request, recipients.clone());
        if let Outcome::SoftBlocked { .. } = &outcome {
            self.pending_wire = Some(PendingWire {
                tool: sim.name.clone(),
                args,
                recipients,
            });
        }
        outcome
    }

    fn settle(&mut self, sim: &ToolSim, request: ToolRequest, recipients: BTreeSet<UserId>) -> Outcome {
        match self.shared().engine.evaluate(&mut self.trajectory, request) {
            Err(refusal) => {
                self.pending_wire = None;
                Outcome::Refused {
                    tool: sim.name.clone(),
                    reason: refusal.to_string(),
                }
            }
            Ok(FlowOutcome::AllowedNow(token)) => self.dispatch(sim, token),
            Ok(FlowOutcome::Blocked {
                violations,
                terminal: None,
                ..
            }) => Outcome::SoftBlocked {
                tool: sim.name.clone(),
                violations,
                recipients,
            },
            Ok(FlowOutcome::Blocked {
                violations,
                terminal: Some(reason),
                ..
            }) => {
                self.pending_wire = None;
                Outcome::TerminalBlocked {
                    tool: sim.name.clone(),
                    reason,
                    violations,
                }
            }
        }
    }

    pub async fn escalate<F, Fut>(&mut self, reason: &str, mut ask: F) -> Outcome
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Option<bool>>,
    {
        let Some(pending) = self.trajectory.pending_action() else {
            return Outcome::NothingPending;
        };
        let request = pending.original().clone();
        let tool = request.tool.clone();
        let Some(sim) = self.config.tools.get(&tool).cloned() else {
            return Outcome::UnknownTool {
                tool: tool.as_str().to_owned(),
            };
        };
        let (args, recipients) = self
            .pending_wire
            .as_ref()
            .map(|wire| (wire.args.clone(), wire.recipients.clone()))
            .unwrap_or_default();

        let mut verdicts: BTreeMap<AuthorityName, bool> = BTreeMap::new();
        for _ in 0..MAX_APPROVAL_ROUNDS {
            let pending_approval =
                match self
                    .shared()
                    .engine
                    .pursue(&mut self.trajectory, request.clone(), MAX_REMEDY_STEPS)
                {
                    Pursuit::Permitted(token) => return self.granted(&sim, token),
                    Pursuit::Terminal { violations, reason } => {
                        return self.denied_or_terminal(&tool, violations, reason);
                    }
                    Pursuit::Refused(refusal) => {
                        self.pending_wire = None;
                        return Outcome::Refused {
                            tool,
                            reason: refusal.to_string(),
                        };
                    }
                    Pursuit::NeedsApproval(pending_approval) => pending_approval,
                    Pursuit::Stalled { violations, cause } => {
                        self.pending_wire = None;
                        return Outcome::RemedyStalled {
                            tool,
                            violations,
                            cause,
                        };
                    }
                };

            let authority = pending_approval.authority().clone();
            let verdict = match verdicts.get(&authority) {
                Some(verdict) => *verdict,
                None => {
                    let message = approval_message(&tool, &args, &recipients, reason, &pending_approval);
                    let Some(verdict) = ask(message).await else {
                        return Outcome::EscalationUnavailable { tool };
                    };
                    verdicts.insert(authority, verdict);
                    verdict
                }
            };
            let ruling = match verdict {
                true => Ruling::Approve {
                    reason: format!("operator approved escalation: {reason}"),
                },
                false => Ruling::Deny {
                    reason: "operator declined the escalation".to_owned(),
                },
            };
            match self
                .shared()
                .engine
                .apply_approval(&mut self.trajectory, pending_approval, ruling)
            {
                Ok(FlowOutcome::AllowedNow(FlowPermit::Execute(token))) => return self.granted(&sim, token),
                Ok(FlowOutcome::AllowedNow(FlowPermit::Emit(_))) => {
                    unreachable!("a tool flow's approval settles in an execution permit")
                }
                Ok(FlowOutcome::Blocked { terminal: None, .. }) => continue,
                Ok(FlowOutcome::Blocked {
                    violations,
                    terminal: Some(reason),
                    ..
                }) => {
                    return self.denied_or_terminal(&tool, violations, reason);
                }
                Err(refused) => {
                    self.pending_wire = None;
                    return Outcome::Refused {
                        tool,
                        reason: refused.to_string(),
                    };
                }
            }
        }
        self.trajectory
            .abandon_pending()
            .expect("a stalled action was never released");
        self.pending_wire = None;
        Outcome::RemedyStalled {
            tool,
            violations: Vec::new(),
            cause: StallCause::BoundExhausted,
        }
    }

    fn granted(&mut self, sim: &ToolSim, token: ExecutionToken) -> Outcome {
        match self.dispatch(sim, token) {
            Outcome::Executed { tool, result } => Outcome::Granted { tool, result },
            other => other,
        }
    }

    fn denied_or_terminal(
        &mut self,
        tool: &ToolName,
        violations: Vec<appa_core::Violation>,
        reason: BlockReason,
    ) -> Outcome {
        self.pending_wire = None;
        match reason {
            BlockReason::DeniedByAuthority { authority, reason } => Outcome::Denied {
                tool: tool.clone(),
                reason: format!("{authority}: {reason}"),
            },
            reason => Outcome::TerminalBlocked {
                tool: tool.clone(),
                reason,
                violations,
            },
        }
    }

    fn dispatch(&mut self, sim: &ToolSim, token: ExecutionToken) -> Outcome {
        self.pending_wire = None;
        let (canonical, receipt) = match self.trajectory.release(token) {
            Ok(released) => released,
            Err(rejected) => {
                return Outcome::Refused {
                    tool: sim.name.clone(),
                    reason: rejected.to_string(),
                };
            }
        };
        let result = canonical_args(&canonical.rendered).and_then(|args| sim.render_result(&args));
        match result {
            Ok(result) => match self.trajectory.record_output(receipt, OpaqueValue::new(&result)) {
                Ok(value) => {
                    self.context.insert(value);
                    Outcome::Executed {
                        tool: sim.name.clone(),
                        result,
                    }
                }
                Err(rejected) => Outcome::Refused {
                    tool: sim.name.clone(),
                    reason: rejected.to_string(),
                },
            },
            Err(reason) => match self.trajectory.record_failure(receipt) {
                Ok(()) => Outcome::ExecutorFailed {
                    tool: sim.name.clone(),
                    reason,
                },
                Err(rejected) => Outcome::Refused {
                    tool: sim.name.clone(),
                    reason: format!("{reason}; and the failure receipt was refused: {rejected}"),
                },
            },
        }
    }

    pub fn respond(&mut self, text: &str) -> Outcome {
        let sink = ToolName::new("appa__respond");
        let body =
            match self
                .trajectory
                .admit_model_output(OpaqueValue::new(text), self.context.clone(), self.context.clone())
            {
                Ok(id) => id,
                Err(unknown) => {
                    return Outcome::Refused {
                        tool: sink,
                        reason: format!("context value vanished: {unknown}"),
                    };
                }
            };
        let request = EmissionRequest {
            body: ArgumentTree::Value(body),
            control: BTreeSet::new(),
            basis: self.trajectory.revision(),
        };
        match self
            .shared()
            .engine
            .pursue_emission(&mut self.trajectory, request, MAX_REMEDY_STEPS)
        {
            EmissionPursuit::Emitted(emitted) => Outcome::Responded {
                rendered: emitted.rendered,
            },
            EmissionPursuit::Terminal { violations, reason } => Outcome::ResponseBlocked {
                reason: reason.to_string(),
                violations,
            },
            EmissionPursuit::NeedsApproval(pending) => {
                let reason = format!(
                    "response needs an external ruling from {}; response escalation is not wired in this demo",
                    pending.authority()
                );
                drop(pending);
                self.trajectory.abandon_pending_emission();
                Outcome::ResponseBlocked {
                    reason,
                    violations: Vec::new(),
                }
            }
            EmissionPursuit::Stalled { violations, cause } => Outcome::ResponseBlocked {
                reason: format!("emission remedy stalled: {cause:?}"),
                violations,
            },
            EmissionPursuit::Refused(refusal) => Outcome::Refused {
                tool: sink,
                reason: refusal.to_string(),
            },
        }
    }

    pub fn audit(&self) -> impl Iterator<Item = String> {
        self.trajectory.audit().iter().map(|event| event.to_string())
    }
}

fn wire_args(
    sim: &ToolSim,
    args: &serde_json::Map<String, serde_json::Value>,
) -> Result<BTreeMap<String, String>, String> {
    let declared = sim.declared_args();
    let mut out = BTreeMap::new();
    for (name, value) in args {
        if !declared.contains(name.as_str()) {
            return Err(format!("undeclared argument `{name}`"));
        }
        match value {
            serde_json::Value::String(s) => {
                out.insert(name.clone(), s.clone());
            }
            other => return Err(format!("argument `{name}` must be a string, got {other}")),
        }
    }
    for arg in &sim.args {
        if arg.required && !out.contains_key(&arg.name) {
            return Err(format!("missing required argument `{}`", arg.name));
        }
    }
    Ok(out)
}

fn recipients_of(sim: &ToolSim, args: &BTreeMap<String, String>) -> BTreeSet<UserId> {
    sim.recipients_arg
        .as_ref()
        .and_then(|arg| args.get(arg))
        .map(|value| BTreeSet::from([UserId::new(value)]))
        .unwrap_or_default()
}

fn canonical_args(rendered: &str) -> Result<BTreeMap<String, String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(rendered).map_err(|e| format!("canonical request did not parse: {e}"))?;
    let serde_json::Value::Object(fields) = value else {
        return Err("canonical request is not an object".to_owned());
    };
    fields
        .into_iter()
        .map(|(name, value)| match value {
            serde_json::Value::String(s) => Ok((name, s)),
            other => Err(format!("canonical argument `{name}` is not a string: {other}")),
        })
        .collect()
}

fn approval_message(
    tool: &ToolName,
    args: &BTreeMap<String, String>,
    recipients: &BTreeSet<UserId>,
    reason: &str,
    pending: &appa_core::PendingApproval,
) -> String {
    let recipients = match recipients.is_empty() {
        true => "no declared recipients".to_owned(),
        false => recipients
            .iter()
            .map(|r| r.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", "),
    };
    let args = match args.is_empty() {
        true => "  (no arguments)".to_owned(),
        false => args
            .iter()
            .map(|(name, value)| format!("  {name}: {}", quote(value)))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    format!(
        "The agent wants to run `{tool}` (recipients: {recipients}), which policy blocks.\n\
         {args}\n\
         Agent's reason (unverified): {reason}\n\
         Grant needed: {grant} — ruled by `{authority}`; one ruling covers every grant \
         this remedy routes to this authority.\n\
         Accept to allow; decline to block.",
        reason = quote(reason),
        grant = pending.grant(),
        authority = pending.authority(),
    )
}

fn quote(text: &str) -> String {
    const MAX: usize = 300;
    let clipped: String = text.chars().take(MAX).collect();
    let mut quoted = serde_json::to_string(&clipped).expect("a string always serializes");
    if text.chars().count() > MAX {
        quoted.push('…');
    }
    quoted
}
