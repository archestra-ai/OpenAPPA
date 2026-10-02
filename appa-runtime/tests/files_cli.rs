#![cfg(unix)]

use std::process::Command;
use std::sync::Arc;

use appa_engine::label::Label;
use appa_engine::value::{DispatchId, TrajectoryId};
use appa_eventlog::files::{FileOperation, FileStore};
use appa_eventlog::{Backend, LogStore};

fn dispatch() -> DispatchId {
    DispatchId::new(
        TrajectoryId::new("actor"),
        serde_json::from_value(serde_json::json!("ab".repeat(32))).unwrap(),
        0,
    )
}

#[test]
fn repair_and_relabel_commands_use_the_installed_database_and_keep_their_decisions_separate() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("workspace");
    let data_dir = fixture.path().join("data");
    let database = data_dir.join("appa.db");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&data_dir).unwrap();
    std::fs::write(workspace.join("tracked.txt"), "before").unwrap();

    let authority = Arc::new(LogStore::open(Backend::Sqlite { path: database.clone() }).unwrap());
    let initial = Label::top();
    let repaired_label = Label::new(
        appa_engine::label::Trust::new(1),
        appa_engine::label::Audience::nobody(),
    );
    let store = FileStore::open(authority, &workspace, "policy", &initial).unwrap();
    store
        .prepare("actor", "partial", FileOperation::Edit, "tracked.txt")
        .unwrap();
    store.bind("actor", "partial", &dispatch(), &repaired_label).unwrap();
    std::fs::write(workspace.join("tracked.txt"), "published without a report").unwrap();
    drop(store);

    let repair = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["files", "repair", "--workspace", workspace.to_str().unwrap()])
        .env("APPA_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(repair.status.success(), "{}", String::from_utf8_lossy(&repair.stderr));
    assert_eq!(
        String::from_utf8(repair.stdout).unwrap(),
        format!(
            "Repaired pending file operation in workspace: {}\n",
            workspace.display()
        )
    );

    let authority = Arc::new(LogStore::open(Backend::Sqlite { path: database.clone() }).unwrap());
    let reopened = FileStore::open(authority, &workspace, "policy", &initial).unwrap();
    assert_eq!(reopened.current("tracked.txt").unwrap().unwrap().label, repaired_label);
    drop(reopened);

    let relabel = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args([
            "files",
            "relabel",
            "--workspace",
            workspace.to_str().unwrap(),
            "tracked.txt",
        ])
        .env("APPA_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    assert!(relabel.status.success(), "{}", String::from_utf8_lossy(&relabel.stderr));

    let authority = Arc::new(LogStore::open(Backend::Sqlite { path: database }).unwrap());
    let reopened = FileStore::open(authority, &workspace, "policy", &initial).unwrap();
    assert_eq!(reopened.current("tracked.txt").unwrap().unwrap().label, initial);
}
