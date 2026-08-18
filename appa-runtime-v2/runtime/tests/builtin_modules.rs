use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use appa_runtime_v2::api::{OpenError, Runtime};
use appa_runtime_v2::config::Config;

const CONFIG: &str = r#"
[policy]
version = 1

[[policy.tool]]
name = "Bash"

[externals]
timeout_ms = 2000
max_body_bytes = 65536
"#;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root resolves")
}

fn build_module(package: &str, features: Option<&str>) -> PathBuf {
    let root = workspace_root();
    let target = root.join("target/module-fixtures").join(features.unwrap_or("default"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut command = Command::new(cargo);
    command
        .current_dir(&root)
        .args(["build", "-p", package, "--message-format=json-render-diagnostics"])
        .arg("--target-dir")
        .arg(&target);
    if let Some(features) = features {
        command.args(["--features", features]);
    }
    let output = command.output().expect("cargo runs");
    assert!(
        output.status.success(),
        "the fixture build failed:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("cargo messages are UTF-8");
    let extension = std::env::consts::DLL_EXTENSION;
    let target_name = package.replace('-', "_");
    for line in stdout.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" || message["target"]["name"] != target_name.as_str() {
            continue;
        }
        let Some(filenames) = message["filenames"].as_array() else {
            continue;
        };
        for filename in filenames {
            let Some(path) = filename.as_str() else { continue };
            if path.ends_with(extension) {
                return PathBuf::from(path);
            }
        }
    }
    panic!("the fixture build produced no {extension} artifact for {package}");
}

fn good_module() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| build_module("appa-module-fixture", None))
}

struct Deployment {
    dir: tempfile::TempDir,
}

impl Deployment {
    fn new(config: &str) -> Deployment {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        std::fs::write(dir.path().join("appa.toml"), config).expect("the config writes");
        std::fs::create_dir(dir.path().join("modules")).expect("the modules dir creates");
        Deployment { dir }
    }

    fn config(&self) -> Config {
        Config::load(&self.dir.path().join("appa.toml")).expect("the test config loads")
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("appa.db")
    }

    fn modules(&self) -> PathBuf {
        self.dir.path().join("modules")
    }

    fn install(&self, artifact: &Path, filename: &str) {
        std::fs::copy(artifact, self.modules().join(filename)).expect("the module copies");
    }

    #[allow(clippy::result_large_err)]
    fn open(&self) -> Result<Runtime, OpenError> {
        Runtime::open(self.config(), self.db(), Some(self.modules()))
    }
}

fn library_filename(stem: &str) -> String {
    format!("{stem}.{}", std::env::consts::DLL_EXTENSION)
}

fn expect_refusal(deployment: &Deployment, needle: &str) {
    match deployment.open() {
        Err(OpenError::Modules(message)) => {
            assert!(
                message.contains(needle),
                "refusal {message:?} does not mention {needle:?}"
            );
        }
        Err(other) => panic!("expected a modules refusal mentioning {needle:?}, got: {other}"),
        Ok(_) => panic!("expected a modules refusal mentioning {needle:?}, but the runtime opened"),
    }
    assert!(!deployment.db().exists(), "a refused open must create no database");
}

#[test]
fn a_wellformed_module_loads_and_its_name_resolves_from_config() {
    let config = format!("{CONFIG}\n[externals.authorities.auto]\nbuiltin = \"fixture-auth\"\n");
    let deployment = Deployment::new(&config);
    deployment.install(
        good_module(),
        &library_filename("libanything_the_filename_means_nothing"),
    );
    deployment.open().expect("a well-formed module deployment opens");
    assert!(deployment.db().exists(), "a successful open creates its database");
}

#[test]
fn stock_builtins_need_no_modules_directory_at_all() {
    let config = format!(
        "{CONFIG}\n[externals.authorities.auto]\nbuiltin = \"approve\"\n\n[externals.sanitizers.pii]\nbuiltin = \"redact-email\"\n"
    );
    let deployment = Deployment::new(&config);
    Runtime::open(deployment.config(), deployment.db(), None).expect("the stock-only deployment opens");
}

