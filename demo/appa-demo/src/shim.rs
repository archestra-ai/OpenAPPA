//! In-process execution of the playground's tools, behind a loopback socket.
//!
//! The runtime's tool backends are a closed set (builtin fixtures or HTTP), so
//! the session serves its tools here and points one HTTP backend per policy
//! tool at this address.
//!
//! Status codes carry the runtime's contract, so they are chosen deliberately:
//! effects commit only on 2xx, and a non-2xx body never reaches the model. A
//! failed write is therefore always non-2xx — a false success would commit the
//! tool's declared effects for a write that never happened — while an empty
//! list (nothing to commit) answers 200 with explanatory text.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use appa_runtime::tool::RenderedCall;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use serde::Deserialize;

use crate::approvals::Approvals;
use crate::derive::Derivations;

use crate::systems::{
    CreateCustomerArgs, CreateError, CreateIssueArgs, SendEmailArgs, System, TransferArgs, Verb, create, list,
    next_number, route,
};

/// The world one session acts on: its private data root, which systems are
/// switched on, and the desk where a human ruling parks. A disabled system's
/// tools answer 404.
pub struct World {
    pub data_root: PathBuf,
    pub enabled: BTreeSet<System>,
    pub approvals: Arc<Approvals>,
    pub derivations: Arc<Derivations>,
}

/// Serve the tools and session-hosted external resolvers on an ephemeral
/// loopback port; the task lives with the process. Returns the bound address.
pub async fn serve(world: World) -> std::io::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let app = Router::new()
        .route("/", post(handle))
        .route("/authority", post(authority))
        .route("/sanitizer/{name}", post(sanitize))
        .route("/dynamic-resolver", post(dynamic_resolver))
        .with_state(Arc::new(world));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(address)
}

async fn handle(State(world): State<Arc<World>>, axum::Json(call): axum::Json<RenderedCall>) -> (StatusCode, String) {
    dispatch(&world, &call)
}

async fn authority(
    State(world): State<Arc<World>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let tool = request
        .get("tool")
        .and_then(|tool| tool.as_str())
        .unwrap_or("(unknown tool)")
        .to_string();
    let ruling = match world.approvals.request(&tool, request).await {
        Some(true) => "approve",
        Some(false) => "deny",
        None => "abstain",
    };
    axum::Json(serde_json::json!({ "ruling": ruling }))
}

async fn sanitize(
    State(world): State<Arc<World>>,
    Path(name): Path<String>,
    axum::Json(input): axum::Json<SanitizerInput>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    match world.derivations.derive(&name, &input.body).await {
        Some(body) => (StatusCode::OK, axum::Json(serde_json::json!({ "body": body }))),
        None => (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({ "error": "the derivation could not be produced" })),
        ),
    }
}

#[derive(Deserialize)]
pub struct SanitizerInput {
    pub body: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DynamicResolverRequest {
    version: u32,
    resolver: String,
    tool: String,
    argument: String,
    value: String,
}

async fn dynamic_resolver(
    axum::Json(request): axum::Json<DynamicResolverRequest>,
) -> (StatusCode, axum::Json<serde_json::Value>) {
    if request.version != 1
        || request.resolver != "email-recipient-readers"
        || request.tool != "send_email"
        || request.argument != "to"
    {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({})));
    }

    let readers = match request.value.as_str() {
        "ap-review@corp.example" => vec!["cfo@corp.example", "ap-lead@corp.example"],
        "finance-all@corp.example" => vec!["cfo@corp.example", "ap-lead@corp.example", "controller@corp.example"],
        recipient => vec![recipient],
    };
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "version": 1, "readers": readers })),
    )
}

