#![cfg(feature = "daemon")]

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A missing helper credential stops the runtime at startup. `appa ui` serves setup on its
/// own; a token saved there lets the next start succeed, and a running runtime reloads.
#[tokio::test(flavor = "multi_thread")]
async fn setup_runs_without_a_runtime_and_reloads_a_running_one() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tempfile::tempdir().unwrap();
    let battery = dir.path().join("batteries/demo");
    std::fs::create_dir_all(&battery).unwrap();
    let config = dir.path().join("appa.toml");
    let root = "include = [\"batteries/demo/appa.toml\"]\n[policy]\nversion = 2\n[policy.audience]\nself = [\"demo:viewer\"]\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 4096\n";
    std::fs::write(&config, root).unwrap();
    std::fs::write(
        battery.join("appa-package.toml"),
        r#"schema = 1
name = "demo"
description = "Runtime setup fixture"
[battery]
policy = "appa.toml"
hosts = ["claude-code"]
helpers = ["source.py", "check.py"]
[battery.readiness]
command = ["python3", "check.py"]
required_executables = ["python3"]
"#,
    )
    .unwrap();
    std::fs::write(
        battery.join("appa.toml"),
        r#"[policy]
version = 2
[[policy.tool]]
name = "mcp/demo/read"
[externals.audience.demo]
command = ["python3", "source.py"]
token_env = "APPA_PROVIDER_DEMO_TOKEN"
selectors = [{ template = "viewer", feeds = "self" }]
"#,
    )
    .unwrap();
    std::fs::write(battery.join("source.py"), "import os,json\nassert os.environ.get('APPA_PROVIDER_DEMO_TOKEN') == 'fixture-only-token'\nprint(json.dumps({'version':1,'answer':{'members':['fixture@example.test']}}))\n").unwrap();
    std::fs::write(battery.join("check.py"), "import os,json\nok = os.environ.get('APPA_PROVIDER_DEMO_TOKEN') == 'fixture-only-token'\nprint(json.dumps({'status':'ready' if ok else 'needs_configuration','authentication':'token' if ok else 'none','reason':'verified' if ok else 'missing_credential'}))\n").unwrap();
    appa_package::validate_package(&battery).expect("valid fixture package");
    let clean = |command: &mut Command| {
        command
            .env_remove("APPA_PROVIDER_DEMO_TOKEN")
            .env_remove("APPA_BATTERIES_DIR")
            .env_remove("APPA_GUIDE_LISTEN")
            .env_remove("APPA_MODULES_DIR")
            .env_remove("APPA_RUNTIME_URL");
    };
    let runtime = |log: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_appa"));
        command
            .args(["runtime", "--listen", "127.0.0.1:0", "--config"])
            .arg(&config)
            .arg("--db")
            .arg(dir.path().join("appa.db"))
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.path().join(log)).unwrap());
        clean(&mut command);
        Process(command.spawn().unwrap())
    };
    let first_line = |process: &mut Process| {
        let mut line = String::new();
        std::io::BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        line.trim().to_owned()
    };
    let ui = |runtime_url: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_appa"));
        command
            .args(["ui", "--no-open", "--setup", "--battery", "demo", "--runtime-url", runtime_url, "--config"])
            .arg(&config)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        clean(&mut command);
        Process(command.spawn().unwrap())
    };
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let save = |page: String| {
        let client = client.clone();
        async move {
            let response = client
                .post(format!("{page}/api/credentials"))
                .header("Origin", &page)
                .json(&json!({"credentials":{"APPA_PROVIDER_DEMO_TOKEN":"fixture-only-token"},"batteries":["demo"]}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let text = response.text().await.unwrap();
            assert!(!text.contains("fixture-only-token"));
            serde_json::from_str::<Value>(&text).unwrap()
        }
    };

    // Without the token the audience source cannot answer, so the runtime stops at startup.
    let mut refused = runtime("refused.log");
    let status = refused.0.wait().unwrap();
    assert!(!status.success(), "the runtime must refuse to start");
    assert!(first_line(&mut refused).is_empty(), "a refused runtime announces no address");

    // Setup needs no runtime: `appa ui` serves the page and saves the token.
    let mut page = ui("http://127.0.0.1:1");
    let link = url::Url::parse(&first_line(&mut page)).unwrap();
    assert!(link.query().unwrap().contains("batteries=demo"));
    assert!(link.fragment().is_none(), "UI URLs need no authentication token");
    let origin = link.origin().ascii_serialization();
    assert!(client.get(format!("{origin}/api/state")).send().await.unwrap().status().is_success());
    let state = save(origin.clone()).await;
    assert_eq!(state["batteries"][0]["check"]["status"], "ready");
    assert!(state["runtime"].is_null());
    assert_eq!(state["applied"], "not_running");
    drop(page);

    // The next start reads the saved token and serves.
    let mut served = runtime("runtime.log");
    let url = first_line(&mut served);
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "stdout={url:?}; stderr={}",
        std::fs::read_to_string(dir.path().join("runtime.log")).unwrap()
    );
    let get = |path: &str| client.get(format!("{url}{path}"));
    assert_eq!(get("/health").send().await.unwrap().text().await.unwrap(), "ok");

    // With a runtime serving this configuration, the page reports it and a save reloads it.
    let mut page = ui(&url);
    let origin = url::Url::parse(&first_line(&mut page)).unwrap().origin().ascii_serialization();
    let state = save(origin.clone()).await;
    assert!(!state["runtime"].is_null());
    assert_eq!(state["applied"], "reloaded");
    let described = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["describe", "--config"])
        .arg(&config)
        .env("APPA_RUNTIME_URL", &url)
        .output()
        .unwrap();
    assert!(described.status.success());
    assert!(String::from_utf8_lossy(&described.stdout).contains("demo: ready (verified)"));

    // A bad edit refuses the reload and keeps the running policy.
    let key = get("/policy-key").send().await.unwrap().text().await.unwrap();
    std::fs::write(&config, "not valid TOML {{{").unwrap();
    let state = save(origin.clone()).await;
    assert_eq!(state["applied"], "refused");
    assert!(state["errors"].to_string().contains("kept its previous policy"));
    assert_eq!(get("/policy-key").send().await.unwrap().text().await.unwrap(), key);

    // The page refuses a request from another site.
    assert_eq!(
        client
            .post(format!("{origin}/api/credentials"))
            .header("Origin", "https://attacker.example")
            .json(&json!({"credentials":{}}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // The runtime's management routes refuse browser requests.
    assert_eq!(
        get("/dashboard").header("Origin", &origin).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}
