//! `appa replay`: trace files down the hook path.
//!
//! One file is one trace: a root trajectory's tool calls in order, each with the decision it
//! must get. The runner builds the typed events itself and hands them to the hook dispatcher,
//! so every step is judged by the production session, engine boundary, and engine over a log
//! that lives in memory for the run. No tool runs: after an allowed call the runner reports an
//! empty output, which is the moment the contract's `delta` lands on the trajectory label, so
//! the next step sees the narrowed trajectory.
//!
//! A step expects one of four things. `allow`: the call runs, or the only thing in the way is
//! the plain narrowing acceptance, which the runner takes as the model would. `authority`: the
//! call is blocked and the offer names an authority; the runner takes it, and the runtime's
//! stand-in approves. `sanitizer`: the call is blocked and the offer names a sanitizer; the
//! runner takes it, and the stand-in returns the value unchanged. `deny`: the call is blocked
//! and nothing is offered. Taking an offer goes through the runtime's own remedy path, so the
//! records read as if the party had answered, and the trace continues from there.
//!
//! Files run at once, each in its own trajectory. Steps inside a file run in order. A step
//! that mismatches or cannot run ends its file; the other files keep running. The exit code
//! is 0 when every step passed, 1 when any step mismatched, and 2 when any step could not run
//! or a file or the configuration was refused.

use std::collections::BTreeSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use appa_runtime_api::{Actor, HookDecision, HookEvent, OutcomeBody, ProposedCall, ToolOutcome, TrajectoryId};
use serde_json::value::RawValue;

use crate::api::{OfferId, OfferKind, RemedyOutcome, Runtime, is_control_tool};
use crate::config::Config;
use crate::hooks;

/// The decision a step must get. `Authority` and `Sanitizer` may name the party the offer
/// must involve; unnamed, any party of that kind passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Allow,
    Deny,
    Authority(Option<String>),
    Sanitizer(Option<String>),
}

impl fmt::Display for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expect::Allow => f.write_str("allow"),
            Expect::Deny => f.write_str("deny"),
            Expect::Authority(None) => f.write_str("authority"),
            Expect::Authority(Some(name)) => write!(f, "authority {name}"),
            Expect::Sanitizer(None) => f.write_str("sanitizer"),
            Expect::Sanitizer(Some(name)) => write!(f, "sanitizer {name}"),
        }
    }
}

impl Expect {
    /// Whether a blocked call's offer is the one this expectation takes.
    fn takes(&self, kind: &OfferKind) -> bool {
        match (self, kind) {
            (Expect::Allow, OfferKind::Accept) => true,
            (Expect::Authority(None), OfferKind::Authority { .. }) => true,
            (Expect::Authority(Some(name)), OfferKind::Authority { names }) => names.contains(name),
            (Expect::Sanitizer(None), OfferKind::Sanitizer { .. }) => true,
            (Expect::Sanitizer(Some(name)), OfferKind::Sanitizer { name: offered }) => name == offered,
            _ => false,
        }
    }
}

/// One proposed call and the decision it must get. The arguments are the JSON object the
/// block spells, values byte-for-byte as written: the engine canonicalizes them, the runner
/// never does.
#[derive(Debug, Clone)]
pub struct Step {
    pub line: usize,
    pub tool: String,
    pub arguments: Box<RawValue>,
    pub expect: Expect,
}

/// One trace file: the steps of one root trajectory, in order.
#[derive(Debug, Clone)]
pub struct Trace {
    pub path: PathBuf,
    pub steps: Vec<Step>,
}

