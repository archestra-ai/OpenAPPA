use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("appa-runtime is inside the repository")
        .to_path_buf()
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("read {path}: {error}"))
}

#[test]
fn parity_contract_names_every_required_invariant_and_only_allowed_exceptions() {
    let parity = read("integrations/appa-guide/PARITY.md");
    for id in [
        "P01", "P02", "P03", "P04", "P05", "P06", "P07", "P08", "P09", "P10", "P11", "P12", "P13", "P14",
    ] {
        assert_eq!(
            parity.matches(&format!("| {id} |")).count(),
            1,
            "parity contract carries {id} once"
        );
    }
    for id in ["X01", "X02", "X03", "X04", "X05"] {
        assert_eq!(
            parity.matches(&format!("| {id} |")).count(),
            1,
            "exception contract carries {id} once"
        );
    }
    assert!(parity.contains("No other host difference may weaken an invariant"));
    assert!(parity.contains("fails closed or reports it as"));
}

#[test]
fn shared_and_host_references_carry_the_parity_choke_points() {
    let shared = read("integrations/appa-guide/SKILL.md");
    let claude = read("integrations/appa-guide/references/claude-code.md");
    let kagent = read("integrations/appa-guide/references/kagent.md");

    for marker in [
        "matching reference file beside this one",
        "IFC monoids first",
        "Inspection and proposal drafting never require approval",
        "A battery is available",
        "Keep user-facing replies compact",
    ] {
        assert!(shared.contains(marker), "shared router carries {marker:?}");
    }
    for marker in [
        "Cross-check both sources",
        "Static contracts can reference `self` and `internal`",
        "End with: **Approve, or tell me what to change.**",
        "After approval:",
        "Reload only after an approved write",
    ] {
        assert!(claude.contains(marker), "Claude mapping carries {marker:?}");
    }
    for marker in [
        "Environment variables alone never prove the gate",
        "appa_match_batteries",
        "Helm values; provider credentials",
        "explicitly named proposal",
        "Never claim fleet-wide coverage",
    ] {
        assert!(kagent.contains(marker), "kagent mapping carries {marker:?}");
    }
}

#[test]
fn parity_evidence_files_exist() {
    for path in [
        "appa-runtime/tests/guide_skill.rs",
        "appa-runtime/src/mcp.rs",
        "integrations/kagent/appa-kagent-adk/tests/test_config_guard.py",
        "integrations/kagent/appa-kagent-adk-go/cmd/appa-kagent-adk-go/main_test.go",
        "integrations/kagent/tests/test_core.py",
        "integrations/kagent/e2e/ui/test_guide_ui.py",
    ] {
        assert!(root().join(path).is_file(), "parity evidence exists: {path}");
    }
}
