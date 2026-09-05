//! The shipped appa-guide skill is one composable package: a host-routing
//! SKILL.md and one reference file per host. These checks keep the package
//! whole: the router routes, the kagent reference uses only the shared remote
//! runtime, and the chart consumes this package rather than a second skill.

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
    assert!(router.contains("/skills/appa-guide/references/kagent.md"));
    assert!(router.contains("`offset: 1`") && router.contains("`limit: 0`"));
    assert!(
        router.contains("k8s_get_resources"),
        "the router detects the kagent host"
    );
    assert!(
        router.contains("Do not call `Read`"),
        "Claude bootstraps without a gated tool call"
    );
    assert!(router.contains("`init`") && router.contains("`adjust`"));
    assert!(router.contains("Do not ask whether to continue before the proposal"));
    for shared in [
        "Never edit a battery",
        "OpenAPPA pieces",
        "smallest change",
        "wait for approval",
        "reload an unchanged config",
        "start a new chat when nothing changed",
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
        "same fetched Pod YAML",
        "/batteries",
        "runtime mode is the only supported deployment",
        "http://appa-runtime.<namespace>.svc.cluster.local:18787",
        "Replace only `name`",
        "PersistentVolumeClaim",
        "kubelet sync",
        "Read-only fallback",
        "Approve, or tell me what to change.",
        "## Cluster operations",
        "helm_upgrade",
        "Protect all Agents",
        "appa-guide-inspect",
        "appa-guide-reload",
        "Required init checklist",
        "Never construct a pod name",
        "List every `RemoteMCPServer`",
        "server not yet attached to an Agent",
        "appa-guide-refresh-check",
        "kagent 0.9.12 sends `k8s_execute_command.command` as one executable",
    ] {
        assert!(reference.contains(marker), "the kagent flow names {marker:?}");
    }
    for stale in ["APPA_CONFIG_CONTENTS", "Bundled mode", "127.0.0.1:8787"] {
        assert!(!reference.contains(stale), "{stale:?} is not a supported kagent mode");
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
    for tool in [
        "k8s_get_resources",
        "k8s_apply_manifest",
        "k8s_patch_resource",
        "k8s_execute_command",
        "helm_upgrade",
    ] {
        assert!(guide.contains(tool), "the guide agent carries {tool}");
    }
    assert!(guide.contains("APPA_RUNTIME_URL"));
    assert!(guide.contains("/skills/appa-guide/references/kagent.md"));
    assert!(guide.contains("offset 1") && guide.contains("limit 0"));
    assert!(guide.contains("without asking whether"));

    let values = fs::read_to_string(chart.join("values.yaml")).expect("the chart values exist");
    assert!(values.contains("integrations/appa-guide"));

    let policy = fs::read_to_string(chart.join("files/demo.appa.toml")).expect("the demo policy exists");
    assert!(policy.contains("name = \"k8s_apply_manifest\""));
    assert!(policy.contains("attention = [\"human-approval\"]"));
    assert!(policy.contains("name = \"skills\""));
    assert!(
        !policy.contains("name = \"bash\""),
        "the unused skill helpers stay undeclared"
    );
}

#[test]
fn kagent_guidance_requires_the_shared_runtime_and_direct_port() {
    let root = repo_root();
    assert!(
        !root.join("integrations/kagent/appa-kagent-quickstart").exists(),
        "kagent has no bundled-runtime image"
    );
    for path in [
        "website/content/docs/kagent.md",
        "integrations/kagent/README.md",
        "integrations/kagent/IMPLEMENTATION.md",
        "integrations/appa-guide/references/kagent.md",
        "integrations/kagent/examples/kagent.appa.toml",
    ] {
        let content = fs::read_to_string(root.join(path)).expect("read kagent guidance");
        assert!(content.contains("APPA_RUNTIME_URL"), "{path} names the runtime URL");
        for stale in [
            "APPA_CONFIG_CONTENTS",
            "appa-kagent-quickstart",
            "Bundled mode",
            "127.0.0.1:8787",
            "18789",
            "relay",
        ] {
            assert!(!content.contains(stale), "{path} retains stale {stale:?} guidance");
        }
    }

    let website = fs::read_to_string(root.join("website/content/docs/kagent.md")).expect("read website guide");
    assert!(website.contains("http://appa-runtime.appa.svc.cluster.local:18787"));
    assert!(website.contains("appaGuide.enabled=true"));
    assert!(website.contains("appa-kagent-adk"));
    assert!(website.contains("archestra-ai/golang-adk"));
}

#[test]
fn kagent_exec_is_annotated_by_exact_guide_operation() {
    let root = repo_root();
    for path in [
        "charts/appa-runtime/files/appa.toml",
        "integrations/kagent/demo/chart/files/demo.appa.toml",
    ] {
        let policy = fs::read_to_string(root.join(path)).expect("read kagent policy");
        let command = policy
            .split("[[policy.tool]]")
            .find(|entry| entry.contains("name = \"k8s_execute_command\""))
            .expect("kagent policy declares command execution");
        assert!(command.contains("annotator = \"appa-guide-command\""));
        assert!(policy.contains("marks = [\"human-approval\"]"));
        assert!(policy.contains("/usr/local/bin/appa-guide-command-annotator"));
    }

    let reference = read("references/kagent.md");
    assert!(reference.contains("`appa-guide-inspect` | Reads"));
    assert!(reference.contains("Exact read-only inspection needs no approval"));
    for operation in [
        "appa-guide-reload",
        "appa-guide-refresh-check",
        "appa-guide-refresh-stage",
        "appa-guide-refresh-commit",
        "appa-guide-refresh-rollback",
    ] {
        assert!(reference.contains(operation), "the reference names {operation}");
    }
    assert!(reference.contains("Every Kubernetes resource write, restart or rollout, Helm mutation"));
    assert!(reference.contains("runtime reload must cross the `human-approval` authority"));
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