impl Trace {
    fn root(&self) -> TrajectoryId {
        TrajectoryId(format!("replay:{}", self.path.display()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{}:{line}: {detail}", path.display())]
pub struct SyntaxError {
    pub path: PathBuf,
    pub line: usize,
    pub detail: String,
}

/// Parse one trace file. The grammar is strict and line-oriented: a step is `Tool {`, one
/// `key: <JSON value>` per line, `}`, then `expect allow`, `expect deny`,
/// `expect authority [name]`, or `expect sanitizer [name]`. Blank lines and lines whose first
/// character is `#` are skipped anywhere.
pub fn parse(path: &Path, text: &str) -> Result<Trace, SyntaxError> {
    let mut parser = Parser {
        path,
        steps: Vec::new(),
        state: State::Outside,
    };
    for (index, line) in text.lines().enumerate() {
        parser.line(index + 1, line)?;
    }
    parser.finish()
}

struct Parser<'a> {
    path: &'a Path,
    steps: Vec<Step>,
    state: State,
}

enum State {
    Outside,
    InBlock {
        line: usize,
        tool: String,
        fields: Vec<(String, String)>,
    },
    AfterBlock {
        line: usize,
        tool: String,
        arguments: Box<RawValue>,
    },
}

const EXPECT_WORDS: &str = "`expect allow`, `expect deny`, `expect authority [name]`, or `expect sanitizer [name]`";

impl Parser<'_> {
    fn error(&self, line: usize, detail: impl Into<String>) -> SyntaxError {
        SyntaxError {
            path: self.path.to_path_buf(),
            line,
            detail: detail.into(),
        }
    }

    fn line(&mut self, number: usize, raw: &str) -> Result<(), SyntaxError> {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(());
        }
        match std::mem::replace(&mut self.state, State::Outside) {
            State::Outside => self.open(number, line),
            State::InBlock { line: at, tool, fields } => self.field(number, line, at, tool, fields),
            State::AfterBlock {
                line: at,
                tool,
                arguments,
            } => self.expect(number, line, at, tool, arguments),
        }
    }

    fn open(&mut self, number: usize, line: &str) -> Result<(), SyntaxError> {
        let Some((name, rest)) = line.split_once('{') else {
            return Err(self.error(number, format!("expected a tool call like `Tool {{`, found `{line}`")));
        };
        let tool = name.trim();
        if !is_identifier(tool) {
            return Err(self.error(number, format!("`{tool}` is not a tool name")));
        }
        if is_control_tool(tool) {
            return Err(self.error(
                number,
                format!("`{tool}` is the remedy tool; a trace holds only the calls the model proposes"),
            ));
        }
        self.state = match rest.trim() {
            "" => State::InBlock {
                line: number,
                tool: tool.to_string(),
                fields: Vec::new(),
            },
            "}" => State::AfterBlock {
                line: number,
                tool: tool.to_string(),
                arguments: arguments_of(&[]),
            },
            _ => return Err(self.error(number, "each argument goes on its own line after `{`")),
        };
        Ok(())
    }

    fn field(
        &mut self,
        number: usize,
        line: &str,
        at: usize,
        tool: String,
        mut fields: Vec<(String, String)>,
    ) -> Result<(), SyntaxError> {
        if line == "}" {
            self.state = State::AfterBlock {
                line: at,
                tool,
                arguments: arguments_of(&fields),
            };
            return Ok(());
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(self.error(number, format!("expected `key: value` or `}}`, found `{line}`")));
        };
        let key = key.trim();
        if !is_identifier(key) {
            return Err(self.error(number, format!("`{key}` is not an argument name")));
        }
        if fields.iter().any(|(name, _)| name == key) {
            return Err(self.error(number, format!("argument `{key}` is repeated")));
        }
        let value = value.trim();
        if let Err(error) = serde_json::from_str::<Box<RawValue>>(value) {
            return Err(self.error(number, format!("argument `{key}` is not one JSON value: {error}")));
        }
        fields.push((key.to_string(), value.to_string()));
        self.state = State::InBlock { line: at, tool, fields };
        Ok(())
    }

    fn expect(
        &mut self,
        number: usize,
        line: &str,
        at: usize,
        tool: String,
        arguments: Box<RawValue>,
    ) -> Result<(), SyntaxError> {
        let expect = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["expect", "allow"] => Expect::Allow,
            ["expect", "deny"] => Expect::Deny,
            ["expect", "authority"] => Expect::Authority(None),
            ["expect", "authority", name] if is_identifier(name) => Expect::Authority(Some(name.to_string())),
            ["expect", "sanitizer"] => Expect::Sanitizer(None),
            ["expect", "sanitizer", name] if is_identifier(name) => Expect::Sanitizer(Some(name.to_string())),
            _ => {
                return Err(self.error(
                    number,
                    format!("expected {EXPECT_WORDS} after the call at line {at}, found `{line}`"),
                ));
            }
        };
        self.steps.push(Step {
            line: at,
            tool,
            arguments,
            expect,
        });
        Ok(())
    }

