
use appa_runtime_v2::api::Runtime;
use appa_runtime_v2::config::Config;

#[test]
fn every_shipped_example_opens() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/claude-code/examples");
    let mut opened = 0;
    for entry in std::fs::read_dir(&examples).expect("the examples directory is readable") {
        let path = entry.expect("the directory entry is readable").path();
        if path.extension().is_none_or(|extension| extension != "toml") {
            continue;
        }
        let config = Config::load(&path).unwrap_or_else(|error| panic!("{} does not load: {error}", path.display()));
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        Runtime::open(config, dir.path().join("appa.db"), None)
            .unwrap_or_else(|error| panic!("{} does not open: {error}", path.display()));
        opened += 1;
    }
    assert!(opened >= 2, "both shipped examples were checked, not {opened}");
}
