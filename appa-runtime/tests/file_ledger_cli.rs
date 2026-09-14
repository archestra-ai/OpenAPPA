//! `appa file-ledger` against a real ledger: what an operator sees when a tracked workspace
//! stops accepting file calls, and the one state the command gives back.

use std::path::PathBuf;
use std::process::Command;

use appa_engine::label::Label;
use appa_eventlog::files::{FileOperation, FileStore};

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary directory");
        let workspace = root.path().join("work");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::write(workspace.join("tracked.txt"), "outside information").expect("tracked file");
        let db = root.path().join("files.db");
        Self {
            _root: root,
            workspace,
            db,
        }
    }

    fn store(&self) -> FileStore {
        FileStore::initialize(&self.db, &self.workspace, "policy-a", &Label::top()).expect("ledger")
    }

    fn run(&self, release: bool) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_appa"));
        command.arg("file-ledger").arg("--ledger").arg(&self.db);
        if release {
            command.arg("--release");
        }
        command.output().expect("file-ledger runs")
    }
}

fn text(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 output")
}

#[test]
fn a_released_call_the_harness_never_ran_is_given_back_on_request() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store
        .prepare("session", "call", FileOperation::Replace, "tracked.txt")
        .expect("reservation");
    store.bind("session", "call", "dispatch", &Label::top()).expect("bound");
    drop(store);

    // Reading it changes nothing: an operator decides, the command does not.
    let read = fixture.run(false);
    let stdout = text(&read);
    assert!(stdout.contains("reservation: session Replace tracked.txt"), "{stdout}");
    assert!(stdout.contains("--release gives it back"), "{stdout}");
    assert!(stdout.contains("drifted paths: none"), "{stdout}");
    assert!(read.status.success(), "{}", String::from_utf8_lossy(&read.stderr));
    let store = FileStore::inspect(&fixture.db).expect("ledger");
    assert!(store.reservation().expect("readable").is_some());

    let released = fixture.run(true);
    let stdout = text(&released);
    assert!(stdout.contains("released: the harness never ran this call"), "{stdout}");
    assert!(released.status.success(), "{}", String::from_utf8_lossy(&released.stderr));
    let store = FileStore::inspect(&fixture.db).expect("ledger");
    assert!(store.reservation().expect("readable").is_none());
    // The workspace accepts file calls again.
    store
        .prepare("session", "next", FileOperation::Read, "tracked.txt")
        .expect("the release did not leave the workspace wedged");
}

#[test]
fn a_workspace_that_moved_is_reported_and_never_released() {
    let fixture = Fixture::new();
    let store = fixture.store();
    store
        .prepare("session", "call", FileOperation::Replace, "tracked.txt")
        .expect("reservation");
    store.bind("session", "call", "dispatch", &Label::top()).expect("bound");
    std::fs::write(fixture.workspace.join("tracked.txt"), "somebody else wrote here")
        .expect("drift");
    drop(store);

    let output = fixture.run(true);
    let stdout = text(&output);
    assert!(stdout.contains("the workspace moved away from this pin"), "{stdout}");
    assert!(stdout.contains("drifted paths: 1"), "{stdout}");
    assert!(stdout.contains("tracked.txt"), "{stdout}");
    assert!(!output.status.success(), "a quarantined ledger is not a success");
    let store = FileStore::inspect(&fixture.db).expect("ledger");
    assert!(
        store.reservation().expect("readable").is_some(),
        "a moved workspace keeps its reservation"
    );
}

#[test]
fn a_settled_workspace_reports_no_reservation_and_no_drift() {
    let fixture = Fixture::new();
    drop(fixture.store());
    let output = fixture.run(false);
    let stdout = text(&output);
    assert!(stdout.contains("reservation: none"), "{stdout}");
    assert!(stdout.contains("drifted paths: none"), "{stdout}");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn an_uninitialized_ledger_is_refused_rather_than_created() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let absent = directory.path().join("files.db");
    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("file-ledger")
        .arg("--ledger")
        .arg(&absent)
        .output()
        .expect("file-ledger runs");
    assert!(!output.status.success());
    assert!(!absent.exists(), "inspection never creates a ledger");
    assert!(String::from_utf8_lossy(&output.stderr).contains("not initialized"));
}

