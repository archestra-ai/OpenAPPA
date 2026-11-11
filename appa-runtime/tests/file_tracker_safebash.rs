//! SafeBash through its MCP server and the real sandbox runtime: a command sees the files it
//! declares and nothing else of the user's, writes only its outputs, and reaches no host it
//! did not declare. Skipped where the sandbox runtime `srt` is not installed.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn safebash(config: &std::path::Path, arguments: serde_json::Value) -> (String, bool) {
    let mut server = Command::new(env!("CARGO_BIN_EXE_appa"))
        .args(["file-tracker", "mcp", "--config"])
        .arg(config)
        .env("PROBE_SERVICE_TOKEN", "environment-secret")
        .env("TMPDIR", scratch_of(config))
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

/// The temporary directory SafeBash runs with beside `config`: its own, so a test sees every
/// scratch file a call leaves.
fn scratch_of(config: &std::path::Path) -> std::path::PathBuf {
    let scratch = config.parent().unwrap().join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    scratch
}

fn installed(program: &str) -> bool {
    let found = Command::new(program).arg("--version").output().is_ok();
    if !found {
        eprintln!("skipped: {program} is not installed");
    }
    found
}

#[test]
fn a_command_sees_only_what_it_declares() {
    if !installed("srt") {
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
         echo x > {root}/new.txt; printenv PROBE_SERVICE_TOKEN || echo ENV-HIDDEN; \
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
    assert!(
        !text.contains("environment-secret"),
        "a credential variable is withheld: {text}"
    );
    for refused in ["OTHER-HIDDEN", "NET-REFUSED", "ENV-HIDDEN"] {
        assert!(text.contains(refused), "{refused}: {text}");
    }
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "DECLARED\n");
    assert!(
        !root.join("new.txt").exists(),
        "an undeclared write never reaches the disk"
    );

    // A command that exits non-zero ran, and its output reaches the model, so the result is
    // no error: the harness reports an error result as a failed call, whose label the engine
    // never applies. An input stays read-only.
    let (text, failed) = safebash(
        &config,
        serde_json::json!({
            "command": format!("cat {input}; echo tamper > {input}; exit 3", input = input.display()),
            "input_files": [input], "output_files": [output], "network_hosts": [],
        }),
    );
    assert!(
        !failed && text.contains("exit: 3") && text.contains("declared"),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&input).unwrap(), "declared\n");

    // A declared host is reached through the sandbox's proxy, while the temporary
    // directories stay hidden. Skipped where this machine cannot reach it either.
    let reachable = Command::new("curl")
        .args(["-sS", "-m", "10", "-o", "/dev/null", "https://example.com"])
        .status()
        .is_ok_and(|status| status.success());
    if reachable {
        let (text, failed) = safebash(
            &config,
            serde_json::json!({
                "command": "curl -sS -m 10 -o /dev/null -w 'status=%{http_code}' https://example.com",
                "input_files": [], "output_files": [], "network_hosts": ["example.com"],
            }),
        );
        assert!(!failed && text.contains("status=200"), "{text}");
    }

    // An input that does not exist is read as nothing: a probe for it runs and finds nothing.
    let missing = root.join("missing");
    let (text, failed) = safebash(
        &config,
        serde_json::json!({
            "command": format!("test -e {} || echo ABSENT", missing.display()),
            "input_files": [missing], "output_files": [], "network_hosts": [],
        }),
    );
    assert!(!failed && text.contains("ABSENT"), "{text}");
}

/// What `clappa`'s preload hands Claude Code's sandbox for one Bash call: the declared input
/// alone readable inside the hidden home, working and temporary directories, the declared
/// output alone writable and emptied, the declared host alone reachable, and nothing for a
/// call that declares no lists.
#[test]
fn a_bash_call_is_confined_to_what_it_declares() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let config = root.join("file-tracker.toml");
    std::fs::write(
        &config,
        format!("store = {:?}\n", root.join("store.json").display().to_string()),
    )
    .unwrap();
    let input = root.join("in.txt");
    let output = root.join("out.txt");
    std::fs::write(&input, "declared\n").unwrap();
    std::fs::write(&output, "stale\n").unwrap();
    let confine = |arguments: serde_json::Value| {
        let mut tracker = Command::new(env!("CARGO_BIN_EXE_appa"))
            .args(["file-tracker", "sandbox", "--config"])
            .arg(&config)
            .current_dir(&root)
            .env("PROBE_SERVICE_TOKEN", "environment-secret")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(tracker.stdin.take().unwrap(), "{arguments}").unwrap();
        let output = tracker.wait_with_output().unwrap();
        (output.status.success(), String::from_utf8(output.stdout).unwrap())
    };

    let (ok, answer) = confine(serde_json::json!({
        "command": "cat in.txt > out.txt", "input_files": [input], "output_files": [output],
        "network_hosts": ["Example.COM"],
    }));
    assert!(ok, "{answer}");
    let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
    let paths = |field: &str| answer["filesystem"][field].clone();
    for hidden in ["/tmp", std::env::var("HOME").unwrap().as_str(), root.to_str().unwrap()] {
        assert!(
            paths("denyRead").as_array().unwrap().iter().any(|path| path == hidden),
            "{hidden} is hidden"
        );
    }
    for root_entry in ["/opt", "/srv", "/mnt", "/media", "/home", "/root"] {
        let path = std::path::Path::new(root_entry);
        if path.symlink_metadata().is_ok_and(|metadata| metadata.is_dir()) {
            assert!(
                paths("denyRead")
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|hidden| hidden == root_entry),
                "{root_entry}, outside the system, is hidden"
            );
        }
    }
    assert!(
        !paths("denyRead")
            .as_array()
            .unwrap()
            .iter()
            .any(|hidden| hidden == "/usr" || hidden == "/etc"),
        "the system stays readable"
    );
    let readable = paths("allowRead");
    let readable: Vec<&str> = readable
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    assert!(readable.contains(&input.to_str().unwrap()), "{readable:?}");
    let home = std::env::var("HOME").unwrap();
    for opened in &readable {
        let opened = std::path::Path::new(opened);
        assert!(
            opened == input || !(opened.starts_with(&root) || std::path::Path::new(&home).starts_with(opened)),
            "{} opens the working directory or the home",
            opened.display()
        );
    }
    let withheld = answer["hiddenEnvironment"].as_array().unwrap();
    assert!(
        withheld.iter().any(|name| name == "PROBE_SERVICE_TOKEN"),
        "{withheld:?}"
    );
    assert!(
        !withheld.iter().any(|name| name == "PATH" || name == "HOME"),
        "{withheld:?}"
    );
    let temporary = answer["tmpdir"].as_str().unwrap();
    assert!(
        std::path::Path::new(temporary).is_dir(),
        "the call has its own temporary directory"
    );
    assert_eq!(paths("allowWrite"), serde_json::json!([output, temporary]));
    assert_eq!(answer["network"]["allowedDomains"], serde_json::json!(["example.com"]));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "stale\n",
        "an output keeps its content until the command starts"
    );
    let prelude = answer["prelude"].as_str().unwrap();
    assert!(
        prelude.contains(&format!(": >| '{}'", output.display())),
        "the command empties its output first: {prelude}"
    );
    assert!(
        prelude.starts_with(&format!("export TMPDIR='{temporary}'")),
        "{prelude}"
    );
    std::fs::remove_dir(temporary).unwrap();

    let (ok, _) = confine(serde_json::json!({ "command": "cat in.txt" }));
    assert!(!ok, "a call that declares no lists has no confinement");
}

