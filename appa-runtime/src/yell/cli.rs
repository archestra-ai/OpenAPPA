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

/// The marks a person reads the run by: a question, a step that succeeded, one that did not.
const ASK: &str = "?";
const TICK: &str = "✓";
const CROSS: &str = "✗";

/// Run one yell to completion.
pub fn run(url: &str, yes: bool, message: Vec<String>) -> ExitCode {
    // Before anything is asked: a build with nowhere to send to has nothing to ask about.
    let Some((receiver, source)) = client::Receiver::resolve() else {
        return fail(&client::SendFailure::NoReceiver);
    };
    let message = match message_from(&message) {
        Ok(message) => message,
        Err(refusal) => return fail(&refusal),
    };
    let mode = match yes {
        true => Mode::Pseudonymized,
        false => ask_pseudonymization(),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => return fail(&format!("no async runtime: {error}")),
    };
    runtime.block_on(yell(url, yes, message, mode, receiver, source))
}

async fn yell(
    url: &str,
    yes: bool,
    message: YellMessage,
    mode: Mode,
    receiver: client::Receiver,
    source: client::Source,
) -> ExitCode {
    let finished = match build(url, &message, mode).await {
        Ok(finished) => finished,
        Err(class) => match local(message, mode, class) {
            Ok(finished) => finished,
            Err(oversize) => return fail(&oversize),
        },
    };
    let written = match super::report::write(finished, &std::env::temp_dir()) {
        Ok(written) => written,
        Err(error) => return fail(&error),
    };
    let path = written.path.display();
    println!();
    println!("{TICK} Report written to {path}");
    println!();
    let destination = destination(source);
    match yes {
        true => println!("Sending it to {destination}."),
        false => {
            if !confirm(&format!("Share it with {destination}?"), false) {
                println!();
                println!("Not sent. The report stays at {path}.");
                return ExitCode::SUCCESS;
            }
        }
    }
    println!();
    match client::send(&written.finished, &receiver).await {
        Ok(receipt) => {
            let already = match receipt.duplicate {
                true => " (we already had this one)",
                false => "",
            };
            println!(
                "{TICK} Sent{already}, thank you. Your reference is {}.",
                reference(&receipt.receipt_id)
            );
            ExitCode::SUCCESS
        }
        Err(failure) => {
            fail(&failure);
            eprintln!("  The report stays at {path}.");
            ExitCode::FAILURE
        }
    }
}

/// Where the report goes, as the person is told it. Whoever set an override knows where it
/// points, so the address itself is not repeated; the tag says only that one is in effect.
fn destination(source: client::Source) -> &'static str {
    match source {
        client::Source::CompiledIn => "the OpenAPPA team",
        client::Source::Environment => "the OpenAPPA team (via APPA_YELL_ENDPOINT)",
    }
}

/// The start of a receipt id: enough for a person to quote and the team to find, and short
/// enough to read aloud.
fn reference(receipt_id: &str) -> &str {
    receipt_id.get(..10).unwrap_or(receipt_id)
}

/// One failure, on stderr, and the exit code that goes with it.
fn fail(error: &impl std::fmt::Display) -> ExitCode {
    eprintln!("{CROSS} appa yell: {error}");
    ExitCode::FAILURE
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
    // `reqwest` refuses to build any client until a provider is installed, TLS or not.
    crate::tls::install_crypto_provider();
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
    // Bounded by what a report may weigh, because whatever is on that port has not earned
    // the right to decide how much memory this process commits.
    let plain = client::bounded_body(answer, super::report::MAX_PLAIN_BYTES)
        .await
        .map_err(|refusal| match refusal {
            client::BodyRefusal::Transport => UnreachableClass::Timeout,
            client::BodyRefusal::TooLarge => UnreachableClass::NotARuntime,
        })?;
    // Something answered on the runtime's port. Whether it *is* the runtime is a different
    // question, and these bytes are about to be written to disk and offered for sending.
    Finished::of(plain)
        .ok()
        .filter(|finished| is_a_report(&finished.plain))
        .ok_or(UnreachableClass::NotARuntime)
}

/// Whether the answer claims to be the document this build knows how to send.
///
/// A discriminator, not an identity check: it separates the runtime from whatever else may be
/// listening on that port, and nothing here proves the answer came from a runtime. Read
/// through a probe rather than a `Value`, so a large document is not also materialized as a
/// tree of nodes to look at one string.
fn is_a_report(plain: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Probe {
        schema: String,
    }
    serde_json::from_slice::<Probe>(plain).is_ok_and(|probe| probe.schema == super::report::SCHEMA)
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
        true => {
            println!("{}", bold("What is APPA doing wrong?"));
            prompt("> ").unwrap_or_default()
        }
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
    println!();
    match confirm(
        "Pseudonymize your policy's names (tools, effects, authorities, sanitizers,\n  \
         trust ranks, audiences) as tokens like tool-1? Your message is never changed.",
        false,
    ) {
        true => Mode::Pseudonymized,
        false => Mode::Baseline,
    }
}

/// A yes/no question. Anything but an explicit answer takes the default, including a closed
/// stdin: a pipe that ran out of input has not said yes to sending anything.
fn confirm(question: &str, default: bool) -> bool {
    let suffix = match default {
        true => "[Y/n]",
        false => "[y/N]",
    };
    match prompt(&format!("{ASK} {}  {suffix} ", bold(question))) {
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

/// Bold when a person is looking at a terminal; plain when the output is a file or a pipe.
fn bold(text: &str) -> String {
    match std::io::stdout().is_terminal() {
        true => format!("\x1b[1m{text}\x1b[0m"),
        false => text.to_string(),
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

    /// Asking the runtime builds a client, which this crate's `reqwest` refuses to do until a
    /// crypto provider is installed. A `yell` with no runtime up must produce the metadata-only
    /// report, not a panic.
    #[tokio::test]
    async fn a_runtime_that_is_not_there_is_a_class_and_not_a_panic() {
        let message = YellMessage::new("nobody is home").expect("a message");
        let refusal = build("http://127.0.0.1:1", &message, Mode::Baseline)
            .await
            .expect_err("nothing is listening on port 1");
        assert_eq!(refusal, UnreachableClass::NotListening);
        assert!(
            local(message, Mode::Baseline, refusal).is_ok(),
            "the report is still made"
        );
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
