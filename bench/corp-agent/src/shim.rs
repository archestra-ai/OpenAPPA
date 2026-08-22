//! The loopback listener `appa-corp-agent` serves everything on.
//!
//! Two kinds of thing arrive here. The corp tools: running a released call is
//! the harness's job, so the agent posts each one and this module executes it.
//! Semantics delegate to the shared `corp-systems` crate — the same code the
//! MCP server wraps — and the response texts mirror the MCP server's, so the
//! model sees the same world through either binary.
//!
//! And one registered external: `pii-redactor`. The runtime ships
//! `redact-email` as its stock declassifier, not a number redactor, so a
//! deployment that wants one hosts it. This is that host — the same listener,
//! a different path — and the binary rewrites the policy's unbound endpoint
//! onto it once the port is real.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use corp_systems::server::{CreateArgs, ExecuteWireArgs, ReadArgs, SearchArgs, SendEmailArgs, ShareLegalPacketArgs};
use corp_systems::systems::{self, CreateError, ReadError, ShareLegalPacketError, System};
use serde::{Deserialize, Serialize};

/// Where the agent posts a released call.
pub const TOOLS_PATH: &str = "/tools";

/// Where the `pii-redactor` sanitizer answers. The path identifies the
/// external to whoever hosts it, so the policy names this one and the binary
/// only ever rewrites the origin in front of it.
pub const REDACTOR_PATH: &str = "/sanitizer/pii-redactor";

/// Whether this shim implements the external at `path`. An unbound endpoint it
/// cannot serve stays unbound and fails closed, rather than being pointed at a
/// listener that would answer it 404.
pub fn serves(path: &str) -> bool {
    path == REDACTOR_PATH
}

/// One released call as the agent posts it: the tool, and the arguments
/// exactly as the model spelled them (`RUL-3`).
#[derive(Debug, Clone, Deserialize)]
pub struct Dispatch {
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// One sanitizer consult as the runtime posts it (`IMP-3`).
#[derive(Debug, Deserialize)]
struct Consult {
    version: u32,
    payload: ConsultPayload,
}

#[derive(Debug, Deserialize)]
struct ConsultPayload {
    body: String,
}

#[derive(Debug, Serialize)]
struct ConsultAnswer {
    version: u32,
    answer: Derivation,
}

#[derive(Debug, Serialize)]
struct Derivation {
    body: String,
}

/// The corp world one episode acts on: the corpus root the `search`/`read`/
/// `create` verbs touch, the sink root `send_email` writes under, and which
/// systems are live (a disabled system's tools answer 404).
pub struct CorpWorld {
    pub data_root: PathBuf,
    pub sink_root: PathBuf,
    pub enabled: BTreeSet<System>,
}

/// Serve the corp tools and the redactor on an ephemeral loopback port.
/// Returns the bound address; the server task lives for the rest of the
/// process.
pub async fn serve(world: CorpWorld) -> std::io::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new()
        .route(TOOLS_PATH, post(handle))
        .route(REDACTOR_PATH, post(redact))
        .with_state(Arc::new(world));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(address)
}

async fn handle(State(world): State<Arc<CorpWorld>>, body: String) -> (StatusCode, String) {
    // The agent sends the arguments as the model spelled them, so a body
    // this host cannot parse is a fact about the model's proposal, not an
    // error to hide. The engine already refused the shapes it judges
    // inadmissible; anything left is this host's own contract.
    match serde_json::from_str::<Dispatch>(&body) {
        Ok(call) => dispatch(&world, &call),
        Err(error) => (StatusCode::BAD_REQUEST, format!("bad dispatch: {error}")),
    }
}

/// The `pii-redactor` consult (`SAN-6`): registration is the trust decision,
/// so the implementation is deliberately plain. `NOT_ACCEPTABLE` for a shape
/// this host does not speak — the runtime reads any non-answer as no answer
/// and fails closed (`EXT-1`), so a wrong version never becomes a derivation.
async fn redact(axum::Json(consult): axum::Json<Consult>) -> Result<axum::Json<ConsultAnswer>, StatusCode> {
    if consult.version != 1 {
        return Err(StatusCode::NOT_ACCEPTABLE);
    }
    Ok(axum::Json(ConsultAnswer {
        version: 1,
        answer: Derivation {
            body: redact_numbers(&consult.payload.body),
        },
    }))
}