/// A command that cannot start leaves every declared output exactly as it was: the outputs are
/// emptied inside the sandbox, never by a call that did not run.
#[test]
fn a_command_that_cannot_start_leaves_its_outputs_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let config = root.join("file-tracker.toml");
    std::fs::write(
        &config,
        format!(
            "store = {:?}\n[sandbox]\ncommand = [{:?}]\ntimeout_secs = 5\n",
            root.join("store.json").display().to_string(),
            root.join("no-such-sandbox").display().to_string(),
        ),
    )
    .unwrap();
    let output = root.join("report.md");
    std::fs::write(&output, "weeks of work\n").unwrap();
    let (text, failed) = safebash(
        &config,
        serde_json::json!({ "command": "echo new > report.md", "input_files": [], "output_files": [output], "network_hosts": [] }),
    );
    assert!(failed && text.contains("is not installed"), "{text}");
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "weeks of work\n");
}

/// A call refused after SafeBash made its scratch directory and settings file leaves neither
/// behind.
#[test]
fn a_call_refused_during_setup_leaves_no_scratch_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let config = root.join("file-tracker.toml");
    std::fs::write(
        &config,
        format!(
            "store = {:?}\n[sandbox]\ncommand = []\ntimeout_secs = 5\n",
            root.join("store.json").display().to_string(),
        ),
    )
    .unwrap();
    let (text, failed) = safebash(
        &config,
        serde_json::json!({ "command": "true", "input_files": [], "output_files": [], "network_hosts": [] }),
    );
    assert!(failed && text.contains("empty"), "{text}");
    let left: Vec<_> = std::fs::read_dir(scratch_of(&config)).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

/// What everyday development needs: a service reached through the socket the call declares
/// and no other, a build written under a declared directory, and a temporary directory of the
/// call's own that is gone when the call ends.
#[cfg(unix)]
#[test]
fn a_command_reaches_its_declared_socket_and_writes_under_its_declared_directory() {
    if !installed("srt") || !installed("python3") {
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
    let socket = root.join("service.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = (&stream).write_all(b"pong");
        }
    });
    let connect = format!(
        "python3 -c \"import socket; s = socket.socket(socket.AF_UNIX); s.connect('{}'); print(s.recv(4).decode())\"",
        socket.display()
    );

    let (text, failed) = safebash(
        &config,
        serde_json::json!({ "command": connect, "input_files": [socket], "output_files": [], "network_hosts": [] }),
    );
    assert!(!failed && text.contains("pong"), "a declared socket is reached: {text}");
    let (text, _) = safebash(
        &config,
        serde_json::json!({ "command": connect, "input_files": [], "output_files": [], "network_hosts": [] }),
    );
    assert!(!text.contains("pong"), "an undeclared socket is not: {text}");

    let build = root.join("build");
    let (text, failed) = safebash(
        &config,
        serde_json::json!({
            "command": format!(
                "mkdir -p {build}/obj && t=$(mktemp) && echo built > $t && cp $t {build}/obj/app && cat {build}/obj/app",
                build = build.display()
            ),
            "input_files": [], "output_files": [format!("{}/", build.display())], "network_hosts": [],
        }),
    );
    assert!(!failed && text.contains("built"), "{text}");
    assert_eq!(std::fs::read_to_string(build.join("obj/app")).unwrap(), "built\n");
    assert_eq!(
        std::fs::read_dir(scratch_of(&config)).unwrap().count(),
        0,
        "the call's temporary directory is gone"
    );
}
