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
        "invent an offer id",
        "nothing needs applying",
        "Say \"include\" rather than \"install\"",
        "awaiting approval to propose",
        "exactly one state",
        "Keep user-facing replies compact",
        "If the request says `diagnose` and `inspect only`",
        "Do not narrate inspection calls",
        "including `appa_update_policy`",
        "`appa-guide-*` executable",
        "runtime-owned `execute_remedy_plan` and",
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
        "Never say the card remains open",
        "runtime mode is the only supported deployment",
        "http://appa-runtime.<namespace>.svc.cluster.local:18787",
        "Replace only `name`",
        "PersistentVolumeClaim",
        "Read-only fallback",
        "Approve, or tell me what to change.",
        "## Cluster operations",
        "helm_upgrade",
        "Protect all Agents",
        "appa_get_runtime_state",
        "appa_match_batteries",
        "appa_include_battery",
        "appa_update_policy",
        "appa_reload_policy",
        "appa_refresh_batteries",
        "one-shot APPA",
        "Required init checklist",
        "List every `RemoteMCPServer`",
        "server not yet attached to an Agent",
        "untrusted proposal input",
        "public `appa-kagent-demo` OCI chart",
        "must own only its",
        "Never use a live ConfigMap as the",
        "A demo template is never serving",
        "any other word as an offer id",
        "Claude-spelled names",
        "Battery matches: none.",
        "Environment variables alone never prove the gate",
        "Raw events and",
        "lowercase singular resource types",
        "Helm values; provider credentials",
        "memory prefetch enters model",
        "Go remote-Agent",
        "Static contracts need no audience source",
        "does not require a person by default",
        "explicitly named proposal",
        "Never claim fleet-wide coverage",
        "runtime namespace by default",
        "Never patch the generated Deployment",
        "without proposing a change or asking",
        "This overrides every proposal",
        "If all are present, never propose the demo template",
        "whole reply below 1,600 characters",
        "## Reconcile batteries",
        "Suggested includes",
        "A refresh never includes a battery",
        "Do not precede it with an inspection summary",
        "Do not append a second summary",
        "exactly match qualified declarations",
        "no exact alias exists",
        "Its `matches` array is the only source",
        "`included` boolean is the only source",
        "`unconfigured_tools` array is the only source",
        "source: <namespace>/delegations",
        "ascending discovered-tool count",
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
fn only_the_runtime_chart_consumes_this_skill_package() {
    let root = repo_root();
    let chart = root.join("charts/appa-runtime");
    let guide =
        fs::read_to_string(chart.join("templates/appa-guide.yaml")).expect("the runtime chart renders the guide agent");
    assert!(
        guide.contains("gitRefs"),
        "the agent attaches the skill through git refs"
    );
    for tool in ["k8s_get_resources", "k8s_apply_manifest", "helm_upgrade"] {
        assert!(guide.contains(tool), "the guide agent carries {tool}");
    }
    assert!(!guide.contains("- k8s_patch_resource"));
    assert!(guide.contains("APPA_RUNTIME_URL"));
    assert!(guide.contains("/skills/appa-guide/references/kagent.md"));
    for marker in [
        "Runtime management uses only direct runtime-owned MCP tools",
        "appa_get_runtime_state reads serving policy",
        "appa_include_battery updates the complete root policy and reloads it",
        "appa_update_policy publishes one complete approved root policy and reloads it",
        "Never use Kubernetes tools, shell commands, helper executables",
        "Pass the policy key from appa_get_runtime_state",
        "matches, included, and unconfigured_tools fields",
        "match it",
        "A request is never approval",
        "Never invent or request an offer id",
        "Protect an existing Agent only with k8s_apply_manifest",
        "Never patch a generated Deployment",
        "If the request says diagnose and inspect only",
    ] {
        assert!(guide.contains(marker), "the chart system message carries {marker:?}");
    }
    for removed in [
        "- k8s_execute_command",
        "- k8s_patch_resource",
        "- k8s_get_events",
        "- k8s_get_pod_logs",
    ] {
        assert!(!guide.contains(removed), "the guide no longer attaches {removed:?}");
    }

    let values = fs::read_to_string(chart.join("values.yaml")).expect("the runtime chart values exist");
    assert!(values.contains("integrations/appa-guide"));

    let demo = root.join("integrations/kagent/demo/chart");
    assert!(
        !demo.join("templates/guide.yaml").exists(),
        "the fixture chart must not create a second appa-guide"
    );
    let demo_values = fs::read_to_string(demo.join("values.yaml")).expect("the demo values exist");
    assert!(!demo_values.contains("integrations/appa-guide"));

    let policy = fs::read_to_string(demo.join("files/demo.appa.toml")).expect("the demo policy exists");
    assert!(policy.contains("name = \"k8s_apply_manifest\""));
    assert!(policy.contains("attention = [\"human-approval\"]"));
    assert!(policy.contains("name = \"skills\""));
    assert!(
        !policy.contains("name = \"bash\""),
        "the unused skill helpers stay undeclared"
    );

    let github = fs::read_to_string(root.join("batteries/github/appa.toml")).expect("the GitHub battery exists");
    assert!(github.contains("name = \"mcp__github__get_file_contents\""));
    assert!(github.contains("name = \"mcp__github__issue_write\""));
    assert!(!github.contains("name = \"get_file_contents\""));
    assert!(!github.contains("name = \"issue_write\""));
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
    assert!(website.contains("providers.openAI.model=gpt-5.6-luna"));
    assert!(website.contains("appaGuide.reasoningEffort=none"));
    assert!(website.contains("runtime.reasoningEffort=none"));
    assert!(website.contains("appa-kagent-adk"));
    assert!(website.contains("friendly-path-465518-r6/appa-public/golang-adk"));
    assert!(website.contains("kubectl get agent -A -o json"));
    assert!(website.contains("It preserves the Agent environment and adds a missing runtime URL:"));
    assert!(website.contains("`appa-guide` uses the existing `controller.agentImage`"));
    assert!(website.contains("cannot guarantee enforced write approval"));
}

