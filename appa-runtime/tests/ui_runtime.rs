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

/// A missing helper credential blocks enforcement, not the runtime's HTTP server.
/// Saving it in the browser activates the same process and the same listening port.
#[tokio::test(flavor = "multi_thread")]
async fn setup_activates_enforcement_on_the_existing_runtime() {
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
    let log = std::fs::File::create(dir.path().join("runtime.log")).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_appa"))
            .args(["runtime", "--listen", "127.0.0.1:0", "--config"])
            .arg(&config)
            .arg("--db")
            .arg(dir.path().join("appa.db"))
            .env_remove("APPA_PROVIDER_DEMO_TOKEN")
            .env_remove("APPA_BATTERIES_DIR")
            .env_remove("APPA_GUIDE_LISTEN")
            .env_remove("APPA_MODULES_DIR")
            .stdout(Stdio::piped())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let mut url = String::new();
    std::io::BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut url)
        .unwrap();
    let url = url.trim();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "stdout={url:?}; stderr={}",
        std::fs::read_to_string(dir.path().join("runtime.log")).unwrap()
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap();
    let get = |path: &str| client.get(format!("{url}{path}"));
    assert_eq!(get("/health").send().await.unwrap().text().await.unwrap(), "ok");
    assert_eq!(
        get("/ready").send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        client
            .post(format!("{url}/hook"))
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(get("/").send().await.unwrap().status(), StatusCode::OK);
    let fingerprint = get("/binary-fingerprint").send().await.unwrap().text().await.unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args([
            "ui",
            "--no-open",
            "--setup",
            "--battery",
            "demo",
            "--runtime-url",
            url,
            "--config",
        ])
        .arg(&config)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let link = url::Url::parse(String::from_utf8_lossy(&output.stdout).trim()).unwrap();
    assert_eq!(link.port(), url::Url::parse(url).unwrap().port());
    assert!(link.query().unwrap().contains("batteries=demo"));
    assert!(link.fragment().is_none(), "UI URLs need no authentication token");
    assert!(get("/api/state").send().await.unwrap().status().is_success());
    let response = client
        .post(format!("{url}/api/credentials"))
        .header("Origin", url)
        .json(&json!({"credentials":{"APPA_PROVIDER_DEMO_TOKEN":"fixture-only-token"},"batteries":["demo"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.unwrap();
    assert!(!text.contains("fixture-only-token"));
    let state: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(state["batteries"][0]["check"]["status"], "ready");
    assert!(
        !state["runtime"].is_null(),
        "{text}; {}",
        std::fs::read_to_string(dir.path().join("runtime.log")).unwrap()
    );
    assert_eq!(get("/ready").send().await.unwrap().status(), StatusCode::OK);
    assert_eq!(
        get("/binary-fingerprint").send().await.unwrap().text().await.unwrap(),
        fingerprint
    );
    let described = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["describe", "--config"])
        .arg(&config)
        .env("APPA_RUNTIME_URL", url)
        .output()
        .unwrap();
    assert!(described.status.success());
    assert!(String::from_utf8_lossy(&described.stdout).contains("demo: ready (verified)"));
    // A bad edit refuses the reload but preserves the active deployment.
    let key = get("/policy-key").send().await.unwrap().text().await.unwrap();
    std::fs::write(&config, "not valid TOML {{{").unwrap();
    assert_eq!(
        client
            .post(format!("{url}/api/apply"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(get("/policy-key").send().await.unwrap().text().await.unwrap(), key);
    assert_eq!(get("/ready").send().await.unwrap().status(), StatusCode::OK);
    // Direct links work repeatedly without a browser session.
    assert!(
        client
            .post(format!("{url}/ui/open"))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(get("/api/state").send().await.unwrap().status().is_success());
    assert_eq!(
        client
            .post(format!("{url}/ui/open"))
            .header("Origin", "https://attacker.example")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}