/// Replace every maximal ASCII-digit run with a fixed placeholder: it strips
/// numeric identifiers — social security numbers, salaries, account and
/// extension digits — and keeps everything else verbatim.
fn redact_numbers(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_digits = false;
    for ch in input.chars() {
        if ch.is_ascii_digit() {
            if !in_digits {
                out.push_str("[redacted-number]");
                in_digits = true;
            }
        } else {
            in_digits = false;
            out.push(ch);
        }
    }
    out
}

/// One tool's verb, parsed from the `{verb}_{system}` naming convention.
enum Verb {
    Search,
    Read,
    Create,
}

fn split_tool(name: &str) -> Option<(Verb, System)> {
    let (verb, system) = if let Some(rest) = name.strip_prefix("search_") {
        (Verb::Search, rest)
    } else if let Some(rest) = name.strip_prefix("read_") {
        (Verb::Read, rest)
    } else {
        (Verb::Create, name.strip_prefix("create_")?)
    };
    match System::parse(system).ok()? {
        System::Email | System::Wire => None,
        system @ (System::Hr | System::Finance | System::TaskTracker | System::PublicForum | System::Vendor) => {
            Some((verb, system))
        }
    }
}

/// Execute one released call against the world.
///
/// Status codes carry the runtime contract: effects commit only on 2xx, and a
/// non-2xx body never reaches the model. So a failed `create`, `send_email`, or
/// `share_legal_packet` is always non-2xx — a false success would commit the
/// tool's declared effects for a write that never happened — while a
/// `read`/`search` domain error (no effects to commit) returns its explanatory
/// text, the same self-correction hint the MCP server delivers.
pub fn dispatch(world: &CorpWorld, call: &Dispatch) -> (StatusCode, String) {
    if call.tool == "execute_wire" {
        if !world.enabled.contains(&System::Wire) {
            return unknown_tool(&call.tool);
        }
        return match parse::<ExecuteWireArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::execute_wire(
                &world.sink_root,
                &args.request_file,
                args.amount_usd,
                &args.beneficiary_account,
            ) {
                Ok(file) => (
                    StatusCode::OK,
                    format!("wire executed; receipt archived as wire/{file}"),
                ),
                Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                    (StatusCode::BAD_REQUEST, format!("execute_wire failed: {error}"))
                }
                Err(error) => (StatusCode::BAD_GATEWAY, format!("execute_wire failed: {error}")),
            },
        };
    }
    if call.tool == "share_legal_packet" {
        if !world.enabled.contains(&System::Finance) || !world.enabled.contains(&System::Email) {
            return unknown_tool(&call.tool);
        }
        return match parse::<ShareLegalPacketArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::share_legal_packet(&world.data_root, &world.sink_root, &args.file, &args.to) {
                Ok(shared) => (StatusCode::OK, shared.to_string()),
                Err(error @ ShareLegalPacketError::Read(ReadError::Name(_))) => {
                    (StatusCode::BAD_REQUEST, error.to_string())
                }
                Err(error @ ShareLegalPacketError::Read(ReadError::NotFound { .. })) => {
                    (StatusCode::NOT_FOUND, error.to_string())
                }
                Err(error @ ShareLegalPacketError::Read(ReadError::Io { .. })) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                }
                Err(error @ ShareLegalPacketError::Send(_)) => (StatusCode::BAD_GATEWAY, error.to_string()),
            },
        };
    }
    if call.tool == "send_email" {
        if !world.enabled.contains(&System::Email) {
            return unknown_tool(&call.tool);
        }
        return match parse::<SendEmailArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::send_email(&world.sink_root, &args.to, &args.subject, &args.body) {
                Ok(file) => (
                    StatusCode::OK,
                    format!(
                        "email sent to {} (subject: {:?}); archived as {file}",
                        args.to, args.subject
                    ),
                ),
                Err(error) => (StatusCode::BAD_GATEWAY, format!("send_email failed: {error}")),
            },
        };
    }
    let Some((verb, system)) = split_tool(&call.tool) else {
        return unknown_tool(&call.tool);
    };
    if !world.enabled.contains(&system) {
        return unknown_tool(&call.tool);
    }
    match verb {
        Verb::Search => match parse::<SearchArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::search(&world.data_root, system, &args.query) {
                Ok(hits) if hits.is_empty() => (
                    StatusCode::OK,
                    format!("no matches for {:?} in the {system} system", args.query),
                ),
                Ok(hits) => {
                    let mut out = format!("{} match(es) in the {system} system:\n", hits.len());
                    for hit in hits {
                        out.push_str(&format!("- {} — {}\n", hit.file, hit.snippet));
                    }
                    (StatusCode::OK, out)
                }
                Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("search failed: {error}")),
            },
        },
        Verb::Read => match parse::<ReadArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::read(&world.data_root, system, &args.file) {
                Ok(body) => (StatusCode::OK, body),
                Err(error @ (ReadError::Name(_) | ReadError::NotFound { .. })) => (StatusCode::OK, error.to_string()),
                Err(error @ ReadError::Io { .. }) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            },
        },
        Verb::Create => match parse::<CreateArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match systems::create(&world.data_root, system, &args.file, &args.content) {
                Ok(()) => (StatusCode::OK, format!("created {} in the {system} system", args.file)),
                Err(error @ (CreateError::Name(_) | CreateError::Exists { .. })) => {
                    (StatusCode::CONFLICT, error.to_string())
                }
                Err(error @ CreateError::Io { .. }) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            },
        },
    }
}

