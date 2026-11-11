//! `appa file-tracker`: the file-tracker plugin's three entry points, served by the `appa`
//! executable so the plugin ships with every runtime.
//!
//! - `annotate` reads one annotation consult on stdin and writes its answer.
//! - `record` reads one recorder delivery on stdin and writes its acknowledgement.
//! - `mcp` serves SafeBash as an MCP server on stdio.
//! - `sandbox` reads one Claude Code `Bash` call's arguments on stdin and writes the
//!   confinement `clappa`'s preload gives Claude Code's sandbox for that call.
//!
//! Each takes `--config <file-tracker.toml>`.

/// The preload `clappa` loads into Claude Code to confine its own Bash per call; see the file.
pub const CLAUDE_BASH_PRELOAD: &str = include_str!("../claude-bash.js");

mod annotate;
mod config;
mod label;
mod paths;
mod record;
mod safebash;
mod store;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

/// Run one entry point: `arguments` are what follows `appa file-tracker`.
pub fn run(arguments: impl IntoIterator<Item = String>) -> ExitCode {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        eprintln!("usage: appa file-tracker annotate|record|mcp|sandbox [--config <file-tracker.toml>]");
        return ExitCode::from(2);
    };
    let mut explicit: Option<PathBuf> = None;
    while let Some(argument) = arguments.next() {
        match (argument.as_str(), arguments.next()) {
            ("--config", Some(path)) => explicit = Some(PathBuf::from(path)),
            _ => {
                eprintln!("appa file-tracker: unknown argument {argument:?}");
                return ExitCode::from(2);
            }
        }
    }
    let config = match config::Config::load(explicit.as_deref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("appa file-tracker: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match command.as_str() {
        "annotate" => answer(|request| annotate::annotate(&config, request)),
        "record" => answer(|delivery| record::record(&config, delivery)),
        "mcp" => safebash::serve(&config),
        "sandbox" => answer(|arguments| safebash::sandbox(&config, arguments)),
        other => Err(format!("unknown command {other:?}")),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa file-tracker: {error}");
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