    fn finish(self) -> Result<Trace, SyntaxError> {
        match &self.state {
            State::Outside => Ok(Trace {
                path: self.path.to_path_buf(),
                steps: self.steps,
            }),
            State::InBlock { line, tool, .. } => {
                Err(self.error(*line, format!("the `{tool}` call is not closed with `}}`")))
            }
            State::AfterBlock { line, tool, .. } => {
                Err(self.error(*line, format!("the `{tool}` call has no {EXPECT_WORDS}")))
            }
        }
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
}

/// The argument object, keys in file order, values verbatim. Every value was checked to be one
/// JSON value and every key is an identifier, so the assembled text is a JSON object.
fn arguments_of(fields: &[(String, String)]) -> Box<RawValue> {
    let body = fields
        .iter()
        .map(|(key, value)| format!("\"{key}\":{value}"))
        .collect::<Vec<_>>()
        .join(",");
    RawValue::from_string(format!("{{{body}}}")).expect("checked keys and values assemble into a JSON object")
}

/// What the runtime answered a step's call with: released, or blocked with these kinds of
/// offer (none for a block nothing can lift).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Got {
    Allowed,
    Blocked(BTreeSet<OfferKind>),
}

impl fmt::Display for Got {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Got::Allowed => f.write_str("allow"),
            Got::Blocked(kinds) if kinds.is_empty() => f.write_str("deny"),
            Got::Blocked(kinds) => {
                let words: Vec<String> = kinds
                    .iter()
                    .map(|kind| match kind {
                        OfferKind::Accept => "allow".to_string(),
                        OfferKind::Authority { names } => format!("authority {}", names.join("+")),
                        OfferKind::Sanitizer { name } => format!("sanitizer {name}"),
                    })
                    .collect();
                f.write_str(&words.join("|"))
            }
        }
    }
}

/// What one step got. `taken` names the offer the runner took to get the call released, as
/// the model would have: none for a call released as proposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Passed {
        taken: Option<OfferKind>,
    },
    Mismatch {
        got: Got,
        want: Expect,
        feedback: Option<String>,
    },
    CannotRun(String),
}

