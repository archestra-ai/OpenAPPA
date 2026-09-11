#![cfg(unix)]
//! `appa runtime stop`: the runtime at the deployment's endpoint goes when it
//! is this user's own appa process, and nothing else is touched.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::time::Duration;

mod common;
#[path = "common/init_fixture.rs"]
mod init_fixture;
use common::{free_port, http, serve_runtime};
use init_fixture::shipped_default_config;

fn stop(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_appa"))
        .arg("runtime")
        .arg("stop")
        .args(arguments)
        .env_remove("APPA_RUNTIME_URL")
        .output()
        .expect("appa runtime stop runs")
}

/// The pid a healthy runtime names at `/binary-fingerprint`.
fn serving_pid(url: &str) -> i32 {
    http(&format!("{url}/binary-fingerprint"), "GET", None)
        .expect("the runtime identifies itself")
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("the identity names a pid")
        .parse()
        .expect("the pid parses")
}

fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn fixture_config() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("appa.toml");
    std::fs::write(&config, shipped_default_config()).expect("the config is written");
    let db = directory.path().join("appa.db");
    (directory, config, db)
}

#[test]
fn the_runtime_at_the_deployment_endpoint_is_stopped() {
    let (_directory, config, db) = fixture_config();
    let mut served = serve_runtime(&config, &db);

    let output = stop(&["--deployment-url", &served.url]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline && !served.has_exited() {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(served.has_exited(), "the runtime process is gone");
    assert_eq!(http(&format!("{}/health", served.url), "GET", None), None);
}

#[test]
fn nothing_answering_is_not_a_failure() {
    let url = format!("http://127.0.0.1:{}", free_port());
    let output = stop(&["--deployment-url", &url]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

/// A runtime the session named itself is the user's own: neither the hooks
/// nor this command restart or stop it.
#[test]
fn a_runtime_at_a_url_of_the_users_own_is_left_running() {
    let (_directory, config, db) = fixture_config();
    let served = serve_runtime(&config, &db);
    let pid = serving_pid(&served.url);

    let output = stop(&["--url", &served.url]);

    assert!(!output.status.success());
    assert!(process_exists(pid));
    assert_eq!(
        http(&format!("{}/health", served.url), "GET", None).as_deref(),
        Some("ok")
    );
}

/// A listener answering as a runtime whose pid is not this user's appa process:
/// the pid arrived in an HTTP body, so it is checked before it is signalled.
#[test]
fn a_process_that_is_not_this_users_appa_runtime_is_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!("http://{}", listener.local_addr().expect("the bound address"));
    // This test process: alive, this user's, and not named appa.
    let identity = format!("not-a-build {}\n/nowhere/appa.toml", std::process::id());
    std::thread::spawn(move || {
        for connection in listener.incoming() {
            let Ok(mut connection) = connection else {
                return;
            };
            let mut reader = BufReader::new(connection.try_clone().expect("the stream clones"));
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let body = if request.starts_with("GET /health") {
                "ok"
            } else {
                identity.as_str()
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = connection.write_all(answer.as_bytes());
            let _ = connection.flush();
        }
    });

    let output = stop(&["--deployment-url", &url]);

    assert!(!output.status.success());
    assert_eq!(http(&format!("{url}/health"), "GET", None).as_deref(), Some("ok"));
}
