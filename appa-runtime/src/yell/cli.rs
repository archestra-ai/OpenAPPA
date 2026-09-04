//! `appa yell` — telling the OpenAPPA team that this deployment is in the way.
//!
//! Two questions, always in this order, and the report is finished before the second one is
//! asked: a person approves the exact file that will leave the machine, not a promise about
//! it. `-y` answers both with yes.
//!
//! The document is built by the runtime, not here. That is the point of the split: this
//! process never holds an unclassified byte of a session, and the only thing it adds to the
//! report is the message the person typed.

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use super::client;
use super::report::{Finished, Origin, Report, ReportId, UnreachableClass, YellMessage};
use super::{Author, Mode};

/// How long the runtime gets to build a report. Longer than a hook's budget: a long session's
/// facts are stripped, serialized and gzipped before the answer comes back.
const BUILD_TIMEOUT: Duration = Duration::from_secs(30);

/// Run one yell to completion.
pub fn run(url: &str, yes: bool, message: Vec<String>) -> ExitCode {
    let message = match message_from(&message) {
        Ok(message) => message,
        Err(refusal) => {
            eprintln!("appa yell: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    let mode = match yes {
        true => Mode::Pseudonymized,
        false => ask_pseudonymization(),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("appa yell: no async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(yell(url, yes, message, mode))
}

async fn yell(url: &str, yes: bool, message: YellMessage, mode: Mode) -> ExitCode {
    let finished = match build(url, &message, mode).await {
        Ok(finished) => finished,
        Err(class) => match local(message, mode, class) {
            Ok(finished) => finished,
            Err(oversize) => {
                eprintln!("appa yell: {oversize}");
                return ExitCode::FAILURE;
            }
        },
    };
    let written = match super::report::write(finished, &std::env::temp_dir()) {
        Ok(written) => written,
        Err(error) => {
            eprintln!("appa yell: {error}");
            return ExitCode::FAILURE;
        }
    };
    let path = written.path.display();
    println!("The report is at {path}");
    if !yes && !ask_sharing(&written.path.display().to_string()) {
        println!("Not sent. The report is kept at {path}.");
        return ExitCode::SUCCESS;
    }
    match client::send(&written.finished).await {
        Ok(receipt) => {
            let already = match receipt.duplicate {
                true => " (already had this one)",
                false => "",
            };
            println!("Sent{already}. Receipt {}.", receipt.receipt_id);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            eprintln!("appa yell: {failure}");
            eprintln!("The report is kept at {path}.");
            ExitCode::FAILURE
        }
    }
}

/// Ask the runtime for the whole document.
async fn build(url: &str, message: &YellMessage, mode: Mode) -> Result<Finished, UnreachableClass> {
    let client = reqwest::Client::builder()
        .timeout(BUILD_TIMEOUT)
        .build()
        .map_err(|_| UnreachableClass::NotListening)?;
    let answer = client
        .post(format!("{}/report", url.trim_end_matches('/')))
        .json(&serde_json::json!({
            "message": message,
            "pseudonymize": mode == Mode::Pseudonymized,
        }))
        .send()
        .await
        .map_err(|error| match error.is_timeout() {
            true => UnreachableClass::Timeout,
            false => UnreachableClass::NotListening,
        })?;
    let status = answer.status();
    if !status.is_success() {
        return Err(UnreachableClass::Refused {
            status: status.as_u16(),
        });
    }
    let plain = answer.bytes().await.map_err(|_| UnreachableClass::Timeout)?;
    Finished::of(plain.to_vec()).map_err(|_| UnreachableClass::Refused {
        status: status.as_u16(),
    })
}

/// The report a person gets when no runtime answers. "It is not running" is a common thing to
/// be angry about, and this is how that anger arrives.
fn local(message: YellMessage, mode: Mode, class: UnreachableClass) -> Result<Finished, super::Oversize> {
    Report::unreachable(ReportId::generate(), Origin::new(Author::Cli, mode), message, class).finalize()
}

/// The message: the words on the command line, or everything on stdin, or one typed line.
fn message_from(words: &[String]) -> Result<YellMessage, String> {
    if !words.is_empty() {
        return YellMessage::new(&words.join(" ")).map_err(|refusal| refusal.to_string());
    }
    let raw = match std::io::stdin().is_terminal() {
        true => prompt("What is APPA doing wrong? ").unwrap_or_default(),
        false => {
            let mut piped = String::new();
            std::io::stdin()
                .read_to_string(&mut piped)
                .map_err(|error| format!("stdin is not readable: {error}"))?;
            piped
        }
    };
    YellMessage::new(&raw).map_err(|refusal| refusal.to_string())
}

/// The first question. Its wording is the whole of what the person is agreeing to, so it names
/// what pseudonymization replaces rather than calling it "additional privacy".
fn ask_pseudonymization() -> Mode {
    println!(
        "Additional pseudonymization replaces the names your policy chose — tools, effects, \n\
         authorities, sanitizers, trust ranks and audiences — with report-local tokens like \n\
         `tool-1`. It never changes what kinds of things the report carries."
    );
    match confirm("Apply additional pseudonymization?", true) {
        true => Mode::Pseudonymized,
        false => Mode::Baseline,
    }
}

/// The second question, asked only once the file exists, so the person can read it first.
fn ask_sharing(path: &str) -> bool {
    confirm(&format!("Share {path} with the OpenAPPA team?"), false)
}

/// A yes/no question. Anything but an explicit answer takes the default, including a closed
/// stdin: a pipe that ran out of input has not said yes to sending anything.
fn confirm(question: &str, default: bool) -> bool {
    let suffix = match default {
        true => "[Y/n]",
        false => "[y/N]",
    };
    match prompt(&format!("{question} {suffix} ")) {
        None => default,
        Some(answer) => match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        },
    }
}

/// One line from the person. `None` when there is nobody there.
fn prompt(question: &str) -> Option<String> {
    print!("{question}");
    std::io::stdout().flush().ok()?;
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_on_the_command_line_are_one_message() {
        let message = message_from(&["the".into(), "hook".into(), "blocked".into()]).expect("a message");
        assert_eq!(
            serde_json::to_value(&message).expect("a message serializes"),
            serde_json::json!("the hook blocked")
        );
    }

    #[test]
    fn an_empty_command_line_message_is_refused() {
        assert!(message_from(&["   ".into()]).is_err());
    }
}
