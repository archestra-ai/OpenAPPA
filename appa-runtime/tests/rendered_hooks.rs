#![cfg(unix)]
//! What a *rendered* deployment's hooks actually execute.
//!
//! The guarantee under test is narrow and stated as such: a deployed tree
//! performs no PATH resolution for the `appa` binary. It still invokes `sh`,
//! `curl` and `uname` by name, and that is unchanged.
//!
//! Proving it by scanning the rendered files would prove nothing, so every
//! assertion here comes from execution: a hostile `appa` sits first on `PATH`,
//! hostile `APPA_BIN` and `APPA_INSTALL_DIR` sit in the environment, and the
//! test asserts which binary ran and where its bytes landed.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use appa_runtime::plugin_bundle::{DEFAULT_ENDPOINT_URL, Endpoint, Population, materialize};

mod common;
use common::stage_bundle;

fn executable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("the fixture is executable");
}

/// A runtime stand-in on a free loopback port. Records the paths it is asked
/// for and answers every hook, so the test can assert that the bytes a hook
/// posted arrived at the deployment's own endpoint.
fn recording_runtime() -> (Endpoint, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let endpoint = Endpoint::parse(&format!("http://{}", listener.local_addr().expect("the bound address")))
        .expect("the bound address is a usable endpoint");
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
            // A healthy answer, so the SessionStart starter finds the runtime up
            // and chains straight through to the hook rather than trying to
            // start one; a hook is acknowledged on the wire.
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

    (endpoint, recorded)
}

/// An endpoint nothing is listening on: bound to learn a free port, then
/// released. A starter probing this one finds no runtime and proceeds to start
/// the binary its paths file names, which is the branch under test.
fn dead_endpoint() -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let address = listener.local_addr().expect("the bound address");
    drop(listener);
    Endpoint::parse(&format!("http://{address}")).expect("the released address is a usable endpoint")
}

/// Every hook command the rendered map registers, with the event it belongs to.
fn rendered_commands(deployment: &Path) -> Vec<(String, String)> {
    let hooks: serde_json::Value = serde_json::from_slice(
        &fs::read(deployment.join("plugin/hooks/hooks.json")).expect("the rendered hook map is readable"),
    )
    .expect("the rendered hook map parses");

    let mut commands = Vec::new();
    for (event, groups) in hooks["hooks"].as_object().expect("the map is an object") {
        for group in groups.as_array().expect("each event carries groups") {
            for hook in group["hooks"].as_array().expect("each group carries hooks") {
                let command = hook["command"].as_str().expect("each hook carries a command");
                // The context hook only prints a file; it posts nothing.
                if command.contains("session-context.md") {
                    continue;
                }
                commands.push((event.clone(), command.to_owned()));
            }
        }
    }
    assert!(!commands.is_empty(), "the rendered map registered no commands");
    commands
}

