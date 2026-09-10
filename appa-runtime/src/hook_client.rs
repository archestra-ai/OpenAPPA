//! The hook client: this same binary, invoked as `appa hook`, translating one
//! host hook event read on stdin onto the canonical wire, posting it to the
//! running runtime, and printing the runtime's answer in the host's own shape.
//!
//! The harness spawns a process per hook, so the cost of reaching the runtime is
//! paid on every tool call. Doing it here rather than through `curl` spends one
//! process where a shell and a curl spent two, and spends it on the binary the
//! install already puts on disk, so the install path grows nothing. The host's
//! shape translation (the adapter's [`Codec`]) runs here, on the client side;
//! the wire carries the host's raw tool spelling and the runtime derives the
//! rest itself, so nothing this client says about a call is trusted.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use appa_runtime_api::{AdapterName, Codec, HookDecision, HookEvent, ParseRefusal, WireDecision, WireEvent};

use crate::loopback_http::{Answer, Deadline, Endpoint, request};
use crate::runtime_start::{self, Deployment};
use crate::runtime_url::RuntimeTarget;

/// What the answer to this hook decides. A turn end reports a turn the actor has
/// already finished, so it decides nothing: it discards the answer and never
/// blocks, because every blocking outcome there would hold the actor in that turn.
/// Every other hook authorizes something, so a runtime that does not answer blocks
/// rather than letting the action through.
///
/// The caller says which, rather than this client reading the event name: the hook
/// map already registers each hook for one event and is where that choice is
/// reviewed, and nothing in the posted event can move a hook off it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Decides {
    Authorization,
    Nothing,
}

impl Decides {
    fn of_a_turn_end(turn_end: bool) -> Self {
        match turn_end {
            true => Self::Nothing,
            false => Self::Authorization,
        }
    }

    /// The budget for the whole round trip. Authorization outlasts the runtime's
    /// own evidence round trips; a turn end waits on none, so it takes the shorter
    /// one and costs a runtime that is down a call left open, never a stuck turn.
    /// Both stay under the timeout the harness declares, which is the deadline that
    /// matters: a hook the harness kills has its exit code ignored and fails open.
    fn budget(self) -> Duration {
        match self {
            Self::Authorization => AUTHORIZATION_BUDGET,
            Self::Nothing => TURN_END_BUDGET,
        }
    }
}

fn post(endpoint: &Endpoint, event: &[u8], deadline: &Deadline) -> Result<Answer, String> {
    request(endpoint, "POST", "/hook", event, deadline)
}

/// The blocking hook outcome. Claude Code reads stderr as the reason it blocked.
fn block(failure: &str) -> ExitCode {
    eprintln!("OpenAPPA hook blocked: {failure}");
    ExitCode::from(2)
}

/// Preserve the status while surfacing the runtime's own diagnostic. Claude Code displays
/// stderr for a blocking hook, not the response body this client forwards on stdout. A
/// refusal arrives either as a wire decision carrying its `detail` or, before any event
/// exists, as `{"error": …}`.
fn refusal(answer: &Answer) -> String {
    let json_detail = serde_json::from_slice::<serde_json::Value>(&answer.body)
        .ok()
        .and_then(|body| {
            ["error", "detail"]
                .into_iter()
                .find_map(|field| body.get(field).and_then(serde_json::Value::as_str).map(str::to_owned))
        });
    let plain_error = std::str::from_utf8(&answer.body)
        .ok()
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(str::to_owned);
    match json_detail.or(plain_error) {
        Some(detail) => format!("status={} {detail}", answer.status),
        None => format!("status={}", answer.status),
    }
}

/// The host whose hook bytes this client translates. It is not a choice: the kagent plugin
/// posts the canonical wire itself, so this bridge is Claude Code's alone.
const HOST: AdapterName = AdapterName::ClaudeCode;

/// The round-trip budgets the hook entries' timeouts are declared above.
pub(crate) const AUTHORIZATION_BUDGET: Duration = Duration::from_secs(120);
pub(crate) const TURN_END_BUDGET: Duration = Duration::from_secs(30);

