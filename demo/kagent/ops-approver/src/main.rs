//! `ops-approver`: the kagent demo's external authority, served over HTTP.
//!
//! appa-proxy POSTs a pending approval here when a flow needs the
//! `ops-approver` authority's grant. The body is the engine's typed facts —
//! the authority name, the exact grant (its delta of typed coordinates), the
//! violations it targets, and the ancestry snapshot (labels and provenance,
//! never value bytes). The ruling is deterministic over the grant's typed
//! coordinates, fail-closed on every branch:
//!
//! - **deny** any delta containing a trust-bearing `RaiseLabel` — the grant
//!   asks to vouch third-party-derived text as trusted, and no paraphrase
//!   can talk around what the coordinates say;
//! - **approve** only a fully validated, release-control-only delta — the
//!   values flowing are within their declared bounds; only the *choice* was
//!   informed by third-party text, and releasing that dependency is exactly
//!   what this authority is mandated for;
//! - anything else — a missing, empty, or malformed delta, an unrecognized
//!   or unexpected coordinate shape — is a **422**, never an approval. A
//!   non-2xx is a non-ruling to the proxy: the flow stays blocked. An
//!   "approve otherwise" fallback would be fail-open under wire drift.
//!
//! This is the demo thesis in one rule: the authority judges engine-supplied
//! typed facts, not the model's story. The approval arrives as untyped JSON
//! on purpose — a `PendingApproval` cannot be deserialized (that would forge
//! core's linearity); out of process it is evidence to read, never a
//! capability.

use std::net::SocketAddr;