#[test]
fn the_website_quickstart_is_copy_safe_and_dependency_ordered() {
    let website = fs::read_to_string(repo_root().join("website/content/docs/kagent.md")).expect("read website guide");
    let quickstart = website
        .split("## Quickstart")
        .nth(1)
        .expect("the guide has a Quickstart")
        .split("## Protect existing Agents")
        .next()
        .expect("the Quickstart ends before existing-Agent guidance");

    for heading in [
        "### 1. Install kagent with appa plugin",
        "### 2. Install appa",
        "### 3. Install the demo Agents",
    ] {
        assert!(quickstart.contains(heading), "the Quickstart carries {heading:?}");
    }

    let crds = quickstart
        .find("helm upgrade --install kagent-crds")
        .expect("CRDs install");
    let kagent = quickstart
        .find("helm upgrade --install kagent oci://")
        .expect("kagent install");
    let runtime = quickstart
        .find("helm upgrade --install appa-runtime")
        .expect("runtime install");
    let demo = quickstart
        .find("helm upgrade --install appa-kagent-demo")
        .expect("demo install");
    assert!(crds < kagent && kagent < runtime && runtime < demo);

    let install_block = quickstart
        .split("```sh")
        .skip(1)
        .map(|rest| rest.split("```").next().expect("a shell block closes"))
        .find(|block| block.contains("helm upgrade --install kagent oci://"))
        .expect("the kagent install has one copyable shell block");
    assert!(install_block.contains("helm upgrade --install kagent-crds"));
    assert!(install_block.contains("${OPENAI_API_KEY:?"));
    assert!(install_block.contains("name: kagent-openai"));
    assert!(install_block.contains("providers.openAI.apiKeySecretRef=kagent-openai"));
    assert!(!install_block.contains("providers.openAI.apiKey=\"$OPENAI_API_KEY\""));
    assert!(install_block.contains("grafana-mcp.enabled=false"));
    assert!(install_block.contains("querydoc.enabled=false"));
    assert!(!install_block.contains("<your-api-key>"));
    assert!(!quickstart.contains("quickstart-ops"));
}

#[test]
fn kagent_runtime_management_is_typed_vouched_and_least_privilege() {
    let root = repo_root();
    for path in [
        "charts/appa-runtime/files/appa.toml",
        "integrations/kagent/demo/chart/files/demo.appa.toml",
    ] {
        let policy = fs::read_to_string(root.join(path)).expect("read kagent policy");
        assert!(policy.contains("annotator = \"appa-guide-apply\""));
        assert!(policy.contains("/usr/local/bin/appa-guide-apply-annotator"));
        let apply = policy
            .split("[[policy.annotator]]")
            .find(|entry| entry.contains("name = \"appa-guide-apply\""))
            .expect("the policy declares the Agent apply annotator");
        assert!(apply.contains("marks = [\"human-approval\"]"));
        assert!(!policy.contains("name = \"k8s_get_events\""));
        assert!(!policy.contains("name = \"k8s_get_pod_logs\""));
        assert!(!policy.contains("k8s_get_resources(resource_type:configmap)"));
        assert!(!policy.contains("name = \"k8s_execute_command\""));
        assert!(!policy.contains("k8s_get_resource_yaml(resource_type:configmap)"));
        for tool in ["appa_get_runtime_state", "appa_match_batteries"] {
            assert!(policy.contains(&format!("name = \"{tool}\"")));
        }
        for tool in [
            "appa_include_battery",
            "appa_update_policy",
            "appa_reload_policy",
            "appa_refresh_batteries",
        ] {
            let declaration = policy
                .split("[[policy.tool]]")
                .find(|entry| entry.contains(&format!("name = \"{tool}\"")))
                .unwrap_or_else(|| panic!("policy declares {tool}"));
            assert!(declaration.contains("attention = [\"human-approval\"]"));
        }
        assert!(!policy.contains("name = \"k8s_get_resource_yaml\"\n"));
        assert!(!policy.contains("k8s_get_resource_yaml(resource_type:secret)"));
        assert!(policy.contains("helm_get_release(resource:manifest)"));
        assert!(!policy.contains("name = \"helm_get_release\"\n"));
    }

    let reference = read("references/kagent.md");
    for operation in [
        "appa_get_runtime_state",
        "appa_include_battery",
        "appa_update_policy",
        "appa_reload_policy",
        "appa_refresh_batteries",
    ] {
        assert!(reference.contains(operation), "the reference names {operation}");
    }
    assert!(reference.contains("generic Kubernetes commands"));
    assert!(reference.contains("one-shot APPA"));
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