fn parse<'a, T: Deserialize<'a>>(arguments: &'a serde_json::Value) -> Result<T, String> {
    T::deserialize(arguments).map_err(|error| format!("bad arguments: {error}"))
}

fn unknown_tool(name: &str) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("no tool named {name:?} is enabled"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(root: &std::path::Path) -> CorpWorld {
        CorpWorld {
            data_root: root.join("data"),
            sink_root: root.join("sink"),
            enabled: System::ALL.into_iter().collect(),
        }
    }

    fn call(tool: &str, arguments: serde_json::Value) -> Dispatch {
        Dispatch {
            tool: tool.to_string(),
            arguments,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fork-tools-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_and_search_round_trip() {
        let root = scratch("read");
        std::fs::create_dir_all(root.join("data/hr")).unwrap();
        std::fs::write(root.join("data/hr/alice.md"), "Buddy: Priya\n").unwrap();
        let world = world(&root);

        let (status, body) = dispatch(&world, &call("read_hr", serde_json::json!({"file": "alice.md"})));
        assert_eq!((status, body.as_str()), (StatusCode::OK, "Buddy: Priya\n"));

        let (status, body) = dispatch(&world, &call("search_hr", serde_json::json!({"query": "priya"})));
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("alice.md"));

        // A missing file is a 200 hint (no effects to commit), like the MCP server's error text.
        let (status, body) = dispatch(&world, &call("read_hr", serde_json::json!({"file": "nope.md"})));
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("alice.md"));
    }

    #[test]
    fn create_collision_is_non_2xx_so_effects_never_commit() {
        let root = scratch("create");
        let world = world(&root);
        let arguments = serde_json::json!({"file": "post.md", "content": "hello"});

        let (status, _) = dispatch(&world, &call("create_public_forum", arguments.clone()));
        assert_eq!(status, StatusCode::OK);
        let (status, _) = dispatch(&world, &call("create_public_forum", arguments));
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            std::fs::read_to_string(root.join("data/public_forum/post.md")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn vendor_has_file_tools_but_email_remains_write_only() {
        let root = scratch("vendor");
        std::fs::create_dir_all(root.join("data/vendor")).unwrap();
        std::fs::write(root.join("data/vendor/acme.md"), "Status: approved\n").unwrap();
        std::fs::create_dir_all(root.join("sink/email")).unwrap();
        std::fs::write(root.join("sink/email/message.md"), "secret archive").unwrap();
        let world = world(&root);

        let (status, body) = dispatch(&world, &call("read_vendor", serde_json::json!({"file": "acme.md"})));
        assert_eq!((status, body.as_str()), (StatusCode::OK, "Status: approved\n"));

        for tool in ["search_email", "read_email", "create_email"] {
            let (status, _) = dispatch(&world, &call(tool, serde_json::json!({})));
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{tool} must not expose the email archive"
            );
        }
    }

    #[test]
    fn send_email_lands_in_the_sink() {
        let root = scratch("email");
        let world = world(&root);
        let (status, body) = dispatch(
            &world,
            &call(
                "send_email",
                serde_json::json!({"to": "a@b.example", "subject": "Hi", "body": "text"}),
            ),
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with("email sent to a@b.example"));
        assert_eq!(std::fs::read_dir(root.join("sink/email")).unwrap().count(), 1);
    }

    #[test]
    fn execute_wire_lands_only_in_the_dedicated_sink() {
        let root = scratch("wire");
        let world = world(&root);
        let (status, body) = dispatch(
            &world,
            &call(
                "execute_wire",
                serde_json::json!({
                    "request_file": "WIRE-REQUEST-880.md",
                    "amount_usd": 72500,
                    "beneficiary_account": "NW-ACCT-4408"
                }),
            ),
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with("wire executed"));
        assert!(root.join("sink/wire/WIRE-REQUEST-880.md.json").is_file());
        assert!(!root.join("data/wire").exists());
    }

    #[test]
    fn share_legal_packet_dispatches_with_exact_email_body() {
        let root = scratch("legal-packet");
        let packet = "# Legal packet\n\nCounterparty: Acme\n";
        std::fs::create_dir_all(root.join("data/finance")).unwrap();
        std::fs::write(root.join("data/finance/acme.md"), packet).unwrap();
        let world = world(&root);

        let (status, body) = dispatch(
            &world,
            &call(
                "share_legal_packet",
                serde_json::json!({"file": "acme.md", "to": "legal@example.com"}),
            ),
        );

        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty());
        let mut emails = std::fs::read_dir(root.join("sink/email")).unwrap();
        let email = emails.next().unwrap().unwrap().path();
        assert!(emails.next().is_none());
        assert_eq!(
            std::fs::read_to_string(email).unwrap(),
            format!("To: legal@example.com\nSubject: Legal packet: acme.md\n\n{packet}")
        );
    }

    #[test]
    fn share_legal_packet_failures_are_non_2xx_and_do_not_commit_effects() {
        let root = scratch("legal-packet-failures");
        let world = world(&root);

        let (status, _) = dispatch(
            &world,
            &call(
                "share_legal_packet",
                serde_json::json!({"file": "missing.md", "to": "legal@example.com"}),
            ),
        );
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!root.join("sink/email").exists());

        std::fs::create_dir_all(root.join("data/finance")).unwrap();
        std::fs::write(root.join("data/finance/acme.md"), "packet").unwrap();
        std::fs::write(root.join("sink"), "not a directory").unwrap();
        let (status, _) = dispatch(
            &world,
            &call(
                "share_legal_packet",
                serde_json::json!({"file": "acme.md", "to": "legal@example.com"}),
            ),
        );
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn share_legal_packet_requires_finance_and_email() {
        let root = scratch("legal-packet-enabled");
        std::fs::create_dir_all(root.join("data/finance")).unwrap();
        std::fs::write(root.join("data/finance/acme.md"), "packet").unwrap();
        let arguments = serde_json::json!({"file": "acme.md", "to": "legal@example.com"});

        for enabled in [[System::Finance], [System::Email]] {
            let mut world = world(&root);
            world.enabled = enabled.into_iter().collect();
            let (status, _) = dispatch(&world, &call("share_legal_packet", arguments.clone()));
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(!root.join("sink/email").exists());
        }

        let mut world = world(&root);
        world.enabled = [System::Finance, System::Email].into_iter().collect();
        let (status, _) = dispatch(&world, &call("share_legal_packet", arguments));
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn disabled_and_unknown_tools_answer_404() {
        let root = scratch("disabled");
        let mut world = world(&root);
        world.enabled = [System::Hr].into_iter().collect();

        let (status, _) = dispatch(&world, &call("read_finance", serde_json::json!({"file": "x.md"})));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(&world, &call("send_email", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(
            &world,
            &call(
                "share_legal_packet",
                serde_json::json!({"file": "x.md", "to": "legal@example.com"}),
            ),
        );
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(&world, &call("frobnicate", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
