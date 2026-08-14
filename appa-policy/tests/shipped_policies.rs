
use std::path::{Path, PathBuf};

use appa_policy::Config;

fn contracts() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("appa-policy sits one level below the repo root");
    let mut found = Vec::new();
    for dir in [
        root.join("harness-agentdojo/src/appa_dojo/contracts"),
        root.join("harness-taubench/src/appa_taubench/contracts"),
    ] {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn every_shipped_contract_loads() {
    let contracts = contracts();
    assert!(
        contracts.len() > 3,
        "expected the harness contracts, found {contracts:?}"
    );
    for path in contracts {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        if let Err(error) = Config::from_toml_str(&text) {
            panic!("{} does not load: {error}", path.display());
        }
    }
}