pub fn dispatch(world: &World, call: &RenderedCall) -> (StatusCode, String) {
    let tool = call.tool.as_str();
    let Some((system, verb)) = route(tool) else {
        return unknown(tool);
    };
    if !world.enabled.contains(&system) {
        return unknown(tool);
    }
    let root = &world.data_root;

    match (system, verb) {
        (System::Crm, Verb::List) => match list(root, System::Crm) {
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("list_customers failed: {error}"),
            ),
            Ok(records) if records.is_empty() => (StatusCode::OK, "no customer records".to_string()),
            Ok(records) => (
                StatusCode::OK,
                records
                    .into_iter()
                    .map(|(_, body)| body)
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
        },
        (System::Github | System::Finance | System::Meetings, Verb::List) => match list(root, system) {
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("listing {system} failed: {error}"),
            ),
            Ok(records) => {
                let parsed: Vec<serde_json::Value> = records
                    .iter()
                    .filter_map(|(_, body)| serde_json::from_str(body).ok())
                    .collect();
                (StatusCode::OK, format!("{:#}\n", serde_json::Value::Array(parsed)))
            }
        },

        (System::Crm, Verb::Create) => match parse::<CreateCustomerArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => match create(root, System::Crm, &args.customer, &args.content) {
                Ok(()) => (StatusCode::OK, format!("created CRM record {}", args.customer)),
                Err(error) => (write_status(&error), error.to_string()),
            },
        },
        (System::Email, Verb::Create) => match parse::<SendEmailArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => {
                let number = match next_number(root, System::Email, "sent", 0) {
                    Ok(number) => number,
                    Err(error) => return (StatusCode::BAD_GATEWAY, format!("send_email failed: {error}")),
                };
                let record = serde_json::json!({
                    "from": "assistant@corp.example",
                    "to": args.to,
                    "subject": args.subject,
                    "body": args.body,
                });
                match create(root, System::Email, &format!("sent-{number}"), &format!("{record:#}\n")) {
                    Ok(()) => (StatusCode::OK, format!("sent to {}: {:?}", args.to, args.subject)),
                    Err(error) => (write_status(&error), error.to_string()),
                }
            }
        },

        (System::Finance, Verb::Create) => match parse::<TransferArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => {
                let number = match next_number(root, System::Finance, "transfer", 0) {
                    Ok(number) => number,
                    Err(error) => return (StatusCode::BAD_GATEWAY, format!("make_transfer failed: {error}")),
                };
                let record = serde_json::json!({
                    "transfer": number,
                    "amount_usd": args.amount_usd,
                    "to_account": args.to_account,
                    "memo": args.memo,
                    "status": "executed",
                });
                match create(
                    root,
                    System::Finance,
                    &format!("transfer-{number}"),
                    &format!("{record:#}\n"),
                ) {
                    Ok(()) => (
                        StatusCode::OK,
                        format!(
                            "executed transfer #{number}: ${} to {}",
                            args.amount_usd, args.to_account
                        ),
                    ),
                    Err(error) => (write_status(&error), error.to_string()),
                }
            }
        },

        (System::Github, Verb::Create) => match parse::<CreateIssueArgs>(&call.arguments) {
            Err(reason) => (StatusCode::BAD_REQUEST, reason),
            Ok(args) => {
                let number = match next_number(root, System::Github, "issue", 100) {
                    Ok(number) => number,
                    Err(error) => return (StatusCode::BAD_GATEWAY, format!("create_issue failed: {error}")),
                };
                let name = format!("issue-{number}");
                let record = serde_json::json!({
                    "number": number,
                    "title": args.title,
                    "author": "@assistant",
                    "labels": [],
                    "body": args.body,
                });
                let body = format!("{:#}\n", record);
                match create(root, System::Github, &name, &body) {
                    Ok(()) => (StatusCode::OK, format!("published issue #{number}: {:?}", args.title)),
                    Err(error) => (write_status(&error), error.to_string()),
                }
            }
        },

        (System::Email, Verb::List) | (System::Meetings, Verb::Create) => unknown(tool),
    }
}

