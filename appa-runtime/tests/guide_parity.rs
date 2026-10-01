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
        "P01", "P02", "P03", "P04", "P05", "P06", "P07", "P08", "P09", "P10", "P11", "P12", "P13", "P14", "P15",
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
