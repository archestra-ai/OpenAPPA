//! `ops-approver`: the kagent demo's external authority, served over HTTP.
//!
//! appa-proxy POSTs a pending approval here when a flow needs the
//! `ops-approver` authority's grant. The body is the engine's typed facts —
//! the authority name, the exact grant, the violations it targets, and the
//! ancestry snapshot (labels and provenance, never value bytes). The ruling
//! is one deterministic rule over those facts:
//!
//! - **deny** when any value in the ancestry is suspicious-tainted — the
//!   flow derives from third-party text, and no paraphrase can talk around
//!   what the provenance shows;
//! - **approve** otherwise.
//!
//! The reason always names the evidence. This is the demo thesis in one
//! rule: the authority judges engine-supplied provenance, not the model's
//! story. The approval arrives as untyped JSON on purpose — a
//! `PendingApproval` cannot be deserialized (that would forge core's
//! linearity); out of process it is evidence to read, never a capability.

use std::net::SocketAddr;

use axum::Json;
use axum::routing::post;
use clap::Parser;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "External approver for the OpenAPPA kagent demo")]
struct Args {
    #[arg(long, env = "OPS_APPROVER_ADDR", default_value = "0.0.0.0:8732")]
    addr: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let addr: SocketAddr = args.addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(addr = %listener.local_addr()?, "ops-approver listening");

    let app = axum::Router::new().route("/rule", post(rule));
    axum::serve(listener, app).await?;
    Ok(())
}

fn is_suspicious(trust: &Value) -> bool {
    trust == &json!({"Known": "Suspicious"})
}

async fn rule(Json(approval): Json<Value>) -> Result<Json<Value>, axum::http::StatusCode> {
    // Rule only on well-formed typed facts. A body without a named authority
    // and an ancestry snapshot is not an approval; answering it with a
    // ruling would be ruling on nothing. A non-2xx is a non-ruling to the
    // proxy — the flow stays blocked, fail closed.
    let Some(authority) = approval["authority"].as_str() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    let Some(values) = approval["ancestry"]["values"].as_object() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    let suspicious: Vec<&str> = values
        .iter()
        .filter(|(_, view)| is_suspicious(&view["label"]["trust"]))
        .map(|(id, _)| id.as_str())
        .collect();

    let ruling = if suspicious.is_empty() {
        json!({
            "ruling": "approve",
            "reason": "no suspicious values in the flow's provenance",
        })
    } else {
        json!({
            "ruling": "deny",
            "reason": format!(
                "flow provenance includes suspicious values ({}): third-party text may not drive this action",
                suspicious.join(", ")
            ),
        })
    };
    tracing::info!(
        authority,
        ruling = ruling["ruling"].as_str().unwrap_or("?"),
        reason = ruling["reason"].as_str().unwrap_or("?"),
        "ruled"
    );
    Ok(Json(ruling))
}
