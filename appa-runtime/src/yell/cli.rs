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
    let Some(receiver) = client::Receiver::resolve() else {
        eprintln!("appa yell: {}", client::SendFailure::NoReceiver);
        eprintln!("The report is kept at {path}.");
        return ExitCode::FAILURE;
    };
    // Named, not described. "The OpenAPPA team" is a claim about a URL, and the person is
    // owed the URL — under `-y` too, where they are told where it went rather than asked.
    match yes {
        true => println!("Sending to {}.", receiver.as_str()),
        false => {
            if !ask_sharing(&written.path.display().to_string(), receiver.as_str()) {
                println!("Not sent. The report is kept at {path}.");
                return ExitCode::SUCCESS;
            }
        }
    }
    match client::send(&written.finished, &receiver).await {
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
///
/// This request carries the message the person has just typed and has not yet agreed to send
/// anywhere, so it must reach the runtime on this machine or nothing at all. The endpoint is a
/// loopback literal, checked here rather than resolved; no proxy is consulted, whatever the
/// environment says; and a redirect is not followed, because a redirect is another
/// destination.
async fn build(url: &str, message: &YellMessage, mode: Mode) -> Result<Finished, UnreachableClass> {
    let endpoint = runtime_report_url(url).ok_or(UnreachableClass::NotLoopback)?;
    let client = reqwest::Client::builder()
        .timeout(BUILD_TIMEOUT)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| UnreachableClass::NotListening)?;
    let answer = client
        .post(endpoint)
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
    // Something answered on the runtime's port. Whether it *is* the runtime is a different
    // question, and these bytes are about to be written to disk and offered for sending.
    Finished::of(plain.to_vec())
        .ok()
        .filter(|finished| is_a_report(&finished.plain))
        .ok_or(UnreachableClass::NotARuntime)
}

/// Whether the answer claims to be the document this build knows how to send.
///
/// A discriminator, not an identity check: it separates the runtime from whatever else may be
/// listening on that port, and nothing here proves the answer came from a runtime.
fn is_a_report(plain: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(plain).is_ok_and(|document| document["schema"] == super::report::SCHEMA)
}

/// `<url>/report`, when `url` is `http://<loopback literal>[:port]` and nothing else.
///
/// A host *name* is refused rather than resolved: `localhost` is whatever the resolver says
/// today, and what this posts is a person's unreviewed words. Credentials, a path, a query and
/// a fragment are all refused too — the runtime's own flag has none of them, so a URL carrying
/// one was written by something other than this deployment.
fn runtime_report_url(url: &str) -> Option<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let plain_loopback = parsed.scheme() == "http" && client::is_loopback(&parsed);
    let bare = parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && matches!(parsed.path(), "" | "/");
    match plain_loopback && bare {
        true => parsed.join("report").ok(),
        false => None,
    }
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
         `tool-1`. It never changes what kinds of things the report carries, and it does not \n\
         touch your message, which is sent exactly as you write it."
    );
    match confirm("Apply additional pseudonymization?", true) {
        true => Mode::Pseudonymized,
        false => Mode::Baseline,
    }
}

/// The second question, asked only once the file exists, so the person can read it first, and
/// naming the destination it will actually go to.
fn ask_sharing(path: &str, receiver: &str) -> bool {
    confirm(&format!("Share {path} with the OpenAPPA team at {receiver}?"), false)
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

    /// The message goes to the runtime before anyone has agreed to send it anywhere, so the
    /// runtime has to be on this machine and named as an address rather than as a name someone
    /// else's resolver answers.
    #[test]
    fn only_a_loopback_literal_is_a_runtime() {
        for reachable in [
            "http://127.0.0.1:8787",
            "http://127.0.0.1:8787/",
            "http://[::1]:8787",
            "http://127.0.0.1",
        ] {
            let endpoint = runtime_report_url(reachable).unwrap_or_else(|| panic!("{reachable} is this machine"));
            assert_eq!(endpoint.path(), "/report");
            assert!(client::is_loopback(&endpoint));
        }
        for refused in [
            "http://localhost:8787",
            "https://127.0.0.1:8787",
            "http://10.0.0.1:8787",
            "http://user@127.0.0.1:8787/",
            "http://127.0.0.1@evil.example/",
            "http://evil.example/127.0.0.1",
            "http://127.0.0.1:8787/somewhere",
            "127.0.0.1:8787",
            "",
        ] {
            assert!(runtime_report_url(refused).is_none(), "{refused} is not this machine");
        }
    }

    /// Something answering on the runtime's port is not the runtime. What it says is written to
    /// disk and offered for sending, so it is checked for being the document this build sends.
    #[test]
    fn only_this_schema_is_a_report() {
        assert!(is_a_report(br#"{"schema":"openappa.yell.v1","message":"x"}"#));
        assert!(!is_a_report(br#"{"schema":"openappa.yell.v2"}"#));
        assert!(!is_a_report(b"{}"));
        assert!(!is_a_report(b"<html>not a runtime</html>"));
    }
}