fn write_status(error: &CreateError) -> StatusCode {
    match error {
        CreateError::Name(_) | CreateError::Exists { .. } => StatusCode::CONFLICT,
        CreateError::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn parse<'a, T: Deserialize<'a>>(arguments: &'a serde_json::Value) -> Result<T, String> {
    T::deserialize(arguments).map_err(|error| format!("bad arguments: {error}"))
}

fn unknown(tool: &str) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("no tool named {tool:?} is enabled"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use appa_engine::value::ToolName;

    fn world(name: &str, enabled: &[System]) -> World {
        let dir = std::env::temp_dir().join(format!("appa-demo-shim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crm")).unwrap();
        std::fs::create_dir_all(dir.join("github")).unwrap();
        World {
            derivations: Arc::new(Derivations::new(String::new(), Default::default())),
            data_root: dir,
            enabled: enabled.iter().copied().collect(),
            approvals: Arc::new(Approvals::default()),
        }
    }

    fn call(tool: &str, arguments: serde_json::Value) -> RenderedCall {
        RenderedCall {
            tool: ToolName::new(tool),
            arguments,
        }
    }

    #[test]
    fn a_disabled_system_has_no_tools() {
        let world = world("disabled", &[System::Crm]);
        let (status, _) = dispatch(&world, &call("list_issues", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = dispatch(&world, &call("frobnicate", serde_json::json!({})));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn list_issues_returns_every_record_as_json() {
        let world = world("list-issues", &[System::Github]);
        std::fs::write(
            world.data_root.join("github/issue-101.json"),
            r#"{"number": 101, "title": "a"}"#,
        )
        .unwrap();
        std::fs::write(
            world.data_root.join("github/issue-102.json"),
            r#"{"number": 102, "title": "b"}"#,
        )
        .unwrap();
        let (status, body) = dispatch(&world, &call("list_issues", serde_json::json!({})));
        assert_eq!(status, StatusCode::OK);
        let issues: Vec<serde_json::Value> = serde_json::from_str(&body).expect("a JSON array");
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0]["number"], 101);
        assert_eq!(issues[1]["title"], "b");
    }

    #[test]
    fn list_customers_returns_the_records_verbatim() {
        let world = world("list-customers", &[System::Crm]);
        std::fs::write(world.data_root.join("crm/acme-corp.md"), "# Acme\nnotes\n").unwrap();
        std::fs::write(world.data_root.join("crm/globex.md"), "# Globex\n").unwrap();
        let (status, body) = dispatch(&world, &call("list_customers", serde_json::json!({})));
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("# Acme\nnotes") && body.contains("# Globex"),
            "got: {body}"
        );
    }

    #[test]
    fn create_issue_numbers_and_publishes_json() {
        let world = world("create-issue", &[System::Github]);
        std::fs::write(world.data_root.join("github/issue-103.json"), "{}").unwrap();
        let (status, body) = dispatch(
            &world,
            &call(
                "create_issue",
                serde_json::json!({"title": "Docs gap", "body": "See above."}),
            ),
        );
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("#104"), "got: {body}");
        let written = std::fs::read_to_string(world.data_root.join("github/issue-104.json")).unwrap();
        let issue: serde_json::Value = serde_json::from_str(&written).expect("a created issue is valid JSON");
        assert_eq!(issue["number"], 104);
        assert_eq!(issue["title"], "Docs gap");
        assert_eq!(issue["body"], "See above.");
        assert_eq!(issue["author"], "@assistant");
    }

    #[test]
    fn a_failed_write_never_answers_two_hundred() {
        let world = world("outcomes", &[System::Crm]);
        std::fs::write(world.data_root.join("crm/acme-corp.md"), "# Acme\n").unwrap();
        let (status, _) = dispatch(
            &world,
            &call(
                "create_customer_data",
                serde_json::json!({"customer": "acme-corp", "content": "overwrite"}),
            ),
        );
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "effects must not commit for a no-op write"
        );
        assert_eq!(
            std::fs::read_to_string(world.data_root.join("crm/acme-corp.md")).unwrap(),
            "# Acme\n"
        );
    }

    #[tokio::test]
    async fn recipient_directory_expands_the_demo_lists() {
        let resolve = |value: &str| DynamicResolverRequest {
            version: 1,
            resolver: "email-recipient-readers".to_string(),
            tool: "send_email".to_string(),
            argument: "to".to_string(),
            value: value.to_string(),
        };

        let (status, axum::Json(answer)) = dynamic_resolver(axum::Json(resolve("ap-review@corp.example"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            answer["readers"],
            serde_json::json!(["cfo@corp.example", "ap-lead@corp.example"])
        );

        let (status, axum::Json(answer)) = dynamic_resolver(axum::Json(resolve("finance-all@corp.example"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            answer["readers"],
            serde_json::json!(["cfo@corp.example", "ap-lead@corp.example", "controller@corp.example"])
        );

        let (status, axum::Json(answer)) = dynamic_resolver(axum::Json(resolve("person@corp.example"))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["readers"], serde_json::json!(["person@corp.example"]));
    }

    #[tokio::test]
    async fn recipient_directory_refuses_unknown_bindings() {
        let request = DynamicResolverRequest {
            version: 2,
            resolver: "email-recipient-readers".to_string(),
            tool: "send_email".to_string(),
            argument: "to".to_string(),
            value: "ap-review@corp.example".to_string(),
        };
        let (status, _) = dynamic_resolver(axum::Json(request)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_traversing_name_never_reaches_the_filesystem() {
        let world = world("traversal", &[System::Crm]);
        let (status, body) = dispatch(
            &world,
            &call(
                "create_customer_data",
                serde_json::json!({"customer": "../../etc/passwd", "content": "x"}),
            ),
        );
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("invalid name"), "got: {body}");
    }

    #[tokio::test]
    async fn an_underivable_sanitizer_fails_closed_rather_than_passing_the_raw_through() {
        let world = Arc::new(world("derive", &[System::Crm]));
        let (status, body) = sanitize(
            State(world),
            Path("nobody-registered-this".to_string()),
            axum::Json(SanitizerInput {
                body: "ask eve@corp.com".to_string(),
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(body.0.get("body").is_none(), "no derivation is offered: {:?}", body.0);
        assert!(
            !serde_json::to_string(&body.0).unwrap().contains("eve@corp.com"),
            "the raw never rides out on the failure path"
        );
    }
}