#[test]
fn rendered_hooks_run_the_deployed_binary_and_post_to_the_deployment_endpoint() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    let source = stage_bundle(root);
    let (endpoint, recorded) = recording_runtime();

    // The binary this deployment installs, on a path that is not on PATH.
    let deployed_bin = root.join("data/bin");
    fs::create_dir_all(&deployed_bin).expect("the private binary directory");
    let deployed = deployed_bin.join("appa");
    fs::copy(env!("CARGO_BIN_EXE_appa"), &deployed).expect("the deployed binary is copied");
    executable(&deployed);

    let config = root.join("config/appa.toml");
    fs::create_dir_all(config.parent().expect("config has a parent")).expect("config directory");

    let deployment = materialize(
        Population::Tree(&source),
        &root.join("data/deployments"),
        &deployed,
        &config,
        &root.join("data"),
        &endpoint,
    )
    .expect("the deployment materializes");

    // A hostile appa, first on PATH, that fails loudly and records the fact.
    let poison_dir = root.join("poison");
    fs::create_dir_all(&poison_dir).expect("the poison directory");
    let poison_log = root.join("poisoned.log");
    let poison = poison_dir.join("appa");
    fs::write(
        &poison,
        format!("#!/bin/sh\nprintf 'ran\\n' >> {}\nexit 1\n", poison_log.display()),
    )
    .expect("the poisoned appa is written");
    executable(&poison);

    let path = format!("{}:{}", poison_dir.display(), std::env::var("PATH").unwrap_or_default());

    for (event, command) in rendered_commands(&deployment.root) {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env("PATH", &path)
            // Hostile values for every variable a deployed tree must ignore.
            .env("APPA_BIN", &poison)
            .env("APPA_INSTALL_DIR", &poison_dir)
            .env("APPA_GATE", "1")
            .env("CLAUDE_PLUGIN_ROOT", deployment.root.join("plugin"))
            .env_remove("APPA_RUNTIME_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("the {event} hook spawns: {error}"));
        // A gated hook the client translates and posts; a hook it cannot parse
        // blocks before any request, which would prove nothing about the endpoint.
        let _ = child.stdin.as_mut().expect("the child has a stdin pipe").write_all(
            br#"{"hook_event_name":"PreToolUse","session_id":"rendered-test","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        );
        let _ = child.wait();

        // The bytes have to land somewhere: an endpoint the rendering missed
        // would fail here even if it were spelled in a way no scan could catch.
        // SessionStart probes /health through its starter first, so the posted
        // event is whichever request in this hook's chain reaches /hook.
        let mut posted = false;
        while let Ok(request) = recorded.recv_timeout(std::time::Duration::from_secs(20)) {
            if request.starts_with("POST /hook ") {
                posted = true;
                break;
            }
            assert!(
                request.starts_with("GET /health"),
                "the {event} hook made an unexpected request: {request:?}",
            );
        }
        assert!(posted, "the {event} hook posted no event to the deployment's endpoint",);
    }

    assert!(
        !poison_log.exists(),
        "a rendered hook ran the appa on PATH: {}",
        fs::read_to_string(&poison_log).unwrap_or_default(),
    );
}

#[test]
fn the_session_start_starter_starts_the_binary_its_deployment_installed() {
    // init runs the starter directly, without CLAUDE_PLUGIN_ROOT, so the
    // directory lookup inside it is load-bearing rather than incidental. The
    // endpoint is dead on purpose: a starter that finds a healthy runtime exits
    // before it ever resolves a binary, which would prove nothing at all.
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    let source = stage_bundle(root);
    let endpoint = dead_endpoint();

    let deployed = root.join("data/bin/appa");
    let started = root.join("started");
    fs::create_dir_all(deployed.parent().expect("a parent")).expect("the private binary directory");
    // A stand-in that records having run. It never becomes healthy, so the
    // starter goes on to fail -- the marker, not the exit status, is the proof.
    fs::write(
        &deployed,
        format!("#!/bin/sh\nprintf 'started\\n' > '{}'\nexit 0\n", started.display()),
    )
    .expect("the deployed stand-in is written");
    executable(&deployed);

    let deployment = materialize(
        Population::Tree(&source),
        &root.join("data/deployments"),
        &deployed,
        &root.join("config/appa.toml"),
        &root.join("data"),
        &endpoint,
    )
    .expect("the deployment materializes");

    let mut child = Command::new("sh")
        .arg(deployment.root.join("plugin/hooks/ensure-runtime.sh"))
        .env_remove("CLAUDE_PLUGIN_ROOT")
        .env_remove("APPA_RUNTIME_URL")
        // Everything the starter could resolve through instead is hostile.
        .env("APPA_BIN", "/nonexistent/hostile/appa")
        .env("APPA_INSTALL_DIR", "/nonexistent/hostile")
        .spawn()
        .expect("the starter runs");

    // The starter waits 20 seconds for health it will never see; the marker
    // appears as soon as it has resolved and executed the rendered path.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while std::time::Instant::now() < deadline && !started.is_file() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        started.is_file(),
        "the starter did not execute the binary its paths file names",
    );
}

/// Every UTF-8 file under `root`, with its path, so an assertion can sweep the
/// whole deployment rather than a list of files someone remembered.
fn text_files(root: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("the deployment is readable") {
            let path = entry.expect("the entry is readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(text) = fs::read_to_string(&path) {
                files.push((path, text));
            }
        }
    }
    files
}

#[test]
fn a_materialized_endpoint_reaches_every_consumer_of_the_default_literal() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    let source = stage_bundle(root);
    let endpoint = dead_endpoint();
    assert_ne!(endpoint.url(), DEFAULT_ENDPOINT_URL);

    let deployment = materialize(
        Population::Tree(&source),
        &root.join("data/deployments"),
        &root.join("data/bin/appa"),
        &root.join("config/appa.toml"),
        &root.join("data"),
        &endpoint,
    )
    .expect("the deployment materializes");

    // The hooks learn the endpoint by sourcing the rendered paths file.
    let sourced = Command::new("sh")
        .arg("-c")
        .arg(". \"$1\" && printf '%s' \"$APPA_ENDPOINT\"")
        .arg("sh")
        .arg(deployment.root.join("plugin/hooks/appa-paths.sh"))
        .env("APPA_ENDPOINT", DEFAULT_ENDPOINT_URL)
        .output()
        .expect("the paths file sources");
    assert_eq!(String::from_utf8_lossy(&sourced.stdout), endpoint.url());

    // The MCP registration and the statusline read the literal themselves.
    let mcp: serde_json::Value =
        serde_json::from_slice(&fs::read(deployment.root.join("plugin/.mcp.json")).expect("the MCP registration"))
            .expect("the MCP registration parses");
    assert_eq!(
        mcp["mcpServers"]["appa"]["url"],
        format!("${{APPA_RUNTIME_URL:-{}}}/mcp", endpoint.url())
    );
    let statusline = fs::read_to_string(deployment.root.join("plugin/statusline.sh")).expect("the statusline");
    assert!(statusline.contains(endpoint.url()));

    let stale: Vec<_> = text_files(&deployment.root)
        .into_iter()
        .filter(|(_, text)| text.contains(DEFAULT_ENDPOINT_URL))
        .map(|(path, _)| path)
        .collect();
    assert!(stale.is_empty(), "the default endpoint survived rendering in {stale:?}");
}
