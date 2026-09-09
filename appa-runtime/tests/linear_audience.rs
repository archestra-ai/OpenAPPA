//! The Linear helper through the runtime's actual command boundary and #262 probes.
//! GraphQL is replaced with local payloads; no Linear account is contacted.

use std::path::{Path, PathBuf};

use appa_runtime::api::Runtime;
use appa_runtime::config::Config;

const VIEWER: &str = "00000000-0000-0000-0000-000000000002";
const WORKSPACE: &str = "00000000-0000-0000-0000-000000000001";

fn fixture(root: &Path) -> PathBuf {
    let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("marketplace/batteries/linear/audience-source.py");
    std::fs::copy(helper, root.join("source.py")).unwrap();
    std::fs::write(
        root.join("fixture.py"),
        r#"import json
from pathlib import Path
import source
def call(query, variables):
    data = json.loads(Path(__file__).with_name('data.json').read_text())
    return data
source.graphql = lambda token: call
raise SystemExit(source.main())
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("data.json"),
        serde_json::to_vec(&serde_json::json!({
            "viewer": {"id":VIEWER,"active":true,"guest":false},
            "organization": {"id":WORKSPACE,"users": {
                "nodes":[{"id":VIEWER,"active":true,"guest":false}],
                "pageInfo":{"hasNextPage":false,"endCursor":null}
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    let config = root.join("appa.toml");
    std::fs::write(
        &config,
        format!(
            r#"[policy]
version = 2
[policy.audience]
self = ["linear:viewer"]
internal = ["linear:workspace/{WORKSPACE}/members"]
[externals]
timeout_ms = 5000
max_body_bytes = 65536
[externals.audience.linear]
command = ["python3", "fixture.py"]
lookup = "people"
[externals.audience.people]
readers = {{ "linear:{VIEWER}" = "alice@corp.example" }}
"#
        ),
    )
    .unwrap();
    config
}

#[tokio::test]
async fn linear_command_probes_and_redirected_readers_survive_relocation() {
    let original = tempfile::tempdir().unwrap();
    let config_path = fixture(original.path());
    let config = Config::load(&config_path).unwrap();
    let runtime = Runtime::open(config, original.path().join("runtime.db"), None).unwrap();
    runtime.probe_sources().await.unwrap();

    let relocated = tempfile::tempdir().unwrap();
    for name in ["appa.toml", "fixture.py", "source.py", "data.json"] {
        std::fs::copy(original.path().join(name), relocated.path().join(name)).unwrap();
    }
    drop(runtime);
    original.close().unwrap();
    let config = Config::load(&relocated.path().join("appa.toml")).unwrap();
    let runtime = Runtime::open(config, relocated.path().join("runtime.db"), None).unwrap();
    runtime.probe_sources().await.unwrap();
}

#[tokio::test]
async fn linear_incomplete_membership_refuses_reload_without_installing_it() {
    let root = tempfile::tempdir().unwrap();
    let path = fixture(root.path());
    let runtime = Runtime::open(Config::load(&path).unwrap(), root.path().join("runtime.db"), None).unwrap();
    runtime.probe_sources().await.unwrap();
    let data = std::fs::read(root.path().join("data.json")).unwrap();
    std::fs::write(root.path().join("data.json"), "{}").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, before.replace("alice@corp.example", "bob@corp.example")).unwrap();
    let prepared = runtime.prepare_reload(Config::load(&path).unwrap()).unwrap();
    assert!(prepared.probe_sources().await.is_err());
    drop(prepared);
    std::fs::write(root.path().join("data.json"), data).unwrap();
    let prepared = runtime.prepare_reload(Config::load(&path).unwrap()).unwrap();
    prepared.probe_sources().await.unwrap();
    assert!(
        runtime.install(prepared).changed,
        "the failed probe must not have installed the revision"
    );
}
