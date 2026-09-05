use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const CONFIG: &str = r#"
include = ["batteries/mail/appa.toml"]

[policy]
version = 2

[[policy.tool]]
name = "root"
delta = {}

[externals]
timeout_ms = 5000
max_body_bytes = 65536
"#;

struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral port binds");
    let port = listener.local_addr().expect("the bound address is readable").port();
    drop(listener);
    port
}

fn start(config: &Path, db: &Path, batteries_dir: &Path, port: u16) -> Server {
    let child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
        .arg("--config")
        .arg(config)
        .arg("--db")
        .arg(db)
        .arg("--batteries-dir")
        .arg(batteries_dir)
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary spawns");
    Server {
        child,
        url: format!("http://127.0.0.1:{port}"),
    }
}

fn wait_for_health(server: &mut Server) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(status) = server.child.try_wait().expect("the child polls") {
            let mut stderr = String::new();
            use std::io::Read;
            if let Some(mut pipe) = server.child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("the server exited before becoming healthy: {status}; stderr: {stderr}");
        }
        if http(&format!("{}/health", server.url), "GET", None).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the server never became healthy within the deadline");
}

fn http(url: &str, method: &str, body: Option<&str>) -> Option<String> {
    use std::io::{Read, Write};
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}")))?;
    let mut stream = std::net::TcpStream::connect(host).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("the read timeout sets");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: {host}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, payload) = response.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 2").then(|| payload.to_string())
}

fn write_battery(root: &Path, name: &str, tool: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("battery directory");
    std::fs::write(
        dir.join("appa.toml"),
        format!(
            r#"
                [policy]
                version = 2

                [[policy.tool]]
                name = "{tool}"
                delta = {{}}
            "#
        ),
    )
    .expect("battery config");
    dir
}

#[test]
fn get_batteries_lists_bundled_names_and_tools() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let batteries = dir.path().join("batteries");
    write_battery(&batteries, "mail", "send_mail");
    write_battery(&batteries, "chat", "post_message");
    let config = dir.path().join("appa.toml");
    std::fs::write(&config, CONFIG).expect("root config");
    let mut server = start(&config, &dir.path().join("appa.db"), &batteries, free_port());
    wait_for_health(&mut server);

    let body = http(&format!("{}/batteries", server.url), "GET", None).expect("/batteries answers");
    let value: serde_json::Value = serde_json::from_str(body.trim()).expect("the body is JSON");
    assert_eq!(
        value,
        serde_json::json!({
            "batteries": [
                {"name": "chat", "tools": ["post_message"]},
                {"name": "mail", "tools": ["send_mail"]}
            ]
        })
    );

    write_battery(&batteries, "mail", "send_mail_v2");
    let unchanged = http(&format!("{}/batteries", server.url), "GET", None).expect("the cached catalog answers");
    assert!(unchanged.contains("send_mail"));
    assert!(!unchanged.contains("send_mail_v2"));

    http(&format!("{}/reload", server.url), "POST", Some("")).expect("the battery-backed policy reloads");
    let changed = http(&format!("{}/batteries", server.url), "GET", None).expect("the refreshed catalog answers");
    assert!(changed.contains("send_mail_v2"));

    std::fs::write(batteries.join("mail/appa.toml"), "not toml = [").expect("the battery is made invalid");
    assert!(
        http(&format!("{}/reload", server.url), "POST", Some("")).is_none(),
        "an invalid battery refuses reload"
    );
    let preserved =
        http(&format!("{}/batteries", server.url), "GET", None).expect("a refused reload preserves the catalog");
    assert!(preserved.contains("send_mail_v2"));
}

#[test]
fn a_file_as_batteries_dir_refuses_startup() {
    let dir = tempfile::tempdir().expect("a temp dir is creatable");
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, "no").expect("the file writes");
    let config = dir.path().join("appa.toml");
    std::fs::write(
        &config,
        r#"
            [policy]
            version = 2
            [externals]
            timeout_ms = 5000
            max_body_bytes = 65536
        "#,
    )
    .expect("root config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
        .arg("--config")
        .arg(&config)
        .arg("--db")
        .arg(dir.path().join("appa.db"))
        .arg("--batteries-dir")
        .arg(&file)
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", free_port()))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary spawns");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child polls") {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the binary kept running instead of refusing");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success(), "the binary must refuse to serve");
    let mut stderr = String::new();
    use std::io::Read;
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    assert!(
        stderr.contains("not a directory"),
        "the refusal must name its cause; stderr was: {stderr}"
    );
}
