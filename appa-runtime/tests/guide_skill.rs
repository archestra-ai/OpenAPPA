//! The shipped appa-guide skill is one composable package: a host-routing
//! SKILL.md, the shared core rules, and one reference file per host. These
//! checks keep the package whole: the router routes, the chart consumes this
//! package rather than a second skill, and the kagent policies gate runtime
//! management.

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

/// The parsed TOML file at `path` below the repository root.
fn toml_file(path: &str) -> toml::Table {
    let text = fs::read_to_string(repo_root().join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"));
    text.parse().unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

/// The `[[policy.<kind>]]` entries of a parsed policy file.
fn entries<'a>(file: &'a toml::Table, kind: &str) -> Vec<&'a toml::Table> {
    file["policy"]
        .get(kind)
        .and_then(toml::Value::as_array)
        .map(|entries| entries.iter().filter_map(toml::Value::as_table).collect())
        .unwrap_or_default()
}

/// The `[[policy.<kind>]]` entry called `name`.
fn named<'a>(file: &'a toml::Table, kind: &str, name: &str) -> Option<&'a toml::Table> {
    entries(file, kind)
        .into_iter()
        .find(|entry| entry.get("name").and_then(toml::Value::as_str) == Some(name))
}

fn strings(value: Option<&toml::Value>) -> Vec<&str> {
    value
        .and_then(toml::Value::as_array)
        .map(|items| items.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default()
}

/// The installer replaces only a file that starts with this frontmatter.
#[test]
fn the_router_opens_with_the_frontmatter_the_installer_recognizes() {
    assert!(read("SKILL.md").starts_with("---\nname: appa-guide\n"));
}

#[test]
fn the_chart_ships_a_byte_identical_copy_of_the_skill() {
    let root = repo_root();
    let chart = root.join("charts/appa-runtime/files/skill");
    let source = root.join("integrations/appa-guide");
    for file in ["SKILL.md", "references/core.md", "references/kagent.md"] {
        let shipped = fs::read_to_string(chart.join(file)).expect("the chart ships the skill file");
        let canonical = fs::read_to_string(source.join(file)).expect("the skill file exists");
        assert!(
            shipped == canonical,
            "charts/appa-runtime/files/skill/{file} drifted from integrations/appa-guide/{file}"
        );
    }
}

#[test]
fn only_the_runtime_chart_consumes_this_skill_package() {
    let root = repo_root();
    let chart = root.join("charts/appa-runtime");
    let guide =
        fs::read_to_string(chart.join("templates/appa-guide.yaml")).expect("the runtime chart renders the guide agent");
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

    let policy = toml_file("integrations/kagent/demo/chart/files/demo.appa.toml");
    assert!(named(&policy, "tool", "k8s_apply_manifest").is_some());
    assert!(entries(&policy, "authority").iter().any(|authority| {
        strings(authority.get("permits").and_then(|permits| permits.get("attention"))).contains(&"human-approval")
    }));
    assert!(named(&policy, "tool", "host/kagent/skills").is_some());
    assert!(
        named(&policy, "tool", "host/kagent/bash").is_none(),
        "the unused skill helpers stay undeclared"
    );

    let github = toml_file("marketplace/batteries/github/appa.toml");
    assert!(named(&github, "tool", "mcp/github/get_file_contents").is_some());
    assert!(named(&github, "tool", "mcp/github/issue_write").is_some());
    assert!(named(&github, "tool", "get_file_contents").is_none());
    assert!(named(&github, "tool", "issue_write").is_none());
}

#[test]
fn kagent_runtime_management_is_typed_vouched_and_least_privilege() {
    for path in [
        "charts/appa-runtime/files/appa.toml",
        "integrations/kagent/demo/chart/files/demo.appa.toml",
    ] {
        let policy = toml_file(path);
        let apply = named(&policy, "tool", "k8s_apply_manifest").expect("the policy declares the Agent apply");
        assert_eq!(
            apply.get("annotator").and_then(toml::Value::as_str),
            Some("appa-guide-apply")
        );
        assert_eq!(
            strings(policy["externals"]["annotators"]["appa-guide-apply"].get("command")),
            ["/usr/local/bin/appa-guide-apply-annotator"]
        );
        let annotator =
            named(&policy, "annotator", "appa-guide-apply").expect("the policy declares the Agent apply annotator");
        assert_eq!(strings(annotator.get("marks")), ["human-approval"]);
        for undeclared in [
            "k8s_get_events",
            "k8s_get_pod_logs",
            "k8s_get_resources(resource_type:configmap)",
            "k8s_execute_command",
            "k8s_get_resource_yaml(resource_type:configmap)",
            "k8s_get_resource_yaml",
            "k8s_get_resource_yaml(resource_type:secret)",
            "helm_get_release",
        ] {
            assert!(
                named(&policy, "tool", undeclared).is_none(),
                "{path} declares {undeclared}"
            );
        }
        for tool in [
            "mcp/appa-guide/appa_get_runtime_state",
            "mcp/appa-guide/appa_match_batteries",
            "helm_get_release(resource:manifest)",
        ] {
            assert!(named(&policy, "tool", tool).is_some(), "{path} declares {tool}");
        }
        for tool in [
            "mcp/appa-guide/appa_include_battery",
            "mcp/appa-guide/appa_update_policy",
            "mcp/appa-guide/appa_reload_policy",
            "mcp/appa-guide/appa_refresh_batteries",
        ] {
            let declaration = named(&policy, "tool", tool).unwrap_or_else(|| panic!("policy declares {tool}"));
            assert!(
                strings(
                    declaration
                        .get("requires")
                        .and_then(|requires| requires.get("attention"))
                )
                .contains(&"human-approval"),
                "{tool} needs a person"
            );
        }
    }
}
