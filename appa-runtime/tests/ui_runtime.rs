#![cfg(feature = "daemon")]

use reqwest::StatusCode;
use serde_json::{Value, json};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};

/// The tests' own client to the page, on the crypto provider the binary installs.
fn http_client() -> reqwest::Client {
    appa_runtime::tls::install_crypto_provider();
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap()
}

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
            .args([
                "ui",
                "--no-open",
                "--battery",
                "demo",
                "--runtime-url",
                runtime_url,
                "--config",
            ])
            .arg(&config)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        clean(&mut command);
        Process(command.spawn().unwrap())
    };
    let client = http_client();
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
    assert!(
        first_line(&mut refused).is_empty(),
        "a refused runtime announces no address"
    );

    // Setup needs no runtime: `appa ui` serves the token page and saves the token.
    let mut page = ui("http://127.0.0.1:1");
    let mut stdout = std::io::BufReader::new(page.0.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let link = url::Url::parse(line.trim()).unwrap();
    assert_eq!(link.path(), "/connect");
    assert_eq!(link.query(), Some("battery=demo"));
    assert!(link.fragment().is_none(), "UI URLs need no authentication token");
    let origin = link.origin().ascii_serialization();
    assert!(
        client
            .get(format!("{origin}/api/state"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let state = save(origin.clone()).await;
    assert_eq!(state["batteries"][0]["check"]["status"], "ready");
    assert!(state["runtime"].is_null());
    assert_eq!(state["applied"], "not_running");
    // A ready battery ends the command, which prints its sanitized status.
    let exit = page.0.wait().unwrap();
    assert!(exit.success(), "setup exits cleanly once the battery is ready");
    let mut printed = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut printed).unwrap();
    assert!(!printed.contains("fixture-only-token"));
    let status: Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(status["batteries"][0]["name"], "demo");
    assert_eq!(status["batteries"][0]["check"]["status"], "ready");
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
    let page_origin = |page: &mut Process| {
        url::Url::parse(&first_line(page))
            .unwrap()
            .origin()
            .ascii_serialization()
    };
    let mut page = ui(&url);
    let origin = page_origin(&mut page);
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
    let mut page = ui(&url);
    let origin = page_origin(&mut page);
    let state = save(origin.clone()).await;
    assert_eq!(state["applied"], "refused");
    assert!(state["errors"].as_array().is_some_and(|errors| !errors.is_empty()));
    assert_eq!(get("/policy-key").send().await.unwrap().text().await.unwrap(), key);

    drop(page);
    // The runtime's management routes refuse browser requests.
    assert_eq!(
        get("/prerequisites")
            .header("Origin", &origin)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

/// The page asks for what the deployment's target reads: a credential only the
/// multi-user target policy binds is undeclared on a single-user deployment, and on a
/// multi-user one it is missing until saved. An unknown target refuses startup.
#[tokio::test(flavor = "multi_thread")]
async fn setup_asks_for_the_credentials_of_the_deployment_target() {
    let dir = tempfile::tempdir().unwrap();
    let battery = dir.path().join("batteries/team");
    std::fs::create_dir_all(&battery).unwrap();
    let config = dir.path().join("appa.toml");
    std::fs::write(
        &config,
        "include = [\"batteries/team/appa.toml\"]\n[policy]\nversion = 2\n",
    )
    .unwrap();
    std::fs::write(
        battery.join("appa-package.toml"),
        "schema = 1\nname = \"team\"\ndescription = \"Target setup fixture\"\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\nhelpers = [\"source.py\"]\n[battery.targets.single-user]\npolicy = \"single-user.toml\"\n[battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n",
    )
    .unwrap();
    std::fs::write(
        battery.join("appa.toml"),
        "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/team/read\"\ndelta = {}\n",
    )
    .unwrap();
    std::fs::write(battery.join("single-user.toml"), "[policy]\nversion = 2\n").unwrap();
    std::fs::write(
        battery.join("multi-user.toml"),
        "[policy]\nversion = 2\n[externals.audience.team]\ncommand = [\"python3\", \"source.py\"]\ntoken_env = \"APPA_PROVIDER_TEAM_TOKEN\"\nselectors = [{ template = \"viewer\", feeds = \"self\" }]\n",
    )
    .unwrap();
    std::fs::write(battery.join("source.py"), "print('{}')\n").unwrap();
    appa_package::validate_package(&battery).expect("valid fixture package");

    let ui = |target: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_appa"));
        command
            .args([
                "ui",
                "--no-open",
                "--battery",
                "team",
                "--runtime-url",
                "http://127.0.0.1:1",
                "--config",
            ])
            .arg(&config)
            .env_remove("APPA_PROVIDER_TEAM_TOKEN")
            .env_remove("APPA_BATTERY_TARGET")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(target) = target {
            command.env("APPA_BATTERY_TARGET", target);
        }
        let mut process = Process(command.spawn().unwrap());
        let mut line = String::new();
        std::io::BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        (process, line.trim().to_owned())
    };
    let client = http_client();
    let post = |origin: String, path: &'static str, body: Value| {
        let client = client.clone();
        async move {
            client
                .post(format!("{origin}{path}"))
                .header("Origin", &origin)
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let origin = |link: &str| url::Url::parse(link).unwrap().origin().ascii_serialization();
    let saving = json!({"credentials": {"APPA_PROVIDER_TEAM_TOKEN": "fixture-only-token"}, "batteries": ["team"]});

    let (_single, link) = ui(None);
    let refused = post(origin(&link), "/api/credentials", saving.clone()).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    let (_multi, link) = ui(Some("multi-user"));
    let checked: Value = post(origin(&link), "/api/check", json!({"batteries": ["team"]}))
        .await
        .json()
        .await
        .unwrap();
    let team = &checked["batteries"][0];
    assert_eq!(team["credentials"][0]["variable"], "APPA_PROVIDER_TEAM_TOKEN");
    assert_eq!(team["check"]["status"], "needs_configuration");
    assert_eq!(team["check"]["reason"], "missing_credential");

    let saved = post(origin(&link), "/api/credentials", saving).await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: Value = saved.json().await.unwrap();
    assert_eq!(
        saved["batteries"][0]["credentials"][0]["variable"],
        "APPA_PROVIDER_TEAM_TOKEN"
    );
    assert_eq!(saved["batteries"][0]["credentials"][0]["saved"], true);
    assert_eq!(saved["batteries"][0]["check"]["status"], "ready");

    let (mut invalid, link) = ui(Some("team"));
    assert!(link.is_empty(), "an unknown target announces no page");
    assert!(!invalid.0.wait().unwrap().success());
}

/// The page reads a runtime's credentials only when it serves the page's deployment target:
/// a multi-user runtime is not the single-user page's runtime.
#[tokio::test(flavor = "multi_thread")]
async fn setup_refuses_a_runtime_of_another_deployment_target() {
    let dir = tempfile::tempdir().unwrap();
    let battery = dir.path().join("batteries/team");
    std::fs::create_dir_all(&battery).unwrap();
    let config = dir.path().join("appa.toml");
    std::fs::write(
        &config,
        "include = [\"batteries/team/appa.toml\"]\n[policy]\nversion = 2\n[externals]\ntimeout_ms = 2000\nmax_body_bytes = 4096\n",
    )
    .unwrap();
    std::fs::write(
        battery.join("appa-package.toml"),
        "schema = 1\nname = \"team\"\ndescription = \"Target runtime fixture\"\n[battery]\npolicy = \"appa.toml\"\nhosts = [\"claude-code\"]\n[battery.targets.single-user]\npolicy = \"single-user.toml\"\n[battery.targets.multi-user]\npolicy = \"multi-user.toml\"\n",
    )
    .unwrap();
    std::fs::write(
        battery.join("appa.toml"),
        "[policy]\nversion = 2\n[[policy.tool]]\nname = \"mcp/team/read\"\ndelta = {}\n",
    )
    .unwrap();
    std::fs::write(battery.join("single-user.toml"), "[policy]\nversion = 2\n").unwrap();
    std::fs::write(battery.join("multi-user.toml"), "[policy]\nversion = 2\n").unwrap();
    appa_package::validate_package(&battery).expect("valid fixture package");

    let mut runtime = Process(
        Command::new(env!("CARGO_BIN_EXE_appa"))
            .args(["runtime", "--listen", "127.0.0.1:0", "--config"])
            .arg(&config)
            .arg("--db")
            .arg(dir.path().join("appa.db"))
            .env_remove("APPA_BATTERIES_DIR")
            .env_remove("APPA_MODULES_DIR")
            .env("APPA_BATTERY_TARGET", "multi-user")
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.path().join("runtime.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    let first_line = |process: &mut Process| {
        let mut line = String::new();
        std::io::BufReader::new(process.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        line.trim().to_owned()
    };
    let url = first_line(&mut runtime);
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "stdout={url:?}; stderr={}",
        std::fs::read_to_string(dir.path().join("runtime.log")).unwrap()
    );

    let ui = |target: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_appa"));
        command
            .args([
                "ui",
                "--no-open",
                "--battery",
                "team",
                "--runtime-url",
                &url,
                "--config",
            ])
            .arg(&config)
            .env_remove("APPA_BATTERIES_DIR")
            .env_remove("APPA_BATTERY_TARGET")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(target) = target {
            command.env("APPA_BATTERY_TARGET", target);
        }
        Process(command.spawn().unwrap())
    };
    let client = http_client();
    let state = |link: String| {
        let client = client.clone();
        async move {
            let origin = url::Url::parse(&link).unwrap().origin().ascii_serialization();
            client
                .get(format!("{origin}/api/state"))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };

    let mut single = ui(None);
    assert!(state(first_line(&mut single)).await["runtime"].is_null());

    let mut multi = ui(Some("multi-user"));
    let served = state(first_line(&mut multi)).await;
    assert_eq!(served["runtime"]["target"], "multi-user");
}
