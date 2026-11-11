//! `appa-dogwood`: the Dogwood plugin's two entry points, each one JSON request on standard
//! input and one JSON answer on standard output.
//!
//! - `annotate` answers one annotation consult: it replays the recorded history through the
//!   Dogwood policy, then decides the proposed call as the next request. A call Dogwood denies
//!   requires the reserved `blocked` mark; a call it allows requires nothing.
//! - `record` takes one recorder delivery and adds the calls it released and their outcomes
//!   to that history.
//!
//! Dogwood is a superset of Cedar, so a plain Cedar policy set runs here unchanged. Each entry
//! point takes `--config <dogwood.toml>`.

mod annotate;
mod config;
mod history;
mod record;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let (Some(command), Some(flag), Some(path), None) =
        (arguments.next(), arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!("usage: appa-dogwood annotate|record --config <dogwood.toml>");
        return ExitCode::from(2);
    };
    if flag != "--config" {
        eprintln!("appa-dogwood: unknown argument {flag:?}");
        return ExitCode::from(2);
    }
    let outcome = config::Config::load(&PathBuf::from(path)).and_then(|config| match command.as_str() {
        "annotate" => answer(|request| annotate::annotate(&config, request)),
        "record" => answer(|delivery| record::record(&config, delivery)),
        other => Err(format!("unknown command {other:?}")),
    });
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa-dogwood: {error}");
            ExitCode::FAILURE
        }
    }
}

/// One JSON request on stdin, one JSON answer on stdout. A refusal exits non-zero, which the
/// runtime reads as no answer: the call does not run, or the delivery is retried.
fn answer(handle: impl FnOnce(&serde_json::Value) -> Result<serde_json::Value, String>) -> Result<(), String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| error.to_string())?;
    let request: serde_json::Value = serde_json::from_str(&input).map_err(|error| error.to_string())?;
    println!("{}", handle(&request)?);
    Ok(())
}
