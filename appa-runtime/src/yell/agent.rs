//! The agent's own yell: the tool's arguments, the ticket that ties one call to the
//! trajectory that made it, and the act itself.
//!
//! An MCP request carries no session. The hook that precedes the call is the one place the
//! harness names the trajectory, so it records the caller's standing under a ticket derived
//! from the call, and the tool spends that ticket to learn whose session it is reporting on.
//! A call no hook vouched for is refused rather than attributed to a guess.
//!
//! Nothing is written to disk here. `appa yell` keeps a file because a person is asked to
//! read it before it leaves; nobody reads an agent's, so a report that does not send is gone.

use sha2::Digest;

use crate::api::{Actor, PermitKey, Runtime};
use crate::runtime_cli::Adapter;

use super::client::{self, Receipt, SendFailure};
use super::report::{Author, ReportRequest, YellMessage};
use super::{Mode, Selection};

/// The arguments the `yell` tool takes.
///
/// One type for both readers: the hook parses the harness's tool input with it and the tool
/// parses the MCP request with it, so the ticket the hook records and the ticket the tool
/// spends are computed from the same reading of the same call. Unknown fields are refused,
/// which is what keeps those two readings identical.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct YellArgs {
    /// What APPA did that is in the way, in the agent's own words.
    pub(crate) message: String,
    /// Whether the report carries this session's decisions as well as the policy.
    pub(crate) with_trajectory: bool,
}

impl YellArgs {
    /// The key this call is vouched under: a digest of the call itself, because the tool
    /// takes no id and the arguments are the only thing the hook and the tool both see.
    ///
    /// RFC 8785 over the parsed arguments, not over the bytes either side received: the
    /// harness and the MCP client serialize the same call differently, and the digest has to
    /// survive that. A digest rather than the text, so nothing a person wrote is a map key.
    pub(crate) fn ticket(&self) -> PermitKey {
        let value = serde_json::to_value(self).expect("two owned scalars always serialize");
        let digest = sha2::Sha256::digest(appa_engine::params::canonical_bytes(&value));
        PermitKey::Yell(format!("{digest:x}"))
    }

    /// The tool input a harness reported, read as this call. `None` when it is not one:
    /// something else was proposed under a name that matched, and nothing is vouched.
    pub(crate) fn parse(arguments: &serde_json::value::RawValue) -> Option<Self> {
        serde_json::from_str(arguments.get()).ok()
    }
}

/// What one agent yell did, in the words the model is given back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent(Receipt),
    /// The message is not one: empty, or past what a report carries.
    Refused(String),
    /// No hook vouched for this call, so there is no session to report on and no way to know
    /// whether the caller is even an agent this runtime serves.
    Unvouched,
    /// The report is over what a receiver accepts even with nothing left to drop.
    Oversize,
    Undeliverable(SendFailure),
}

/// Build and send one agent's report.
///
/// Always in the deployment's own names: nobody is here to be asked the pseudonymization
/// question, and a deployment that turned agent reporting on has already answered it. What
/// may leave is the same either way — the mode chooses only how the names are spelled.
pub(crate) async fn yell(runtime: &std::sync::Arc<Runtime>, harness: Adapter, args: &YellArgs) -> Outcome {
    let Some((acting, _)) = runtime.take_vouched(&args.ticket()) else {
        return Outcome::Unvouched;
    };
    let message = match YellMessage::new(&args.message) {
        Ok(message) => message,
        Err(refusal) => return Outcome::Refused(refusal.to_string()),
    };
    let request = ReportRequest {
        message,
        author: Author::Agent,
        mode: Mode::Baseline,
        selection: selection(&acting, args.with_trajectory),
        harness,
    };
    let Ok(finished) = runtime.report_off_thread(request).await else {
        return Outcome::Oversize;
    };
    let Some(receiver) = client::Receiver::resolve() else {
        return Outcome::Undeliverable(SendFailure::NoReceiver);
    };
    match client::send(&finished, &receiver).await {
        Ok(receipt) => Outcome::Sent(receipt),
        Err(failure) => Outcome::Undeliverable(failure),
    }
}

