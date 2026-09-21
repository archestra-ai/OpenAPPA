//! `appa runtime annotate`: a policy's Annotators asked about a list of calls.
//!
//! Standard input is JSON lines, one call each: `id`, `tool` (the name the policy writes),
//! and `arguments`. Every call is asked `--repeat` times through the production consult
//! path — the same declaration, prompt, backend, and mandate check a session uses — and
//! every answer is one JSON line on standard output, written as it arrives. No trajectory
//! is opened, so an Annotator is asked afresh each time and no tool runs.
//!
//! An ask waits for its consult gate inside the consult's own deadline, so `--concurrency`
//! bounds how many calls are in flight at once.

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::time::Instant;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::api::Runtime;
use crate::config::Config;
use crate::external::ConsultOutcome;

#[derive(Deserialize)]
struct Call {
    id: String,
    tool: String,
    arguments: Box<RawValue>,
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Outcome {
    /// A static contract covers the call: no Annotator is asked.
    Static,
    Answer {
        annotator: String,
        answer: serde_json::Value,
    },
    /// An answer outside the Annotator's declared mandate, which refuses the call.
    OutsideMandate {
        annotator: String,
        answer: serde_json::Value,
    },
    /// No answer, which refuses the call.
    NoAnswer { annotator: String, reason: String },
    /// The policy names no such tool, or the arguments are not the tool's.
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

async fn ask(runtime: &Runtime, call: &Call, repeat: u32) {
    let started = Instant::now();
    let outcome = match runtime.annotate(&call.tool, call.arguments.get().as_bytes()).await {
        Ok(None) => Outcome::Static,
        Ok(Some(consult)) => match consult.outcome {
            ConsultOutcome::Answer(answer) if consult.admitted => Outcome::Answer {
                annotator: consult.annotator,
                answer,
            },
            ConsultOutcome::Answer(answer) => Outcome::OutsideMandate {
                annotator: consult.annotator,
                answer,
            },
            ConsultOutcome::NoAnswer(reason) => Outcome::NoAnswer {
                annotator: consult.annotator,
                reason: reason.diagnostic(),
            },
        },
        Err(error) => Outcome::Unresolved {
            reason: error.to_string(),
        },
    };
    let line = serde_json::to_string(&Answered {
        id: &call.id,
        repeat,
        latency_ms: started.elapsed().as_millis(),
        outcome,
    })
    .expect("an answered call serializes");
    // A closed reader ends the run at the next write; nothing is owed to it.
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

pub(crate) async fn run(
    config: Config,
    modules: Option<std::path::PathBuf>,
    repeat: u32,
    concurrency: usize,
) -> ExitCode {
    let runtime = match Runtime::open_in_memory(config, modules) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa runtime annotate: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut calls = Vec::new();
    for (number, line) in std::io::stdin().lock().lines().enumerate() {
        let parsed = line
            .map_err(|error| error.to_string())
            .and_then(|line| serde_json::from_str::<Call>(&line).map_err(|error| error.to_string()));
        match parsed {
            Ok(call) => calls.push(call),
            Err(error) => {
                eprintln!("appa runtime annotate: line {}: {error}", number + 1);
                return ExitCode::FAILURE;
            }
        }
    }
    let asks = calls
        .iter()
        .flat_map(|call| (0..repeat).map(move |repeat| (call, repeat)))
        .map(|(call, repeat)| ask(&runtime, call, repeat));
    futures_util::stream::iter(asks)
        .buffer_unordered(concurrency)
        .collect::<()>()
        .await;
    ExitCode::SUCCESS
}