use axum::Json;
use axum::routing::post;
use clap::Parser;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "External approver for the OpenAPPA kagent demo")]
struct Args {
    /// Address to listen on.
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

/// One grant coordinate, classified. Everything not explicitly recognized —
/// and everything recognized but outside this authority's two expected grant
/// shapes — is `Unrulable`.
enum Coordinate {
    /// `{"RaiseLabel": {"trust": "Trusted"|"Suspicious", ...}}`
    TrustRaise,
    /// `{"ReleaseControl": [value ids]}`
    ReleaseControl,
    Unrulable,
}

/// Classify one wire coordinate against appa-core's `DeltaCoordinate`
/// encoding: externally tagged, one variant key per object.
fn classify(coordinate: &Value) -> Coordinate {
    let Some(object) = coordinate.as_object() else {
        // Unit variants (`"StandInConfirmation"`) land here: recognized in
        // the encoding, but not a grant this authority expects to rule on.
        return Coordinate::Unrulable;
    };
    if object.len() != 1 {
        return Coordinate::Unrulable;
    }
    match (object.get("RaiseLabel"), object.get("ReleaseControl")) {
        (Some(raise), None) => match raise.get("trust") {
            Some(Value::String(_)) => Coordinate::TrustRaise,
            // An audience-only (or malformed) raise is not a trust raise,
            // but it is not something this authority approves either.
            _ => Coordinate::Unrulable,
        },
        (None, Some(deps)) if deps.is_array() => Coordinate::ReleaseControl,
        _ => Coordinate::Unrulable,
    }
}

async fn rule(Json(approval): Json<Value>) -> Result<Json<Value>, axum::http::StatusCode> {
    // Rule only on well-formed typed facts. A body without a named
    // authority, an ancestry snapshot, and a non-empty grant delta is not an
    // approval; answering it with a ruling would be ruling on nothing. A
    // non-2xx is a non-ruling to the proxy — the flow stays blocked.
    let Some(authority) = approval["authority"].as_str() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    let Some(values) = approval["ancestry"]["values"].as_object() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    let Some(delta) = approval["grant"]["delta"].as_array() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    if delta.is_empty() {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    let coordinates: Vec<Coordinate> = delta.iter().map(classify).collect();
    let ruling = if coordinates.iter().any(|c| matches!(c, Coordinate::TrustRaise)) {
        let suspicious: Vec<&str> = values
            .iter()
            .filter(|(_, view)| view["label"]["trust"] == json!({"Known": "Suspicious"}))
            .map(|(id, _)| id.as_str())
            .collect();
        let reason = if suspicious.is_empty() {
            "the grant asks to vouch a value as trusted; this authority does not vouch content it cannot audit"
                .to_string()
        } else {
            format!(
                "the grant asks to vouch a value as trusted; the flow's provenance includes suspicious values ({}): \
                 third-party text may not drive this action",
                suspicious.join(", ")
            )
        };
        json!({ "ruling": "deny", "reason": reason })
    } else if coordinates.iter().all(|c| matches!(c, Coordinate::ReleaseControl)) {
        json!({
            "ruling": "approve",
            "reason": "the grant releases control dependencies only: every value flowing is within its declared \
                       bounds; the choice was informed by third-party text, and releasing that dependency is this \
                       authority's mandate",
        })
    } else {
        // Recognized-but-unexpected or unrecognized coordinates: not a grant
        // this rule covers. Refuse to rule rather than approve by fallback.
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };

    tracing::info!(
        authority,
        ruling = ruling["ruling"].as_str().unwrap_or("?"),
        reason = ruling["reason"].as_str().unwrap_or("?"),
        "ruled"
    );
    Ok(Json(ruling))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the handler with a body and return `Ok(ruling)` or the status.
    async fn decide(approval: Value) -> Result<Value, axum::http::StatusCode> {
        rule(Json(approval)).await.map(|Json(v)| v)
    }

    /// A well-formed approval around the given delta, with one suspicious
    /// value in the ancestry — the exact wire shapes appa-core serializes
    /// (`DeltaCoordinate` externally tagged; trust as `{"Known": ...}`).
    fn approval(delta: Value) -> Value {
        json!({
            "authority": "ops-approver",
            "grant": { "delta": delta, "scope": { "PolicyCheck": { "flow": 7 } } },
            "ancestry": { "values": {
                "v3": { "label": { "trust": {"Known": "Suspicious"}, "audience": {"Readers": ["operator"]} } },
                "v9": { "label": { "trust": {"Known": "Trusted"}, "audience": {"Readers": ["operator"]} } },
            }},
        })
    }

    #[tokio::test]
    async fn a_trust_raise_is_denied_naming_the_suspicious_evidence() {
        let ruling = decide(approval(
            json!([{"RaiseLabel": {"trust": "Trusted", "audience": null}}]),
        ))
        .await
        .unwrap();
        assert_eq!(ruling["ruling"], "deny");
        assert!(ruling["reason"].as_str().unwrap().contains("v3"));
    }

    #[tokio::test]
    async fn a_product_containing_a_trust_raise_is_denied() {
        let delta = json!([
            {"RaiseLabel": {"trust": "Trusted", "audience": null}},
            {"ReleaseControl": ["v1", "v3"]},
        ]);
        assert_eq!(decide(approval(delta)).await.unwrap()["ruling"], "deny");
    }

    #[tokio::test]
    async fn a_release_control_only_delta_is_approved() {
        let ruling = decide(approval(json!([{"ReleaseControl": ["v1", "v3"]}])))
            .await
            .unwrap();
        assert_eq!(ruling["ruling"], "approve");
        assert!(
            ruling["reason"]
                .as_str()
                .unwrap()
                .contains("releases control dependencies only")
        );
    }

    #[tokio::test]
    async fn everything_else_is_a_422_never_an_approval() {
        for delta in [
            json!([]),                                                   // empty delta
            json!(["StandInConfirmation"]),                              // recognized, not expected
            json!([{"AcknowledgeUnknown": []}]),                         // recognized, not expected
            json!([{"RaiseLabel": {"trust": null, "audience": ["x"]}}]), // audience-only raise
            json!([{"Forged": 1}]),                                      // unrecognized
            json!([{"ReleaseControl": "not-an-array"}]),                 // malformed payload
            json!([{"ReleaseControl": ["v1"], "RaiseLabel": {}}]),       // two tags in one object
        ] {
            let status = decide(approval(delta.clone())).await.expect_err("must refuse");
            assert_eq!(
                status,
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "delta {delta} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn malformed_envelopes_are_refused() {
        for body in [
            json!({}),
            json!({"authority": "ops-approver"}),
            json!({"authority": "ops-approver", "ancestry": {"values": {}}}),
        ] {
            assert!(decide(body).await.is_err());
        }
    }
}
