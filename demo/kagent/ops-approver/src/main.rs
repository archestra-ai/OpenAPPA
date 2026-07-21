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
use serde::Deserialize;
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

/// Strict mirrors of the wire shapes appa-core serializes for a grant —
/// mirrors, never the capability itself (a `PendingApproval` cannot be
/// deserialized; out of process it is evidence to read). An externally
/// tagged coordinate this enum does not name, an out-of-range payload, or
/// an unknown field anywhere is a deserialization error → 422, never an
/// approval.
// Mirror payloads exist to *validate* the wire shape, not to be read —
// dead-code lints on their fields are the point, not an oversight.
#[allow(dead_code)]
#[derive(Deserialize)]
enum WireCoordinate {
    RaiseLabel(WireRaise),
    ExceptPriorEffects(Value),
    StandInConfirmation,
    ReleaseControl(Vec<u64>),
    AcknowledgeUnknown(Value),
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRaise {
    // serde reads an absent Option field as null. Core always serializes
    // both fields, but tolerating absence here is harmless in the safe
    // direction: a RaiseLabel can only ever deny, never approve.
    trust: Option<WireKnownTrust>,
    audience: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum WireKnownTrust {
    Trusted,
    Suspicious,
}

#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
enum WireScope {
    DerivedValue { source: u64 },
    PolicyCheck { flow: u64 },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGrant {
    delta: Vec<WireCoordinate>,
    scope: WireScope,
}

async fn rule(Json(approval): Json<Value>) -> Result<Json<Value>, axum::http::StatusCode> {
    // Rule only on well-formed typed facts. A body without this authority's
    // own name, an ancestry snapshot, and a strictly-parsed grant is not an
    // approval; answering it with a ruling would be ruling on nothing. A
    // non-2xx is a non-ruling to the proxy — the flow stays blocked.
    let Some(authority) = approval["authority"].as_str() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    if authority != "ops-approver" {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }
    let Some(values) = approval["ancestry"]["values"].as_object() else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    let Ok(grant) = serde_json::from_value::<WireGrant>(approval["grant"].clone()) else {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    };
    if grant.delta.is_empty() {
        return Err(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }

    let trust_raise = grant
        .delta
        .iter()
        .any(|c| matches!(c, WireCoordinate::RaiseLabel(WireRaise { trust: Some(_), .. })));
    let release_only = grant
        .delta
        .iter()
        .all(|c| matches!(c, WireCoordinate::ReleaseControl(deps) if !deps.is_empty()))
        && matches!(grant.scope, WireScope::PolicyCheck { .. });
    let ruling = if trust_raise {
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
    } else if release_only {
        json!({
            "ruling": "approve",
            "reason": "the grant releases control dependencies only: every value flowing is within its declared \
                       bounds; the choice was informed by third-party text, and releasing that dependency is this \
                       authority's mandate",
        })
    } else {
        // Recognized-but-unexpected coordinates or scopes: not a grant this
        // rule covers. Refuse to rule rather than approve by fallback.
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
            {"ReleaseControl": [1, 3]},
        ]);
        assert_eq!(decide(approval(delta)).await.unwrap()["ruling"], "deny");
    }

    #[tokio::test]
    async fn a_release_control_only_delta_is_approved() {
        let ruling = decide(approval(json!([{"ReleaseControl": [1, 3]}]))).await.unwrap();
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
            json!([{"AcquireEffects": {"Has": ["Egress"]}}]),            // retired coordinate
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

    #[tokio::test]
    async fn foreign_authorities_wrong_scopes_and_degenerate_releases_are_refused() {
        // A body naming another authority is not this service's to rule on.
        let mut foreign = approval(json!([{"ReleaseControl": [1]}]));
        foreign["authority"] = json!("someone-else");
        assert!(decide(foreign).await.is_err());
        // A release grant is check-scoped by construction; any other scope
        // shape is not the expected grant.
        let mut wrong_scope = approval(json!([{"ReleaseControl": [1]}]));
        wrong_scope["grant"]["scope"] = json!({"DerivedValue": {"source": 4}});
        assert!(decide(wrong_scope).await.is_err());
        let mut forged_scope = approval(json!([{"ReleaseControl": [1]}]));
        forged_scope["grant"]["scope"] = json!("forged");
        assert!(decide(forged_scope).await.is_err());
        // Degenerate release sets: nothing released, or non-id elements.
        assert!(decide(approval(json!([{"ReleaseControl": []}]))).await.is_err());
        assert!(decide(approval(json!([{"ReleaseControl": [null]}]))).await.is_err());
        // Unknown fields inside the grant, its scope, or a raise are wire
        // drift, not evidence.
        let mut extra = approval(json!([{"ReleaseControl": [1]}]));
        extra["grant"]["extra"] = json!(1);
        assert!(decide(extra).await.is_err());
        let mut scope_extra = approval(json!([{"ReleaseControl": [1]}]));
        scope_extra["grant"]["scope"] = json!({"PolicyCheck": {"flow": 7, "extra": 1}});
        assert!(decide(scope_extra).await.is_err());
        // A raise with an absent (not null) audience field still reads as a
        // trust raise — and a raise can only deny, never approve.
        let ruling = decide(approval(json!([{"RaiseLabel": {"trust": "Trusted"}}])))
            .await
            .unwrap();
        assert_eq!(ruling["ruling"], "deny");
    }
}