/// The family, never the acting trajectory: a subagent's complaint is about the session it
/// runs in, and the export is of the whole family either way.
fn selection(acting: &Actor, with_trajectory: bool) -> Selection {
    match with_trajectory {
        true => Selection::Vouched(acting.root.clone()),
        false => Selection::RulesOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(message: &str, with_trajectory: bool) -> YellArgs {
        YellArgs {
            message: message.to_string(),
            with_trajectory,
        }
    }

    /// The hook and the tool never see the same bytes — one reads a harness's hook JSON and
    /// the other an MCP request — so the ticket has to come from the call, not its rendering.
    #[test]
    fn a_ticket_is_the_call_and_not_its_spelling() {
        let spelled = YellArgs::parse(
            &serde_json::value::RawValue::from_string(
                r#"{ "with_trajectory": true,
                     "message":  "the hook blocked" }"#
                    .to_string(),
            )
            .expect("the fixture is one JSON value"),
        )
        .expect("the fixture is this call");
        assert_eq!(spelled.ticket(), args("the hook blocked", true).ticket());
    }

    /// Every field is part of what was vouched: a call the hook let through must not spend
    /// its standing on a different report.
    #[test]
    fn a_different_call_is_a_different_ticket() {
        let ticket = args("the hook blocked", true).ticket();
        assert_ne!(ticket, args("the hook blocked", false).ticket());
        assert_ne!(ticket, args("the hook blocked ", true).ticket());
        assert_ne!(ticket, args("something else", true).ticket());
    }

    /// A ticket and an offer id are different things, and a map holding both must not
    /// confuse them however they are spelled.
    #[test]
    fn a_ticket_is_not_an_offer_id() {
        let ticket = args("x", true).ticket();
        let PermitKey::Yell(digest) = ticket.clone() else {
            panic!("a yell ticket");
        };
        assert_ne!(ticket, PermitKey::Offer(digest));
    }

    /// The proposal a hook reads is not always the call: the tool's name can be matched by
    /// something carrying other arguments, and that is not a call to vouch for.
    #[test]
    fn only_this_shape_is_a_yell_call() {
        for refused in [
            r#"{"message":"x"}"#,
            r#"{"with_trajectory":true}"#,
            r#"{"message":"x","with_trajectory":true,"endpoint":"https://evil.example"}"#,
            r#"{"message":"x","with_trajectory":"yes"}"#,
            r#"{"message":"x","with_trajectory":true,"trajectory":"someone-else"}"#,
            r#"[]"#,
        ] {
            let raw = serde_json::value::RawValue::from_string(refused.to_string()).expect("one JSON value");
            assert_eq!(YellArgs::parse(&raw), None, "{refused}");
        }
    }

    #[test]
    fn the_rules_alone_name_no_trajectory() {
        let acting = Actor {
            root: appa_runtime_api::TrajectoryId("root".to_string()),
            child: None,
        };
        assert_eq!(
            selection(&acting, true),
            Selection::Vouched(appa_runtime_api::TrajectoryId("root".to_string()))
        );
        assert_eq!(selection(&acting, false), Selection::RulesOnly);
    }

    /// A subagent reports on the family it runs in, not on itself: the export is of the
    /// whole family, and the child alone would name a log that holds part of the story.
    #[test]
    fn a_subagent_reports_on_its_family() {
        let acting = Actor {
            root: appa_runtime_api::TrajectoryId("root".to_string()),
            child: Some(appa_runtime_api::TrajectoryId("child".to_string())),
        };
        assert_eq!(
            selection(&acting, true),
            Selection::Vouched(appa_runtime_api::TrajectoryId("root".to_string()))
        );
    }
}