impl StepOutcome {
    fn passed(&self) -> bool {
        matches!(self, StepOutcome::Passed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepReport {
    pub line: usize,
    pub tool: String,
    pub expect: Expect,
    pub outcome: StepOutcome,
}

/// What one file got: every step up to and including the one that ended it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceReport {
    pub path: PathBuf,
    /// The trajectory could not open, so no step ran.
    pub unopened: Option<String>,
    pub steps: Vec<StepReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Failed,
    CannotRun,
}

impl TraceReport {
    pub fn verdict(&self) -> Verdict {
        if self.unopened.is_some() {
            return Verdict::CannotRun;
        }
        match self.steps.last().map(|step| &step.outcome) {
            Some(StepOutcome::CannotRun(_)) => Verdict::CannotRun,
            Some(StepOutcome::Mismatch { .. }) => Verdict::Failed,
            Some(StepOutcome::Passed { .. }) | None => Verdict::Ok,
        }
    }
}

/// Run every trace at once, each in its own trajectory, and answer in the order given.
pub async fn run(runtime: &Runtime, traces: &[Trace]) -> Vec<TraceReport> {
    futures_util::future::join_all(traces.iter().map(|trace| run_trace(runtime, trace))).await
}

async fn run_trace(runtime: &Runtime, trace: &Trace) -> TraceReport {
    let root = trace.root();
    let actor = Actor {
        root: root.clone(),
        child: None,
    };
    let mut report = TraceReport {
        path: trace.path.clone(),
        unopened: None,
        steps: Vec::new(),
    };
    match hooks::handle(runtime, HookEvent::SessionStart { root }).await {
        HookDecision::Ack => {}
        HookDecision::Refuse { detail } => {
            report.unopened = Some(detail);
            return report;
        }
        other => {
            report.unopened = Some(format!("the session start answered {other:?}"));
            return report;
        }
    }
    for step in &trace.steps {
        let outcome = run_step(runtime, &actor, step).await;
        let ended = !outcome.passed();
        report.steps.push(StepReport {
            line: step.line,
            tool: step.tool.clone(),
            expect: step.expect.clone(),
            outcome,
        });
        if ended {
            break;
        }
    }
    report
}

async fn run_step(runtime: &Runtime, actor: &Actor, step: &Step) -> StepOutcome {
    let call = ProposedCall {
        tool: step.tool.clone(),
        arguments: step.arguments.clone(),
        cwd: None,
    };
    let (got, feedback) = match propose(runtime, actor, call).await {
        Proposed::Allowed(call) => match report_empty_output(runtime, actor, call).await {
            Ok(()) => (Got::Allowed, None),
            Err(detail) => return StepOutcome::CannotRun(detail),
        },
        Proposed::Denied { feedback } => {
            let offers = offers_in(runtime, actor, &feedback);
            let kinds: BTreeSet<OfferKind> = offers.into_iter().map(|(kind, _)| kind).collect();
            (Got::Blocked(kinds), Some(feedback))
        }
        Proposed::CannotRun(detail) => return StepOutcome::CannotRun(detail),
    };
    match (&got, &step.expect) {
        (Got::Allowed, Expect::Allow) => StepOutcome::Passed { taken: None },
        (Got::Blocked(kinds), Expect::Deny) if kinds.is_empty() => StepOutcome::Passed { taken: None },
        (Got::Blocked(kinds), expect) if kinds.iter().any(|kind| expect.takes(kind)) => {
            let feedback = feedback.as_deref().expect("a block carries its feedback");
            let (kind, offer) = offers_in(runtime, actor, feedback)
                .into_iter()
                .find(|(offered, _)| expect.takes(offered))
                .expect("the kind was read from these offers");
            match take_offer(runtime, actor, offer).await {
                Ok(()) => StepOutcome::Passed { taken: Some(kind) },
                Err(detail) => StepOutcome::CannotRun(detail),
            }
        }
        _ => StepOutcome::Mismatch {
            got,
            want: step.expect.clone(),
            feedback,
        },
    }
}

enum Proposed {
    Allowed(ProposedCall),
    Denied { feedback: String },
    CannotRun(String),
}

async fn propose(runtime: &Runtime, actor: &Actor, call: ProposedCall) -> Proposed {
    let event = HookEvent::ToolCall {
        actor: actor.clone(),
        call: call.clone(),
        spawn: false,
        ruling: None,
    };
    match hooks::handle(runtime, event).await {
        HookDecision::AllowCall { .. } => Proposed::Allowed(call),
        HookDecision::DenyCall { feedback, .. } => Proposed::Denied { feedback },
        HookDecision::Refuse { detail } => Proposed::CannotRun(detail),
        other => Proposed::CannotRun(format!("the call answered {other:?}")),
    }
}

/// No tool runs. An empty successful output closes the call; this is when the contract's
/// `delta` lands on the trajectory label.
async fn report_empty_output(runtime: &Runtime, actor: &Actor, call: ProposedCall) -> Result<(), String> {
    let event = HookEvent::ToolResult {
        actor: actor.clone(),
        call,
        outcome: ToolOutcome::Success {
            body: OutcomeBody::Available(String::new()),
        },
    };
    match hooks::handle(runtime, event).await {
        HookDecision::Ack | HookDecision::ReplaceOutput { .. } => Ok(()),
        HookDecision::Refuse { detail } => Err(detail),
        HookDecision::Block { reason } => Err(reason),
        other => Err(format!("the result answered {other:?}")),
    }
}

/// The offers a block's feedback names, each with whom taking it involves. An id the
/// runtime no longer recognizes is dropped.
fn offers_in(runtime: &Runtime, actor: &Actor, feedback: &str) -> Vec<(OfferKind, OfferId)> {
    feedback
        .lines()
        .filter_map(|line| {
            let after = line.split("offer_id:").nth(1)?;
            let rest = after.trim_start().strip_prefix('"')?;
            Some(OfferId(rest[..rest.find('"')?].to_string()))
        })
        .filter_map(|offer| runtime.offer_kind(&actor.root, &offer).map(|kind| (kind, offer)))
        .collect()
}

/// Take an offer the way the model does — the remedy tool, then the call proposed again
/// exactly as authorized or substituted — and close the call with an empty output.
async fn take_offer(runtime: &Runtime, actor: &Actor, offer: OfferId) -> Result<(), String> {
    let call = match runtime.execute_remedy(actor, offer).await {
        RemedyOutcome::Authorized { call } | RemedyOutcome::Substituted { call } => call,
        RemedyOutcome::Returned { .. } => return Ok(()),
        RemedyOutcome::Declined { feedback } | RemedyOutcome::NoAnswer { feedback } => {
            return Err(format!("taking the offer did not release the call: {feedback}"));
        }
        RemedyOutcome::Refused { detail } => return Err(detail),
    };
    match propose(runtime, actor, call).await {
        Proposed::Allowed(call) => report_empty_output(runtime, actor, call).await,
        Proposed::Denied { feedback } => Err(format!("the released call was proposed again and denied: {feedback}")),
        Proposed::CannotRun(detail) => Err(detail),
    }
}

/// The counts one run ends with, and the exit code they select.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub ok: usize,
    pub failed: usize,
    pub cannot_run: usize,
}

impl Summary {
    pub fn exit_code(self) -> ExitCode {
        if self.cannot_run > 0 {
            ExitCode::from(2)
        } else if self.failed > 0 {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let files = self.ok + self.failed + self.cannot_run;
        write!(
            f,
            "{files} {}: {} ok, {} failed, {} could not run",
            if files == 1 { "file" } else { "files" },
            self.ok,
            self.failed,
            self.cannot_run
        )
    }
}

fn taken_note(taken: Option<&OfferKind>) -> String {
    match taken {
        None => String::new(),
        Some(OfferKind::Accept) => " (after accepting the narrowing)".to_string(),
        Some(OfferKind::Authority { names }) => format!(" (after {} approved)", names.join(" and ")),
        Some(OfferKind::Sanitizer { name }) => format!(" (after {name} rewrote it)"),
    }
}

/// Print the reports the way `go test` does: nothing for a step that passed unless
/// `verbose`, one line per step that did not, `ok` or `FAIL` per file, then the summary.
pub fn render(reports: &[TraceReport], verbose: bool, out: &mut impl Write) -> std::io::Result<Summary> {
    let mut summary = Summary::default();
    for report in reports {
        let path = report.path.display();
        if let Some(detail) = &report.unopened {
            writeln!(out, "{path}: cannot run: {detail}")?;
        }
        for step in &report.steps {
            let line = step.line;
            let tool = &step.tool;
            match &step.outcome {
                StepOutcome::Passed { taken } => {
                    if verbose {
                        writeln!(
                            out,
                            "ok    {path}:{line} {tool} {}{}",
                            step.expect,
                            taken_note(taken.as_ref())
                        )?;
                    }
                }
                StepOutcome::Mismatch { got, want, feedback } => {
                    writeln!(out, "{path}:{line}: {tool}: got {got}, want {want}")?;
                    for text in feedback.iter().flat_map(|feedback| feedback.lines()) {
                        writeln!(out, "    {text}")?;
                    }
                }
                StepOutcome::CannotRun(detail) => {
                    writeln!(out, "{path}:{line}: {tool}: cannot run: {detail}")?;
                }
            }
        }
        match report.verdict() {
            Verdict::Ok => {
                summary.ok += 1;
                writeln!(out, "ok    {path}")?;
            }
            Verdict::Failed => {
                summary.failed += 1;
                writeln!(out, "FAIL  {path}")?;
            }
            Verdict::CannotRun => {
                summary.cannot_run += 1;
                writeln!(out, "FAIL  {path}")?;
            }
        }
    }
    writeln!(out, "{summary}")?;
    Ok(summary)
}

/// Every `.appa` file the paths name: a file as itself, a directory as its `.appa` files,
/// recursively and sorted. Each file once, in the order first named.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        let metadata = std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let found = if metadata.is_dir() {
            let mut found = Vec::new();
            walk(path, &mut found).map_err(|error| format!("{}: {error}", path.display()))?;
            found.sort();
            found
        } else {
            vec![path.clone()]
        };
        for file in found {
            if seen.insert(file.clone()) {
                files.push(file);
            }
        }
    }
    if files.is_empty() {
        return Err("no trace files: name a `.appa` file or a directory holding some".to_string());
    }
    Ok(files)
}

