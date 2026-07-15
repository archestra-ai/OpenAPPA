//! Fetch a Grain recording (readable only by the internal team), then open an
//! issue on a *public* GitHub repo. A public issue egresses to `world` — a
//! sentinel recipient standing for the public (there is no e-mail for
//! "everyone"; see the design notes).

use baton_core::{
    ArgumentTree, Blocked, Decision, OpaqueValue, PolicyEngine, Speaker, ToolContract, ToolName, ToolRequest,
    Trajectory, UserId, ValueId, ValueLabel,
};

const ALICE: &str = "alice@archestra.ai";
const BOB: &str = "bob@archestra.ai";
const WORLD: &str = "world";

fn u(id: &str) -> UserId {
    UserId::new(id)
}

fn build_engine() -> PolicyEngine {
    let mut engine = PolicyEngine::new();
    engine
        .register(ToolContract::source(
            "grain.fetch",
            ValueLabel::trusted_readers([u(ALICE), u(BOB)]),
        ))
        .unwrap();
    engine
        .register(ToolContract::egress_sink("github.open_issue", "to"))
        .unwrap();
    engine
}

fn main() {
    let engine = build_engine();
    let mut trajectory = Trajectory::new();
    trajectory.ingress(
        Speaker::user(u(ALICE)),
        ValueLabel::identity(),
        OpaqueValue::new("Skim the latest customer call and open a bug for the crash they hit."),
    );

    // Fetch the recording; the output wears the internal team's audience.
    let fetch = ToolRequest::new(ToolName::new("grain.fetch"), ArgumentTree::empty(), []);
    let recording = match engine.evaluate(&mut trajectory, fetch) {
        Decision::Permitted(token) => {
            let (_canonical, receipt) = trajectory.release(token).unwrap();
            trajectory
                .record_output(
                    receipt,
                    OpaqueValue::new("<transcript: names the customer's staging host>"),
                )
                .unwrap()
        }
        other => unreachable!("the read-only recording fetch has no requirements, got {other:?}"),
    };
    println!(
        "recording is internal-only ({{{ALICE}, {BOB}}}); a public issue egresses to `{WORLD}`.\n\
         recording wears {}",
        trajectory.value(recording).unwrap().label()
    );

    let open = open_issue(&mut trajectory, recording, WORLD);
    print!("  open public issue → {WORLD}: ");
    match engine.evaluate(&mut trajectory, open) {
        Decision::Permitted(_) => println!("PERMITTED (unexpected: no authority is mandated)"),
        Decision::Blocked(Blocked::Terminal(block)) => println!("BLOCKED — {}", block.reason),
        Decision::Blocked(Blocked::Remediable { .. }) => {
            println!("BLOCKED — remediable, but no registered authority can clear it")
        }
    }

    println!("\naudit trail:");
    for event in trajectory.state().audit() {
        println!("   * {event}");
    }
}

fn open_issue(trajectory: &mut Trajectory, recording: ValueId, recipient: &str) -> ToolRequest {
    let to = trajectory.ingress(
        Speaker::user(u(ALICE)),
        ValueLabel::identity(),
        OpaqueValue::new(recipient),
    );
    ToolRequest::new(
        ToolName::new("github.open_issue"),
        ArgumentTree::object([("to", to), ("body", recording)]),
        [],
    )
}
