//! In-process execution of the corp-systems tools for `appa-corp-agent`.
//!
//! The runtime's tool backends are a closed set (builtin fixtures or HTTP), so
//! this module serves the corp tools on an ephemeral loopback socket and the
//! binary points one HTTP backend per registered policy tool at it. Semantics
//! delegate to the shared `corp-systems` crate — the same code the MCP server
//! wraps — and the response texts mirror the MCP server's, so the model sees
//! the same world through either binary.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use appa_runtime::tool::RenderedCall;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use corp_systems::server::{CreateArgs, ReadArgs, SearchArgs, SendEmailArgs};
use corp_systems::systems::{self, CreateError, ReadError, System};
use serde::Deserialize;

/// The corp world one episode acts on: the corpus root the `search`/`read`/
/// `create` verbs touch, the sink root `send_email` writes under, and which
/// systems are live (a disabled system's tools answer 404).
pub struct CorpWorld {
    pub data_root: PathBuf,
    pub sink_root: PathBuf,
    pub enabled: BTreeSet<System>,
}

/// Serve the corp tools on an ephemeral loopback port. Returns the bound
/// address; the server task lives for the rest of the process.
pub async fn serve(world: CorpWorld) -> std::io::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new().route("/", post(handle)).with_state(Arc::new(world));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(address)
}

async fn handle(
    State(world): State<Arc<CorpWorld>>,
    axum::Json(call): axum::Json<RenderedCall>,
) -> (StatusCode, String) {
    dispatch(&world, &call)
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
    } else if let Some(rest) = name.strip_prefix("create_") {
        (Verb::Create, rest)
    } else {
        return None;
    };
    System::parse(system).ok().map(|system| (verb, system))
}

/// Execute one rendered call against the world.
///
/// Status codes carry the runtime contract: effects commit only on 2xx, and a
/// non-2xx body never reaches the model. So a failed `create`/`send_email` is
/// always non-2xx — a 200 "already exists" would commit the tool's declared
/// effects for a write that never happened — while a `read`/`search` domain
/// error (no effects to commit) returns its explanatory text, the same
/// self-correction hint the MCP server delivers.
pub fn dispatch(world: &CorpWorld, call: &RenderedCall) -> (StatusCode, String) {
    if call.tool.as_str() == "send_email" {
        if !world.enabled.contains(&System::Email) {
            return unknown_tool(call.tool.as_str());
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
    let Some((verb, system)) = split_tool(call.tool.as_str()) else {
        return unknown_tool(call.tool.as_str());
    };
    if !world.enabled.contains(&system) {
        return unknown_tool(call.tool.as_str());
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
    use appa_engine::value::ToolName;

    fn world(root: &std::path::Path) -> CorpWorld {
        CorpWorld {
            data_root: root.join("data"),
            sink_root: root.join("sink"),
            enabled: System::ALL.into_iter().collect(),
        }
    }

    fn call(tool: &str, arguments: serde_json::Value) -> RenderedCall {
        RenderedCall {
            tool: ToolName::new(tool),
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
    fn disabled_and_unknown_tools_answer_404() {
        let root = scratch("disabled");
        let mut world = world(&root);
        world.enabled = [System::Hr].into_iter().collect();

        let (status, _) = dispatch(&world, &call("read_finance", serde_json::json!({"file": "x.md"})));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(&world, &call("send_email", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(&world, &call("frobnicate", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
