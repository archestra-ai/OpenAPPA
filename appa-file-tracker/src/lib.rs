//! `appa file-tracker`: the file-tracker plugin's three entry points, served by the `appa`
//! executable so the plugin ships with every runtime.
//!
//! - `annotate` reads one annotation consult on stdin and writes its answer.
//! - `record` reads one recorder delivery on stdin and writes its acknowledgement.
//! - `mcp` serves SafeBash as an MCP server on stdio.
//! - `sandbox` reads one Claude Code `Bash` call's arguments on stdin and writes the
//!   confinement `clappa`'s preload gives Claude Code's sandbox for that call.
//! - `labels [<path>...]` lists the labels recorded writes left under each path, and
//!   `reset <path>...` removes them: the person's commands, run in their own terminal.
//!
//! Each takes `--config <file-tracker.toml>`.

/// The preload `clappa` loads into Claude Code to confine its own Bash per call; see the file.
pub const CLAUDE_BASH_PRELOAD: &str = include_str!("../claude-bash.js");

mod annotate;
mod config;
mod label;
mod paths;
mod record;
mod reset;
mod safebash;
mod store;

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

/// Why SafeBash cannot run a command on this machine, when it cannot: the sandbox program
/// the configuration names is not installed. A fresh install says so before the first
/// session, and `clappa` before every session that falls back to SafeBash.
pub fn sandbox_missing() -> Option<String> {
    let config = config::Config::load(None).ok()?;
    safebash::missing(&config)
}

/// What `clappa` says before a session in the current directory when files under it carry
/// labels earlier sessions recorded, with the commands that review and reset them.
pub fn earlier_labels() -> Option<String> {
    let config = config::Config::load(None).ok()?;
    reset::note(&config, &std::env::current_dir().ok()?)
}

const USAGE: &str = "usage: appa file-tracker <command> [--config <file-tracker.toml>]
  labels [<path>...]   list the labels recorded on files under each path (default: .)
  reset <path>...      remove the labels recorded on files under each path
  annotate | record | mcp | sandbox   the plugin's entry points, run by the runtime and clappa";

/// Run one entry point: `arguments` are what follows `appa file-tracker`.
pub fn run(arguments: impl IntoIterator<Item = String>) -> ExitCode {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if matches!(command.as_str(), "-h" | "--help" | "help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mut explicit: Option<PathBuf> = None;
    let mut paths: Vec<String> = Vec::new();
    while let Some(argument) = arguments.next() {
        match (argument.as_str(), argument.starts_with('-')) {
            ("--config", _) => match arguments.next() {
                Some(path) => explicit = Some(PathBuf::from(path)),
                None => {
                    eprintln!("appa file-tracker: --config names a file");
                    return ExitCode::from(2);
                }
            },
            (_, false) if matches!(command.as_str(), "labels" | "reset") => paths.push(argument),
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
        "annotate" => answer(|request| {
            let started = std::time::Instant::now();
            let answer = annotate::annotate(&config, request);
            log(&config, request, &answer, started.elapsed());
            answer
        }),
        "record" => answer(|delivery| record::record(&config, delivery)),
        "mcp" => safebash::serve(&config),
        "sandbox" => answer(|arguments| safebash::sandbox(&config, arguments)),
        "labels" => reset::labels(&config, &paths).map(|listing| print!("{listing}")),
        "reset" => reset::reset(&config, &paths).map(|done| print!("{done}")),
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

/// One line per annotation in `file-tracker.log` beside the store: the call's tool and the
/// files and hosts it names, and what the annotation answered or why it refused. A refused
/// call reaches no other record, so this is where its lists are found. The command text and
/// file contents are left out.
fn log(
    config: &config::Config,
    request: &serde_json::Value,
    answer: &Result<serde_json::Value, String>,
    elapsed: std::time::Duration,
) {
    use std::io::Write;
    let call = &request["artifact"]["args"];
    let arguments = &call["arguments"];
    let mut line = serde_json::json!({
        "at": store::now(),
        "tool": call["name"],
        "ms": elapsed.as_millis() as u64,
    });
    for field in ["file_path", "input_files", "output_files", "network_hosts"] {
        if !arguments[field].is_null() {
            line[field] = arguments[field].clone();
        }
    }
    match answer {
        Ok(answer) => {
            line["delta"] = answer["answer"]["delta"].clone();
            line["requires"] = answer["answer"]["requires"].clone();
        }
        Err(error) => line["refused"] = serde_json::json!(error),
    }
    let path = config.store.with_extension("log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
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