#[test]
fn an_empty_modules_directory_is_a_valid_deployment() {
    let deployment = Deployment::new(CONFIG);
    deployment.open().expect("an empty modules directory opens");
}

#[test]
fn a_missing_modules_directory_refuses_before_the_store_opens() {
    let deployment = Deployment::new(CONFIG);
    std::fs::remove_dir(deployment.modules()).expect("the modules dir removes");
    expect_refusal(&deployment, "unreadable");
}

#[test]
fn a_wrong_abi_version_refuses() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(
        &build_module("appa-module-fixture-bad", Some("bad-abi")),
        &library_filename("libbad_abi"),
    );
    expect_refusal(&deployment, "ABI version 99");
}

#[test]
fn a_missing_symbol_refuses() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(
        &build_module("appa-module-fixture-bad", Some("missing-symbol")),
        &library_filename("libmissing_symbol"),
    );
    expect_refusal(&deployment, "appa_builtin_descriptor_v1");
}

#[test]
fn a_descriptor_name_outside_the_grammar_refuses() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(
        &build_module("appa-module-fixture-bad", Some("bad-name")),
        &library_filename("libbad_name"),
    );
    expect_refusal(&deployment, "lowercase kebab");
}

#[test]
fn a_module_claiming_a_stock_name_refuses() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(
        &build_module("appa-module-fixture-bad", Some("claims-approve")),
        &library_filename("libclaims_approve"),
    );
    expect_refusal(&deployment, "already provided");
}

#[test]
fn two_modules_claiming_one_name_refuse() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(good_module(), &library_filename("libfirst"));
    deployment.install(good_module(), &library_filename("libsecond"));
    expect_refusal(&deployment, "fixture-auth");
}

#[test]
fn a_stray_file_in_the_modules_directory_refuses() {
    let deployment = Deployment::new(CONFIG);
    std::fs::write(deployment.modules().join("README.md"), "not a module").expect("the file writes");
    expect_refusal(&deployment, "README.md");
}

#[test]
fn a_dangling_builtin_reference_refuses_with_and_without_a_directory() {
    let config = format!("{CONFIG}\n[externals.sanitizers.pii]\nbuiltin = \"no-such-module\"\n");

    let deployment = Deployment::new(&config);
    expect_refusal(&deployment, "no-such-module");

    let deployment = Deployment::new(&config);
    match Runtime::open(deployment.config(), deployment.db(), None) {
        Err(OpenError::Modules(message)) => assert!(message.contains("no-such-module")),
        Err(other) => panic!("expected a modules refusal, got: {other}"),
        Ok(_) => panic!("a dangling builtin reference must refuse"),
    }
    assert!(!deployment.db().exists());
}

#[test]
fn two_kinds_claiming_one_name_refuse() {
    let deployment = Deployment::new(CONFIG);
    deployment.install(good_module(), &library_filename("liba_authority"));
    deployment.install(
        &build_module("appa-module-fixture-bad", Some("same-name-sanitizer")),
        &library_filename("libz_sanitizer"),
    );
    expect_refusal(&deployment, "fixture-auth");
}

#[test]
fn dot_files_are_skipped_not_refused() {
    let deployment = Deployment::new(CONFIG);
    std::fs::write(deployment.modules().join(".DS_Store"), b"finder droppings").expect("the file writes");
    deployment.open().expect("a dot-file is ignored");
}

#[test]
fn a_kind_crossed_reference_refuses() {
    let config = format!("{CONFIG}\n[externals.sanitizers.pii]\nbuiltin = \"fixture-auth\"\n");
    let deployment = Deployment::new(&config);
    deployment.install(good_module(), &library_filename("libgood"));
    expect_refusal(&deployment, "fixture-auth");
}
