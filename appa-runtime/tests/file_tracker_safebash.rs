//! SafeBash through its MCP server and the real sandbox runtime: a command sees the files it
//! declares and nothing else of the user's, writes only its outputs, and reaches no host it
//! did not declare. Skipped where the sandbox runtime `srt` is not installed.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn safebash(config: &std::path::Path, arguments: serde_json::Value) -> (String, bool) {
    let mut server = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["file-tracker", "mcp", "--config"])
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.stdout.take().unwrap());
    for request in [
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "SafeBash", "arguments": arguments } }),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    let mut line = String::new();
    let response = loop {
        line.clear();
        stdout.read_line(&mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if response["id"] == 2 {
            break response;
        }
    };
    drop(stdin);
    server.wait().unwrap();
    let result = &response["result"];
    (
        result["content"][0]["text"].as_str().unwrap().to_string(),
        result["isError"] == true,
    )
}

#[test]
fn a_command_sees_only_what_it_declares() {
    if Command::new("srt").arg("--version").output().is_err() {
        eprintln!("skipped: the sandbox runtime srt is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let config = root.join("file-tracker.toml");
    std::fs::write(
        &config,
        format!("store = {:?}\n", root.join("store.json").display().to_string()),
    )
    .unwrap();
    let input = root.join("in.txt");
    let other = root.join("other.txt");
    let output = root.join("out.txt");
    std::fs::write(&input, "declared\n").unwrap();
    std::fs::write(&other, "undeclared\n").unwrap();
    std::fs::write(&output, "stale\n").unwrap();

    let command = format!(
        "cat {input}; cat {other} || echo OTHER-HIDDEN; tr a-z A-Z < {input} > {output}; \
         echo x > {root}/new.txt; \
         curl -sS -m 5 https://example.com >/dev/null 2>&1 || echo NET-REFUSED",
        input = input.display(),
        other = other.display(),
        output = output.display(),
        root = root.display(),
    );
    let (text, failed) = safebash(
        &config,
        serde_json::json!({ "command": command, "input_files": [input], "output_files": [output], "network_hosts": [] }),
    );
    assert!(!failed, "{text}");
    assert!(text.contains("declared\n"), "{text}");
    assert!(!text.contains("undeclared"), "an undeclared file is hidden: {text}");
    for refused in ["OTHER-HIDDEN", "NET-REFUSED"] {
        assert!(text.contains(refused), "{refused}: {text}");
    }
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "DECLARED\n");
    assert!(
        !root.join("new.txt").exists(),
        "an undeclared write never reaches the disk"
    );

    // An undeclared file as an input is refused before anything runs.
    let (text, failed) = safebash(
        &config,
        serde_json::json!({ "command": "true", "input_files": [root.join("missing")], "output_files": [], "network_hosts": [] }),
    );
    assert!(failed && text.contains("refused"), "{text}");
}