/// The runtime's answer, read off the wire. A body that is not a wire decision is
/// not guessed at: the hook fails closed on it.
fn decision_of(body: &[u8]) -> Result<HookDecision, String> {
    let wire: WireDecision = serde_json::from_slice(body)
        .map_err(|error| format!("the runtime's answer is not a wire decision: {error}"))?;
    wire.into_decision().map_err(|refusal| match refusal {
        ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail } => detail,
    })
}

/// Put one answer where the harness reads it. Whether a failed write matters is the
/// caller's to weigh: an answer the harness never received decided nothing, and a
/// withholding is the answer that carries its whole effect this way.
fn print(answer: &serde_json::Value) -> std::io::Result<()> {
    let mut stdout = std::io::stdout();
    stdout.write_all(answer.to_string().as_bytes())?;
    stdout.flush()
}

/// Hand the harness one answer and exit on it. An answer that could not be written is an
/// answer the harness never received, so it decided nothing: exiting zero would release the
/// action on a decision that never arrived, and the blocking exit is what says so instead.
fn deliver(answer: &serde_json::Value) -> ExitCode {
    match print(answer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => block(&format!("the answer could not be written: {error}")),
    }
}

/// The host event read as the typed event it reports: `None` for a hook the adapter does
/// not gate, whose answer is the empty opinion without a round trip.
fn parse_host_event(codec: &Codec, host_event: &[u8]) -> Result<Option<HookEvent>, String> {
    match (codec.parse)(host_event) {
        Ok(event) => Ok(event),
        Err(ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail }) => Err(detail),
    }
}

/// One parsed event on the canonical wire. Crossing is a step of its own because the event
/// outlives its failure: an event that cannot cross still reports what the host did, so a
/// result the tool already produced is withheld rather than left in front of the model.
fn wire_body(event: &HookEvent) -> Result<Vec<u8>, String> {
    let wire = WireEvent::from_event(HOST, event).map_err(|refusal| match refusal {
        ParseRefusal::Unreadable { detail } | ParseRefusal::Malformed { detail } => detail,
    })?;
    serde_json::to_vec(&wire).map_err(|error| format!("the wire event does not serialize: {error}"))
}

/// Whether this process runs inside a session APPA protects. Only a session launched
/// with `APPA_GATE=1` is: the variable is read from the process environment the harness
/// fixed at its own launch, so a session cannot turn the protection off. Outside one,
/// every client this binary runs as says nothing and exits 0.
pub(crate) fn session_is_gated() -> bool {
    std::env::var_os("APPA_GATE").is_some_and(|value| value == "1")
}

