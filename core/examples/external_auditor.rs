//! Read this quarter's invoices from the internal system (readable only by the
//! finance team), then e-mail the report to an *external auditor* who is **not**
//! a reader of that data. The send crosses the audience boundary, so OpenAPPA
//! routes it to a mandated sign-off that *declassifies* it (endorses the
//! auditor in), leaving an audit record, rather than letting it out silently.

use appa_core::{
    ArgumentTree, Authority, AuthorityMandate, Authorization, FlowOutcome, OpaqueValue, PolicyEngine, Pursuit, Ruling,
    Speaker, ToolContract, ToolName, ToolRequest, Trajectory, TrajectoryView, UserId, ValueId, ValueLabel, Violation,
};

const ALICE: &str = "alice@archestra.ai";
const BOB: &str = "bob@archestra.ai";
const AUDITOR: &str = "alex@finance-audit.com";

fn u(id: &str) -> UserId {
    UserId::new(id)
}

fn approve_auditor(_: &Authorization, _: &[Violation], _: &TrajectoryView<'_>) -> Option<Ruling> {
    Some(Ruling::Approve {
        reason: "approved sending financials to the external auditor".to_owned(),
    })
}

fn finance_approver() -> Authority {
    Authority::inline(
        "finance-approver",
        AuthorityMandate::none().vouch_audience([u(AUDITOR)]),
        approve_auditor,
    )
}

fn build_engine() -> PolicyEngine {
    let mut engine = PolicyEngine::new();
    engine
        .register(ToolContract::source(
            "invoices.list",
            ValueLabel::trusted_readers([u(ALICE), u(BOB)]),
        ))
        .unwrap();
    engine.register(ToolContract::egress_sink("email.send", "to")).unwrap();
    engine.register_authority(finance_approver()).unwrap();
    engine
}

fn main() {
    let engine = build_engine();
    let mut trajectory = Trajectory::new();
    trajectory.ingress(
        Speaker::user(u(ALICE)),
        ValueLabel::identity(),
        OpaqueValue::new("Pull this quarter's invoices, summarize them, and send the report to our external auditor."),
    );

    // Read the invoices; the output wears the finance team's audience.
    let list = ToolRequest::new(ToolName::new("invoices.list"), ArgumentTree::empty(), []);
    let report = match engine.evaluate(&mut trajectory, list) {
        Ok(FlowOutcome::AllowedNow(token)) => {
            let (_canonical, receipt) = trajectory.release(token).unwrap();
            trajectory
                .record_output(receipt, OpaqueValue::new("<47 invoices totaling $1.2M>"))
                .unwrap()
        }
        other => unreachable!("the read-only invoice list has no requirements, got {other:?}"),
    };
    println!(
        "invoices are internal-only ({{{ALICE}, {BOB}}}); the auditor {AUDITOR} is outside the audience.\n\
         report wears {}",
        trajectory.value(report).unwrap().label()
    );

    let send = email(&mut trajectory, report, AUDITOR);
    print!("  email → {AUDITOR}: ");
    match engine.pursue(&mut trajectory, send, 8) {
        Pursuit::Permitted(token) => {
            let (_canonical, receipt) = trajectory.release(token).unwrap();
            trajectory
                .record_output(receipt, OpaqueValue::new("message-id: 1"))
                .unwrap();
            println!("PERMITTED (finance approver endorsed the auditor)");
        }
        Pursuit::Terminal { reason, .. } => println!("BLOCKED — {reason}"),
        other => println!("BLOCKED — {other:?}"),
    }

    println!("\naudit trail:");
    for event in trajectory.audit() {
        println!("   * {event}");
    }
}

fn email(trajectory: &mut Trajectory, report: ValueId, recipient: &str) -> ToolRequest {
    let to = trajectory.ingress(
        Speaker::user(u(ALICE)),
        ValueLabel::identity(),
        OpaqueValue::new(recipient),
    );
    ToolRequest::new(
        ToolName::new("email.send"),
        ArgumentTree::object([("to", to), ("body", report)]),
        [],
    )
}