fn walk(dir: &Path, into: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, into)?;
        } else if path.extension().is_some_and(|extension| extension == "appa") {
            into.push(path);
        }
    }
    Ok(())
}

/// The `appa replay` command: parse every trace, open the deployment over an in-memory log,
/// run, print, and exit.
pub fn main(config: &Path, modules: Option<PathBuf>, verbose: bool, paths: &[PathBuf]) -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let files = match collect(paths) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("appa replay: {error}");
            return ExitCode::from(2);
        }
    };
    let mut traces = Vec::new();
    let mut refused = false;
    for file in &files {
        match std::fs::read_to_string(file) {
            Ok(text) => match parse(file, &text) {
                Ok(trace) => traces.push(trace),
                Err(error) => {
                    eprintln!("{error}");
                    refused = true;
                }
            },
            Err(error) => {
                eprintln!("{}: {error}", file.display());
                refused = true;
            }
        }
    }
    if refused {
        return ExitCode::from(2);
    }
    let config = match Config::load(config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("appa replay: {error}");
            return ExitCode::from(2);
        }
    };
    let runtime = match Runtime::open_in_memory(config, modules) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa replay: {error}");
            return ExitCode::from(2);
        }
    };
    let executor = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(executor) => executor,
        Err(error) => {
            eprintln!("appa replay: cannot create async runtime: {error}");
            return ExitCode::from(2);
        }
    };
    let reports = executor.block_on(run(&runtime, &traces));
    let mut stdout = std::io::stdout().lock();
    match render(&reports, verbose, &mut stdout) {
        Ok(summary) => summary.exit_code(),
        Err(error) => {
            eprintln!("appa replay: cannot write the report: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Result<Trace, SyntaxError> {
        parse(Path::new("t.appa"), text)
    }

    #[test]
    fn a_trace_parses_into_steps_with_verbatim_arguments() {
        let trace = parsed(
            "# a comment\n\nRead {\n  path: \"/etc/secrets\"\n}\nexpect allow\n\nEmail {\n  to: \"x@other.com\"\n  count: 2\n  tags: [\"a\", \"b\"]\n}\nexpect sanitizer redactor\n\nList {}\nexpect authority\n\nDrop {}\nexpect deny\n\nWipe {}\nexpect authority hitl\n",
        )
        .expect("the trace parses");
        assert_eq!(trace.steps.len(), 5);
        let read = &trace.steps[0];
        assert_eq!(
            (read.line, read.tool.as_str(), &read.expect),
            (3, "Read", &Expect::Allow)
        );
        assert_eq!(read.arguments.get(), r#"{"path":"/etc/secrets"}"#);
        let email = &trace.steps[1];
        assert_eq!(
            (email.line, email.tool.as_str(), &email.expect),
            (8, "Email", &Expect::Sanitizer(Some("redactor".into())))
        );
        assert_eq!(
            email.arguments.get(),
            r#"{"to":"x@other.com","count":2,"tags":["a", "b"]}"#
        );
        assert_eq!(
            (trace.steps[2].arguments.get(), &trace.steps[2].expect),
            ("{}", &Expect::Authority(None))
        );
        assert_eq!(trace.steps[3].expect, Expect::Deny);
        assert_eq!(trace.steps[4].expect, Expect::Authority(Some("hitl".into())));
    }

    #[test]
    fn every_syntax_error_names_its_line() {
        let cases = [
            (
                "Read {\n  path: \"a\"\n  path: \"b\"\n}\nexpect allow\n",
                3,
                "argument `path` is repeated",
            ),
            (
                "Read {\n  path: secret\n}\nexpect allow\n",
                2,
                "argument `path` is not one JSON value",
            ),
            ("Read {\n  path: \"a\"\n}\n", 1, "has no `expect allow`"),
            ("Read {\n  path: \"a\"\n}\nexpect maybe\n", 4, "expected `expect allow`"),
            ("Read {\n  path: \"a\"\n", 1, "is not closed with `}`"),
            ("expect allow\n", 1, "expected a tool call like `Tool {`"),
            (
                "Read {\n  path: \"a\"\n}\nexpect allow\nresult \"x\"\n",
                5,
                "expected a tool call like `Tool {`",
            ),
            (
                "Read { path: \"a\" }\nexpect allow\n",
                1,
                "each argument goes on its own line",
            ),
            ("execute_remedy_plan {}\nexpect allow\n", 1, "is the remedy tool"),
            ("9Read {}\nexpect allow\n", 1, "is not a tool name"),
        ];
        for (text, line, detail) in cases {
            let error = parsed(text).expect_err(text);
            assert_eq!(error.line, line, "{text}");
            assert!(error.detail.contains(detail), "{text}: {}", error.detail);
            assert!(error.to_string().starts_with(&format!("t.appa:{line}: ")));
        }
    }

    #[test]
    fn a_block_describes_what_it_offers() {
        assert_eq!(Got::Allowed.to_string(), "allow");
        assert_eq!(Got::Blocked(BTreeSet::new()).to_string(), "deny");
        assert_eq!(
            Got::Blocked(BTreeSet::from([
                OfferKind::Sanitizer {
                    name: "redactor".into()
                },
                OfferKind::Authority {
                    names: vec!["cto".into(), "hitl".into()]
                },
            ]))
            .to_string(),
            "authority cto+hitl|sanitizer redactor"
        );
        assert_eq!(Got::Blocked(BTreeSet::from([OfferKind::Accept])).to_string(), "allow");
    }

    #[test]
    fn the_summary_selects_the_exit_code() {
        let ok = Summary {
            ok: 2,
            failed: 0,
            cannot_run: 0,
        };
        assert_eq!(ok.exit_code(), ExitCode::SUCCESS);
        assert_eq!(ok.to_string(), "2 files: 2 ok, 0 failed, 0 could not run");
        let failed = Summary {
            ok: 1,
            failed: 1,
            cannot_run: 0,
        };
        assert_eq!(failed.exit_code(), ExitCode::from(1));
        let incomplete = Summary {
            ok: 0,
            failed: 1,
            cannot_run: 1,
        };
        assert_eq!(incomplete.exit_code(), ExitCode::from(2));
    }

    #[test]
    fn rendering_prints_failures_then_one_line_per_file() {
        let reports = vec![
            TraceReport {
                path: PathBuf::from("leak.appa"),
                unopened: None,
                steps: vec![
                    StepReport {
                        line: 3,
                        tool: "Read".into(),
                        expect: Expect::Allow,
                        outcome: StepOutcome::Passed {
                            taken: Some(OfferKind::Accept),
                        },
                    },
                    StepReport {
                        line: 8,
                        tool: "Email".into(),
                        expect: Expect::Deny,
                        outcome: StepOutcome::Mismatch {
                            got: Got::Blocked(BTreeSet::from([OfferKind::Sanitizer {
                                name: "redactor".into(),
                            }])),
                            want: Expect::Deny,
                            feedback: None,
                        },
                    },
                ],
            },
            TraceReport {
                path: PathBuf::from("push.appa"),
                unopened: None,
                steps: vec![StepReport {
                    line: 1,
                    tool: "Bash".into(),
                    expect: Expect::Authority(Some("hitl".into())),
                    outcome: StepOutcome::Passed {
                        taken: Some(OfferKind::Authority {
                            names: vec!["hitl".into()],
                        }),
                    },
                }],
            },
        ];
        let mut out = Vec::new();
        let summary = render(&reports, false, &mut out).expect("renders");
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            "leak.appa:8: Email: got sanitizer redactor, want deny\nFAIL  leak.appa\nok    push.appa\n2 files: 1 ok, 1 failed, 0 could not run\n"
        );
        assert_eq!(summary.exit_code(), ExitCode::from(1));

        let mut out = Vec::new();
        render(&reports, true, &mut out).expect("renders");
        let text = String::from_utf8(out).expect("utf-8");
        assert!(text.starts_with("ok    leak.appa:3 Read allow (after accepting the narrowing)\n"));
        assert!(text.contains("ok    push.appa:1 Bash authority hitl (after hitl approved)\n"));
    }
}
