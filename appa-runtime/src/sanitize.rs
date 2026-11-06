//! `appa runtime sanitize`: a policy's sanitizers asked about a list of values.
//!
//! Standard input is JSON lines, one value each: `id`, `sanitizer` (the name the policy
//! registers), `on` (`tool_output` or `tool_input`), `tool`, and `body`. At `tool_output`
//! the body is the result as a JSON string, and `tool` names its producer or is omitted for
//! a child's return. At `tool_input` the body is the call's arguments and `tool` names the
//! called tool. Every value is asked `--repeat` times through the production consult path —
//! the same declaration, prompt, backend, deadline, and byte cap a session uses — and every
//! answer is one JSON line on standard output, written as it arrives. No trajectory is
//! opened, so no tool runs and the engine's plan is not asked whether it would offer the
//! sanitizer on this value.
//!
//! An ask waits for its consult gate inside the consult's own deadline, so `--concurrency`
//! bounds how many values are in flight at once.

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::time::Instant;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::api::Runtime;
use crate::config::Config;
use crate::consult::SanitizerPoint;
use crate::external::ConsultOutcome;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Value {
    id: String,
    sanitizer: String,
    on: SanitizerPoint,
    #[serde(default)]
    tool: Option<String>,
    body: Box<RawValue>,
}

impl Value {
    /// The value as the engine holds it: a result's text, or a call's raw arguments.
    fn text(&self) -> Result<String, String> {
        match self.on {
            SanitizerPoint::ToolOutput => serde_json::from_str(self.body.get())
                .map_err(|_| "a tool_output body is the result as a JSON string".to_string()),
            SanitizerPoint::ToolInput => Ok(self.body.get().to_string()),
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Outcome {
    /// The derivation a session admits in place of the value.
    Derived { body: String },
    /// An answer a session does not take: not a sanitizer's answer, or a `tool_input`
    /// rewrite that is no call of its tool. The offer fails as it does without an answer.
    Rejected { answer: serde_json::Value },
    /// No answer, which fails the offer.
    NoAnswer { reason: String },
    /// The policy registers no such sanitizer at this point, or the value is not one it
    /// could be offered on.
    Unresolved { reason: String },
}

#[derive(Serialize)]
struct Answered<'a> {
    id: &'a str,
    repeat: u32,
    latency_ms: u128,
    #[serde(flatten)]
    outcome: Outcome,
}

async fn ask(runtime: &Runtime, value: &Value, repeat: u32) {
    let started = Instant::now();
    let outcome = match value.text() {
        Err(reason) => Outcome::Unresolved { reason },
        Ok(text) => match runtime
            .sanitize(&value.sanitizer, value.on, value.tool.as_deref(), &text)
            .await
        {
            Ok(consult) => match (consult.outcome, consult.derived) {
                (_, Some(body)) => Outcome::Derived { body },
                (ConsultOutcome::Answer(answer), None) => Outcome::Rejected { answer },
                (ConsultOutcome::NoAnswer(reason), None) => Outcome::NoAnswer {
                    reason: reason.diagnostic(),
                },
            },
            Err(error) => Outcome::Unresolved {
                reason: error.to_string(),
            },
        },
    };
    let line = serde_json::to_string(&Answered {
        id: &value.id,
        repeat,
        latency_ms: started.elapsed().as_millis(),
        outcome,
    })
    .expect("an answered value serializes");
    // A closed reader ends the run at the next write; nothing is owed to it.
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

pub(crate) async fn run(
    config: Config,
    modules: Option<std::path::PathBuf>,
    repeat: u32,
    concurrency: std::num::NonZeroUsize,
) -> ExitCode {
    let runtime = match Runtime::open_in_memory(config, modules, crate::api::Remedies::Live) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa runtime sanitize: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut values = Vec::new();
    for (number, line) in std::io::stdin().lock().lines().enumerate() {
        let parsed = line
            .map_err(|error| error.to_string())
            .and_then(|line| serde_json::from_str::<Value>(&line).map_err(|error| error.to_string()));
        match parsed {
            Ok(value) => values.push(value),
            Err(error) => {
                eprintln!("appa runtime sanitize: line {}: {error}", number + 1);
                return ExitCode::FAILURE;
            }
        }
    }
    let asks = values
        .iter()
        .flat_map(|value| (0..repeat).map(move |repeat| (value, repeat)))
        .map(|(value, repeat)| ask(&runtime, value, repeat));
    futures_util::stream::iter(asks)
        .buffer_unordered(concurrency.get())
        .collect::<()>()
        .await;
    ExitCode::SUCCESS
}
