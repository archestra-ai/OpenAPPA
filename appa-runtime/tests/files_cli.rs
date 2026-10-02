#![cfg(unix)]

use std::process::Command;
use std::sync::Arc;

use appa_engine::label::Label;
use appa_engine::value::{DispatchId, TrajectoryId};
use appa_eventlog::files::{FileOperation, FileStore, FileStoreError};
use appa_eventlog::{Backend, LogStore};

fn dispatch() -> DispatchId {
    DispatchId::new(
        TrajectoryId::new("actor"),
        serde_json::from_value(serde_json::json!("ab".repeat(32))).unwrap(),
        0,
    )
}

#[test]
fn reconcile_command_accepts_current_files_from_the_installed_database() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let data_dir = fixture.path().join("data");
    let database = data_dir.join("appa.db");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&data_dir).unwrap();
    std::fs::write(workspace.join("tracked.txt"), "before").unwrap();

    let authority = Arc::new(LogStore::open(Backend::Sqlite { path: database.clone() }).unwrap());
    let store = FileStore::open(authority, &workspace, "policy", &Label::top()).unwrap();
    store
        .prepare("actor", "partial", FileOperation::Edit, "tracked.txt")
        .unwrap();
    store.bind("actor", "partial", &dispatch(), &Label::top()).unwrap();
    std::fs::write(workspace.join("tracked.txt"), "operator accepted change").unwrap();
    assert!(matches!(
        store.finish("actor", "partial", false),
        Err(FileStoreError::Quarantined)
    ));
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args([
            "files",
            "reconcile",
            "--workspace",
            workspace.to_str().unwrap(),
            "--accept-current-files",
        ])
        .env("APPA_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("Reconciled tracked workspace: {}\n", workspace.display())
    );

    let authority = Arc::new(LogStore::open(Backend::Sqlite { path: database }).unwrap());
    let reopened = FileStore::open(authority, &workspace, "policy", &Label::top()).unwrap();
    reopened
        .prepare("actor", "after", FileOperation::Read, "tracked.txt")
        .unwrap();
}
