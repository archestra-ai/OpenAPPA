#![cfg(unix)]
//! What the hook entries an activation writes actually execute.
//!
//! The guarantee under test is narrow and stated as such: an entry runs the
//! binary its deployment installed, by absolute path, and posts to the
//! deployment's own endpoint. Proving it by reading the settings file would
//! prove nothing, so every assertion here comes from execution: a hostile
//! `appa` sits first on `PATH`, hostile `APPA_BIN` and `APPA_INSTALL_DIR` sit in
//! the environment, and the test asserts which binary ran and where its bytes
//! landed.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

mod common;
#[path = "common/init_fixture.rs"]
mod init_fixture;
use common::http;
use init_fixture::{Fixture, executable};

/// A runtime stand-in on a free loopback port. Records the paths it is asked
/// for and answers every hook, so the test can assert that the bytes a hook
/// posted arrived at the deployment's own endpoint.
fn recording_runtime() -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let url = format!("http://{}", listener.local_addr().expect("the bound address"));
    let (record, recorded) = mpsc::channel();

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
            let request = request.trim_end().to_owned();
            // A healthy answer, so the SessionStart entry finds the runtime up
            // and goes straight on to its post rather than starting one; a
            // hook is acknowledged on the wire.
            let (content_type, body) = if request.starts_with("GET /health") {
                ("text/plain", "ok")
            } else {
                ("application/json", r#"{"protocol":1,"decision":"ack"}"#)
            };
            if record.send(request).is_err() {
                return;
            }
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = connection.write_all(answer.as_bytes());
            let _ = connection.flush();
        }
    });

    (url, recorded)
}

/// An endpoint nothing is listening on: bound to learn a free port, then
/// released. A start probing this one finds no runtime and proceeds to start
/// the binary its deployment installed, which is the branch under test.
fn dead_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let address = listener.local_addr().expect("the bound address");
    drop(listener);
    format!("http://{address}")
}

#[test]
fn the_written_entries_run_the_deployed_binary_and_post_to_the_deployment_endpoint() {
    let fixture = Fixture::new();
    let (url, recorded) = recording_runtime();
    let output = fixture
        .activate()
        .env("APPA_ENDPOINT", &url)
        .output()
        .expect("appa activates");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    // A hostile appa, first on PATH, that fails loudly and records the fact.
    let poison_dir = fixture.root.join("poison");
    fs::create_dir_all(&poison_dir).expect("the poison directory");
    let poison_log = fixture.root.join("poisoned.log");
    let poison = poison_dir.join("appa");
    fs::write(
        &poison,
        format!("#!/bin/sh\nprintf 'ran\\n' >> {}\nexit 1\n", poison_log.display()),
    )
    .expect("the poisoned appa is written");
    executable(&poison);
    let path = format!("{}:{}", poison_dir.display(), std::env::var("PATH").unwrap_or_default());

    let entries = fixture.owned_hook_entries();
    let posting: Vec<_> = entries
        .into_iter()
        // The advice entry only prints; it posts nothing.
        .filter(|(_, hook)| hook["args"][0] == "hook")
        .collect();
    assert_eq!(posting.len(), 9, "one posting entry per event of a protected session");
    for (event, hook) in posting {
        let args: Vec<&str> = hook["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|argument| argument.as_str().expect("an argument"))
            .collect();
        let mut child = Command::new(hook["command"].as_str().expect("a command"))
            .args(&args)
            .env("PATH", &path)
            // Hostile values for every variable an entry must ignore.
            .env("APPA_BIN", &poison)
            .env("APPA_INSTALL_DIR", &poison_dir)
            .env("APPA_GATE", "1")
            .env_remove("APPA_RUNTIME_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("the {event} entry spawns: {error}"));
        // A gated hook the client translates and posts; a hook it cannot parse
        // blocks before any request, which would prove nothing about the endpoint.
        let _ = child.stdin.as_mut().expect("the child has a stdin pipe").write_all(
            br#"{"hook_event_name":"PreToolUse","session_id":"settings-test","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        );
        let _ = child.wait();

        // The bytes have to land somewhere: an endpoint the entry misspelled
        // would fail here. SessionStart probes /health first, so the posted
        // event is whichever request in its chain reaches /hook.
        let mut posted = false;
        while let Ok(request) = recorded.recv_timeout(Duration::from_secs(20)) {
            if request.starts_with("POST /hook ") {
                posted = true;
                break;
            }
            assert!(
                request.starts_with("GET /health"),
                "the {event} entry made an unexpected request: {request:?}",
            );
        }
        assert!(posted, "the {event} entry posted no event to the deployment's endpoint");
    }

    assert!(
        !poison_log.exists(),
        "an entry ran the appa on PATH: {}",
        fs::read_to_string(&poison_log).unwrap_or_default(),
    );
}

/// Without the fixture's starter, activation's last step is the deployed
/// binary's own `runtime ensure`: the runtime it brings up is that binary,
/// running from the deployed path with the deployment's config and data.
#[test]
fn activation_starts_the_deployed_runtime_through_its_own_binary() {
    let fixture = Fixture::new();
    let url = dead_endpoint();
    let output = fixture
        .activate()
        .env("APPA_ENDPOINT", &url)
        .env_remove("APPA_RUNTIME_STARTER")
        .output()
        .expect("appa activates");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    assert_eq!(http(&format!("{url}/health"), "GET", None).as_deref(), Some("ok"));
    let identity = http(&format!("{url}/binary-fingerprint"), "GET", None).expect("the runtime identifies itself");
    let pid: i32 = identity
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("the identity names a pid")
        .parse()
        .expect("the pid parses");
    assert_eq!(
        identity.lines().nth(1).map(str::trim),
        fixture.config.join("appa.toml").to_str(),
        "the runtime serves the deployment's config"
    );
    // The argument line carries the path the process was started from on
    // Linux and macOS alike; `comm` is the basename on Linux.
    let arguments = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    let arguments = String::from_utf8_lossy(&arguments.stdout);
    assert!(
        arguments
            .trim_start()
            .starts_with(fixture.deployed_binary().to_str().unwrap()),
        "the runtime runs from the deployed binary: {arguments:?}"
    );
    assert!(fixture.data.join("appa.db").is_file());

    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline && unsafe { libc::kill(pid, 0) } == 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
}
