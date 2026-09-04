//! The shipped appa-guide skill is one composable package: a host-routing
//! SKILL.md and one reference file per host. These checks keep the package
//! whole — the router routing, each reference carrying its host's flow, and
//! the kagent chart consuming this same package rather than a second one.

mod common;
use common::repo_root;

use std::fs;

fn skill_dir() -> std::path::PathBuf {
    repo_root().join("integrations/appa-guide")
}

fn read(name: &str) -> String {
    let path = skill_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn the_router_routes_by_host_and_carries_the_shared_rules() {
    let router = read("SKILL.md");
    assert!(router.starts_with("---\n"), "the router keeps its frontmatter");
    assert!(router.contains("name: appa-guide"));
    assert!(router.contains("references/claude-code.md"));
    assert!(router.contains("references/kagent.md"));
    assert!(
        router.contains("k8s_get_resources"),
        "the router detects the kagent host"
    );
    assert!(
        router.contains("Do not call `Read`"),
        "Claude bootstraps without a gated tool call"
    );
    assert!(router.contains("`init`") && router.contains("`adjust`"));
    for shared in [
        "Never edit a battery",
        "OpenAPPA pieces",
        "smallest change",
        "wait for approval",
    ] {
        assert!(
            router.contains(shared),
            "the shared rule {shared:?} lives in the router"
        );
    }
    // Host mechanics stay out of the router.
    for host_only in ["claude mcp list", "status.discoveredTools", "known_marketplaces.json"] {
        assert!(!router.contains(host_only), "{host_only:?} belongs to a reference file");
    }
}

#[test]
fn the_claude_code_reference_carries_the_full_flow() {
    let reference = read("references/claude-code.md");
    for marker in [
        "appa describe --config",
        "known_marketplaces.json",
        "claude mcp list",
        "mcp__<server>__<tool>",
        "batteries/<name>/",
        "website/content/docs/contracts.md",
        "http://127.0.0.1:8787",
        "clappa",
        "Approve, or tell me what to change.",
    ] {
        assert!(reference.contains(marker), "the claude-code flow names {marker:?}");
    }
}

#[test]
fn the_kagent_reference_carries_the_full_flow() {
    let reference = read("references/kagent.md");
    for marker in [
        "status.discoveredTools",
        "k8s_get_resource_yaml",
        "k8s_apply_manifest",
        "__NS__",
        "Approve/Reject card",
        "/reload",
        "kubelet syncs",
        "Read-only fallback",
        "Approve, or tell me what to change.",
    ] {
        assert!(reference.contains(marker), "the kagent flow names {marker:?}");
    }
    for claude_only in ["claude mcp list", "clappa", "marketplace-root", "APPA_GATE"] {
        assert!(
            !reference.contains(claude_only),
            "{claude_only:?} is claude-code machinery"
        );
    }
}

#[test]
fn the_kagent_chart_consumes_this_skill_package() {
    let chart = repo_root().join("integrations/kagent/demo/chart");
    let guide = fs::read_to_string(chart.join("templates/guide.yaml")).expect("the chart renders the guide agent");
    assert!(
        guide.contains("gitRefs"),
        "the agent attaches the skill through git refs"
    );
    for tool in ["k8s_get_resources", "k8s_apply_manifest", "k8s_execute_command"] {
        assert!(guide.contains(tool), "the guide agent carries {tool}");
    }
    assert!(guide.contains("APPA_RUNTIME_URL"));

    let values = fs::read_to_string(chart.join("values.yaml")).expect("the chart values exist");
    assert!(values.contains("integrations/appa-guide"));

    let policy = fs::read_to_string(chart.join("files/demo.appa.toml")).expect("the demo policy exists");
    assert!(policy.contains("name = \"k8s_apply_manifest\""));
    assert!(policy.contains("attention = [\"human-approval\"]"));
    assert!(policy.contains("name = \"appa-guide\""));
    assert!(
        !policy.contains("name = \"bash\""),
        "the unused skill helpers stay undeclared"
    );
}

#[test]
fn the_claude_plugin_has_no_second_source_copy() {
    assert!(
        !repo_root()
            .join("integrations/claude-code/plugin/skills/appa-guide")
            .exists(),
        "the Claude plugin materializes the canonical skill at staging time; a source copy would drift"
    );
}