/// Post the hook event on stdin to `target`. With `ensure` the deployed runtime
/// is brought up first: the harness runs an event's hooks in parallel, so the
/// SessionStart entry that starts the runtime is the one that posts to it, and a
/// start that fails blocks like an unanswered hook rather than posting into
/// nothing.
pub fn run(target: &RuntimeTarget, turn_end: bool, ensure: Option<&Deployment>) -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let decides = Decides::of_a_turn_end(turn_end);
    let codec = appa_adapter_claude_code::codec();
    let mut host_event = Vec::new();
    if let Err(error) = std::io::stdin().read_to_end(&mut host_event) {
        return block(&format!("the hook event could not be read: {error}"));
    }
    if let Some(deployment) = ensure {
        let executable = match std::env::current_exe() {
            Ok(executable) => executable,
            Err(error) => {
                return block(&format!(
                    "this executable has no path to start the runtime from: {error}"
                ));
            }
        };
        if let Err(error) = runtime_start::ensure(target, deployment, &executable) {
            return block(&format!("the runtime could not be started: {error}"));
        }
    }
    let event = match parse_host_event(&codec, &host_event) {
        // A hook the adapter does not gate is the empty opinion, with no round trip.
        Ok(None) => {
            if decides == Decides::Authorization {
                return deliver(&serde_json::json!({}));
            }
            return ExitCode::SUCCESS;
        }
        Ok(Some(event)) => event,
        // Bytes this codec cannot read at all are still a hook: where they report a result
        // the harness has already produced, the codec renders the withholding for it, so
        // the output the tool produced does not stay in front of the model.
        Err(failure) => return unanswered(&codec, Unanswered::Unparsed(&host_event), &failure, decides),
    };
    // A prompt is refused while a subagent definition in reach declares `maxTurns`:
    // Claude Code ends such a subagent at its cap with no SubagentStop, so its
    // partial output would reach the parent unchecked.
    if let HookEvent::Prompt { .. } = &event
        && let Some(refusal) = crate::agent_scan::refusal()
    {
        eprintln!("{refusal}");
        return ExitCode::from(2);
    }
    // A parsed event that cannot cross the wire is still an event to answer: it is handed
    // to the withholding path rather than dropped, so a result that already ran is taken
    // out of the model's attention instead of staying in front of it.
    let body = match wire_body(&event) {
        Ok(body) => body,
        Err(failure) => return unanswered(&codec, Unanswered::Event(&event), &failure, decides),
    };
    let answered =
        Endpoint::parse(&target.url).and_then(|endpoint| post(&endpoint, &body, &Deadline::spanning(decides.budget())));
    let answer = match answered {
        Ok(answer) => answer,
        Err(failure) => return unanswered(&codec, Unanswered::Event(&event), &failure, decides),
    };
    if decides == Decides::Nothing {
        return ExitCode::SUCCESS;
    }
    match (decision_of(&answer.body), answer.is_success()) {
        (Ok(decision), true) => deliver(&(codec.render)(&event, &decision)),
        // A refusal is rendered too, and for a result that already ran the rendering
        // is the whole answer: the harness reads it only from a hook that exits zero,
        // so a replacement carried out on a blocking exit would be discarded and the
        // withheld output would stay in front of the model. That holds whatever the
        // refusal carries — a decision, a refusal that decides nothing, or a body that
        // is no wire decision at all. Every other refusal is what the exit code stops,
        // and the rendering only reports it.
        (answered, false) => {
            let failure = refusal(&answer);
            carry_out(&codec, &event, refused(&event, answered.ok(), &failure), &failure)
        }
        (Err(failure), true) => unanswered(&codec, Unanswered::Event(&event), &failure, decides),
    }
}

/// What one non-2xx answer leaves this client to do.
///
/// `Withheld` carries the replacement that stands in for a result the tool already
/// produced: printing it is the whole answer, because the harness applies a replacement
/// only from a hook that exits zero. `Stopped` carries the answer's own rendering, where
/// it has one, for a call that has not run and that the non-zero exit stops.
#[derive(Debug, PartialEq)]
enum Refused {
    Withheld(HookDecision),
    Stopped(Option<HookDecision>),
}

/// Read a refusal as what it does to this event. An event reporting a result is answered
/// by the replacement the runtime sent, or — where the refusal carries none, because it
/// decided nothing (an operational [`HookDecision::Refuse`]) or was no wire decision at
/// all — by a withholding synthesized from the refusal itself. Nothing has run yet at any
/// other hook, so there the exit code is what stops the call.
fn refused(event: &HookEvent, answered: Option<HookDecision>, failure: &str) -> Refused {
    match reports_a_result(event) {
        false => Refused::Stopped(answered),
        true => Refused::Withheld(match answered {
            Some(decision) if stands_in_for_a_result(&decision) => decision,
            _ => HookDecision::Block {
                reason: format!("the runtime refused this hook: {failure}"),
            },
        }),
    }
}

fn carry_out(codec: &Codec, event: &HookEvent, refused: Refused, failure: &str) -> ExitCode {
    match refused {
        Refused::Withheld(withholding) => withhold(&(codec.render)(event, &withholding), failure),
        // The exit code stops the call either way; where the answer's own rendering could
        // not be delivered, the failure to write it is surfaced beside the refusal rather
        // than swallowed.
        Refused::Stopped(answered) => match answered.map(|decision| print(&(codec.render)(event, &decision))) {
            Some(Err(error)) => block(&format!("{failure}; the answer could not be written: {error}")),
            _ => block(failure),
        },
    }
}

/// Take a result the tool already produced out of the model's attention, by printing the
/// replacement that stands in for it and exiting zero — the exit code the harness reads a
/// replacement from.
///
/// A replacement that could not be written is no withholding: the result the tool produced
/// is still in front of the model, and exiting zero would claim a replacement the harness
/// never received. The blocking exit is what is left to say so — it surfaces the failure on
/// stderr and stops the turn rather than reporting a withholding that did not happen.
fn withhold(withholding: &serde_json::Value, failure: &str) -> ExitCode {
    eprintln!("OpenAPPA hook withheld the result: {failure}");
    match print(withholding) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => block(&format!("{failure}; the withholding could not be written: {error}")),
    }
}

/// What one hook the runtime did not answer leaves this client holding: the event the host
/// bytes reported, or the bytes themselves where this codec could not read them. Only the
/// codec can tell what a host's own bytes report, so the unread ones are handed back to it
/// rather than sniffed here.
enum Unanswered<'a> {
    Event(&'a HookEvent),
    Unparsed(&'a [u8]),
}

/// Fail closed on a hook the runtime did not answer. A turn end decides nothing and
/// never blocks. Otherwise the exit code stops a call the harness has not run yet,
/// but a result the tool already produced stays in front of the model unless the
/// withholding is rendered for it — and the harness reads a rendered answer only
/// from a hook that exits zero, so a hook reporting a result is withheld by the
/// rendering and exits zero, while everything else blocks by the exit code alone.
fn unanswered(codec: &Codec, hook: Unanswered<'_>, failure: &str, decides: Decides) -> ExitCode {
    if decides == Decides::Nothing {
        eprintln!("OpenAPPA runtime did not answer the turn end: {failure}");
        return ExitCode::SUCCESS;
    }
    match withholding(codec, hook, failure) {
        Some(withholding) => withhold(&withholding, failure),
        None => block(failure),
    }
}

/// The withholding one unanswered hook needs, where it reports a result the tool already
/// produced: rendered from the event the host bytes reported, or — where they never parsed
/// — by the codec, which reads the host's own shape for a result the harness has produced.
fn withholding(codec: &Codec, hook: Unanswered<'_>, failure: &str) -> Option<serde_json::Value> {
    match hook {
        Unanswered::Event(event) => reports_a_result(event).then(|| {
            (codec.render)(
                event,
                &HookDecision::Block {
                    reason: format!("the runtime did not answer this hook: {failure}"),
                },
            )
        }),
        Unanswered::Unparsed(host_event) => {
            (codec.withholding)(host_event, &format!("this hook could not be read: {failure}"))
        }
    }
}

/// Whether this event reports a result the tool already produced. Such a result is only
/// ever taken out of the model's attention by the replacement that stands in for it, and
/// the harness reads that replacement from a hook that exits zero — so an answer to one of
/// these events takes effect through what the client prints, not through its exit code.
fn reports_a_result(event: &HookEvent) -> bool {
    matches!(
        event,
        HookEvent::ToolResult { .. } | HookEvent::SpawnResult { .. } | HookEvent::ChildEnd { .. }
    )
}

/// Whether this decision is one the harness can put in a result's place.
fn stands_in_for_a_result(decision: &HookDecision) -> bool {
    matches!(
        decision,
        HookDecision::Block { .. }
            | HookDecision::ReplaceOutput { .. }
            | HookDecision::DeliverValue { .. }
            | HookDecision::ChildReturn { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_end_waits_on_less_than_an_authorization_does() {
        assert!(Decides::of_a_turn_end(true) == Decides::Nothing);
        assert!(Decides::of_a_turn_end(false) == Decides::Authorization);
        assert!(Decides::Nothing.budget() < Decides::Authorization.budget());
    }

    #[test]
    fn a_refusal_surfaces_the_runtimes_exact_error() {
        let answer = Answer {
            status: 409,
            body: serde_json::to_vec(&serde_json::json!({
                "error": "annotator=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
            }))
            .expect("the fixture serializes"),
        };
        assert_eq!(
            refusal(&answer),
            "status=409 annotator=claude-code error=malformed field=delta.audience value=\"secret\" allowed=declaration.audiences"
        );

        let refused = Answer {
            status: 409,
            body: serde_json::to_vec(&WireDecision::of(&HookDecision::Refuse {
                detail: "storage failure: disk full".to_string(),
            }))
            .expect("the fixture serializes"),
        };
        assert_eq!(refusal(&refused), "status=409 storage failure: disk full");

        assert_eq!(
            refusal(&Answer {
                status: 503,
                body: b"temporarily unavailable".to_vec(),
            }),
            "status=503 temporarily unavailable"
        );
        assert_eq!(
            refusal(&Answer {
                status: 500,
                body: Vec::new(),
            }),
            "status=500"
        );
    }

    /// A hook this codec cannot read at all is still answered where its bytes report a
    /// result the tool produced: the codec renders the withholding for it, and every other
    /// unreadable hook has nothing to print and is stopped by the exit code alone.
    #[test]
    fn an_unreadable_hook_is_withheld_only_where_its_bytes_report_a_result() {
        let codec = appa_adapter_claude_code::codec();
        // A post-use hook with no `tool_input`: the tool has run, and the parse refuses.
        let ran = br#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Bash",
            "tool_response":{"stdout":"root:x:0:0"}}"#;
        assert!(parse_host_event(&codec, ran).is_err());
        let withheld =
            withholding(&codec, Unanswered::Unparsed(ran), "malformed").expect("a hook reporting a result is withheld");
        assert!(
            !withheld["hookSpecificOutput"]["updatedToolOutput"].is_null(),
            "the result is replaced, not left in front of the model: {withheld}"
        );
        assert!(
            !withheld.to_string().contains("root:x:0:0"),
            "the withheld output does not reach the model: {withheld}"
        );

        let proposed = br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash"}"#;
        assert!(parse_host_event(&codec, proposed).is_err());
        assert_eq!(
            withholding(&codec, Unanswered::Unparsed(proposed), "malformed"),
            None,
            "a call that has not run has no result to withhold"
        );
        assert_eq!(
            withholding(&codec, Unanswered::Unparsed(b"not json"), "unreadable"),
            None
        );
    }

    #[test]
    fn a_host_event_crosses_as_a_wire_event_with_its_raw_spelling() {
        let codec = appa_adapter_claude_code::codec();
        let host =
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Agent","tool_input":{"prompt":"go"}}"#;
        let event = parse_host_event(&codec, host)
            .expect("the event parses")
            .expect("PreToolUse is gated");
        assert!(matches!(event, HookEvent::ToolCall { .. }));
        let body = wire_body(&event).expect("the event crosses");
        let wire: serde_json::Value = serde_json::from_slice(&body).expect("the wire is JSON");
        assert_eq!(wire["protocol"], appa_runtime_api::PROTOCOL);
        assert_eq!(wire["adapter"], "claude-code");
        assert_eq!(wire["event"], "tool_call");
        assert_eq!(wire["root_id"], "s1");
        assert_eq!(wire["tool"], "Agent", "the wire carries the host's raw spelling");
        assert_eq!(wire["arguments"], serde_json::json!({"prompt": "go"}));
        assert!(
            wire.get("spawn").is_none(),
            "the client claims nothing the runtime derives: {wire}"
        );

        let ungated = br#"{"hook_event_name":"Notification","session_id":"s1"}"#;
        assert!(
            parse_host_event(&codec, ungated)
                .expect("an ungated hook parses")
                .is_none()
        );
        assert!(parse_host_event(&codec, b"not json").is_err());
        assert!(
            parse_host_event(&codec, br#"{"hook_event_name":"PreToolUse","session_id":"s1"}"#).is_err(),
            "a malformed host event blocks before any round trip"
        );
    }

    /// An event the host spells well enough to parse, but that carries no id the wire can
    /// take, is still an event: parsing keeps it, and the failure to cross is answered with
    /// the event in hand rather than with nothing.
    #[test]
    fn an_event_that_cannot_cross_the_wire_survives_its_failure() {
        let codec = appa_adapter_claude_code::codec();
        let host = br#"{"hook_event_name":"PostToolUse","session_id":"","tool_name":"Bash",
            "tool_input":{"command":"ls"},"tool_response":{"stdout":"root:x:0:0"}}"#;
        let event = parse_host_event(&codec, host)
            .expect("an empty session id still parses")
            .expect("PostToolUse is gated");
        assert!(matches!(event, HookEvent::ToolResult { .. }));
        assert!(
            wire_body(&event).is_err(),
            "an empty root id names no trajectory the wire can carry"
        );
        assert!(reports_a_result(&event));
    }

    /// Every non-2xx answer to an event that reports a result ends in a rendered
    /// withholding and a zero exit, whatever the refusal carries: the tool has already run,
    /// and the harness applies a replacement only from a hook that exits zero. A refusal
    /// that decides nothing (the 409 `refuse` an operational failure answers with) and a
    /// body that is no wire decision at all both carry none, so the withholding is
    /// synthesized — and neither rendering carries the output that was withheld.
    #[test]
    fn a_refusal_to_a_result_that_already_ran_withholds_it_rather_than_blocking() {
        let codec = appa_adapter_claude_code::codec();
        let host = br#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Bash",
            "tool_input":{"command":"cat /etc/passwd"},"tool_response":{"stdout":"root:x:0:0"},"tool_use_id":"t1"}"#;
        let event = parse_host_event(&codec, host)
            .expect("the event parses")
            .expect("PostToolUse is gated");
        assert!(matches!(event, HookEvent::ToolResult { .. }));

        let operational = HookDecision::Refuse {
            detail: "storage failure: disk full".to_string(),
        };
        for answered in [Some(operational), None] {
            let Refused::Withheld(withholding) = refused(&event, answered.clone(), "status=409 storage failure") else {
                panic!("a result that already ran is withheld, not blocked: {answered:?}");
            };
            assert!(
                matches!(withholding, HookDecision::Block { .. }),
                "a refusal carrying no replacement synthesizes one: {withholding:?}"
            );
            let rendered = (codec.render)(&event, &withholding).to_string();
            assert!(
                !rendered.contains("root:x:0:0"),
                "the withheld output does not reach the model: {rendered}"
            );
        }
    }

    /// A refusal before the call runs is stopped by the exit code, and the answer's own
    /// rendering only reports it.
    #[test]
    fn a_refusal_before_a_call_runs_stays_a_block() {
        let codec = appa_adapter_claude_code::codec();
        let host =
            br#"{"hook_event_name":"PreToolUse","session_id":"s1","tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        let event = parse_host_event(&codec, host)
            .expect("the event parses")
            .expect("PreToolUse is gated");
        let refuse = HookDecision::Refuse {
            detail: "storage failure: disk full".to_string(),
        };
        assert_eq!(
            refused(&event, Some(refuse.clone()), "status=409"),
            Refused::Stopped(Some(refuse))
        );
        assert_eq!(refused(&event, None, "status=500"), Refused::Stopped(None));
    }

    #[test]
    fn an_answer_is_read_as_a_wire_decision_or_fails_closed() {
        let allow =
            serde_json::to_vec(&WireDecision::of(&HookDecision::AllowCall { spawn: None })).expect("serializes");
        assert_eq!(
            decision_of(&allow).expect("a wire decision reads"),
            HookDecision::AllowCall { spawn: None }
        );
        assert!(decision_of(b"{}").is_err(), "an empty object is no decision");
        assert!(decision_of(br#"{"protocol":9,"decision":"ack"}"#).is_err());
        assert!(decision_of(br#"{"protocol":1,"decision":"block"}"#).is_err());
    }
}
