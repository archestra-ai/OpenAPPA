//! SafeBash: one shell command, run in a sandbox confined to the files and hosts its call
//! declares — the same values the annotator labeled. The command reads only its
//! `input_files` and the system outside the user's files, writes only its `output_files`, and
//! reaches only its `network_hosts`. Anything
//! it does not declare is invisible to it, so its output carries no label it did not name.
//!
//! Two sandboxes enforce it, and the file-tracker builds neither. Where `clappa` confines
//! Claude Code's own `Bash` per call ([`sandbox`] answers each call's confinement), Claude
//! Code's built-in sandbox runs the command. Everywhere else, SafeBash runs it in Anthropic's
//! sandbox runtime (`srt`) — the runtime Claude Code links into its Bash tool — whose proxy
//! admits the declared hosts.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::config::{Config, Role, ToolRole};
use crate::paths::{Named, named};
use crate::store::Locked;

/// How much of each stream a result carries.
const STREAM_LIMIT: usize = 64 * 1024;

pub const DESCRIPTION: &str = "Runs one shell command in a sandbox. The command can read only the files listed in \
    input_files (plus system directories such as /usr and /etc; other directories such as /opt, /srv and /home are \
    hidden), write only the files listed in output_files, and \
    connect only to the hosts listed in network_hosts. Use absolute paths. Each output file is emptied before the \
    command runs. Declare every file the command reads or writes: any other file in your workspace, home or /tmp is \
    hidden from it, so ls and find there show only what the call declares. To find files, use the Glob tool, then \
    declare the ones the command needs.";

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "The shell command to run with bash -c." },
            "input_files": { "type": "array", "items": { "type": "string" }, "description": "Absolute paths of the files the command reads." },
            "output_files": { "type": "array", "items": { "type": "string" }, "description": "Absolute paths of the files the command writes." },
            "network_hosts": { "type": "array", "items": { "type": "string" }, "description": "Hosts the command connects to: example.com, *.example.com, or host:port." }
        },
        "required": ["command", "input_files", "output_files", "network_hosts"]
    })
}

/// The command's result as the model reads it, or why it was refused before it ran.
pub fn run(config: &Config, arguments: &Value) -> Result<String, String> {
    let command = arguments["command"].as_str().ok_or("command is a string")?;
    let role = ToolRole {
        name: "SafeBash".to_string(),
        role: Role::Run,
        path: None,
    };
    let named = named(&role, arguments)?;
    prepare(&named)?;
    // The sandbox runtime opens its proxy's sockets under its temporary directory, which the
    // command must reach; the temporary directories it would otherwise use stay hidden. This
    // one is the call's own and holds nothing of the user's.
    let scratch = std::env::temp_dir().join(format!("appa-safebash-{}-{}", std::process::id(), crate::store::now()));
    std::fs::create_dir(&scratch).map_err(|error| format!("{}: {error}", scratch.display()))?;
    let settings = settings(config, &named, Some(&scratch))?;
    let file = tempfile_in_temp(&settings)?;
    let (program, prefix) = config
        .sandbox
        .command
        .split_first()
        .ok_or("the sandbox command is empty")?;
    let mut sandbox = Command::new(program);
    sandbox
        .args(prefix)
        .arg("--settings")
        .arg(&file)
        .arg("-c")
        .arg(format!("{}{command}", emptying(&named)))
        .env("TMPDIR", &scratch);
    let finished = supervise(sandbox, config.sandbox.timeout_secs);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(finished?.report(config.sandbox.timeout_secs, ""))
}

/// The confinement of one Claude Code `Bash` call, for `clappa`'s preload to hand to Claude
/// Code's built-in sandbox: the same filesystem and network settings SafeBash gives `srt`,
/// and in `prelude` the line the preload runs ahead of the command to empty its outputs, as
/// SafeBash does. A call that does not name all three lists, or names an input that is not a
/// regular file, is refused.
pub fn sandbox(config: &Config, arguments: &Value) -> Result<Value, String> {
    let role = ToolRole {
        name: "Bash".to_string(),
        role: Role::Run,
        path: None,
    };
    let named = named(&role, arguments)?;
    prepare(&named)?;
    let mut settings = settings(config, &named, None)?;
    settings["prelude"] = json!(emptying(&named));
    Ok(settings)
}

/// Check what a call names and create its absent outputs, so the sandbox can open each for
/// writing: every input an existing regular file, every output a regular file or absent. An
/// existing output keeps its content until the command starts — [`emptying`].
fn prepare(named: &Named) -> Result<(), String> {
    for input in &named.reads {
        if !std::fs::metadata(input).is_ok_and(|metadata| metadata.is_file()) {
            return Err(match std::fs::metadata(input).is_ok_and(|metadata| metadata.is_dir()) {
                true => format!(
                    "input {} is a directory: declare the files the command reads, and find them with the Glob tool",
                    input.display()
                ),
                false => format!("input {} is not a regular file", input.display()),
            });
        }
    }
    for output in &named.writes {
        if std::fs::metadata(output).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(format!("output {} is not a regular file", output.display()));
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output)
            .map_err(|error| format!("{}: {error}", output.display()))?;
    }
    Ok(())
}

/// The shell line that empties a call's outputs, run inside the sandbox ahead of the command:
/// an output is emptied only once the command has started, never by a call that was refused
/// or could not start. The command's output then derives from its declared inputs alone.
fn emptying(named: &Named) -> String {
    if named.writes.is_empty() {
        return String::new();
    }
    let quoted = |path: &PathBuf| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let empties: Vec<String> = named
        .writes
        .iter()
        .map(|path| format!(": >| {}", quoted(path)))
        .collect();
    format!("{{ {}; }} || exit 126\n", empties.join(" && "))
}

/// How a sandboxed command ended.
struct Finished {
    /// The exit status, or none when the command ran past its time.
    status: Option<std::process::ExitStatus>,
    stdout: String,
    stderr: String,
}

impl Finished {
    /// The result as the model reads it, `note` appended.
    fn report(&self, timeout_secs: u64, note: &str) -> String {
        let code = match self.status {
            Some(status) => status
                .code()
                .map_or("killed by a signal".to_string(), |code| code.to_string()),
            None => format!("timed out after {timeout_secs}s"),
        };
        format!(
            "exit: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}{note}",
            self.stdout, self.stderr
        )
    }
}

/// Run `command` with no input, its streams captured, and kill it after `timeout_secs`.
fn supervise(mut command: Command, timeout_secs: u64) -> Result<Finished, String> {
    let program = command.get_program().to_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the sandbox {program:?} cannot start: {error}"))?;
    let stdout = drain(child.stdout.take().expect("stdout is piped"));
    let stderr = drain(child.stderr.take().expect("stderr is piped"));
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(Finished {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// The top-level directories a command reads as the system: its programs, libraries and
/// configuration. Every other top-level entry is hidden.
const SYSTEM: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/etc",
    "/dev",
    "/proc",
    "/sys",
    "/run",
    "/nix",
    "/snap",
    "/System",
    "/Library",
    "/Applications",
    "/private",
];

/// Every top-level entry of `/` that is not [`SYSTEM`]. A symbolic link is left to its
/// target, which is a top-level entry of its own or under one.
fn outside_the_system(root: impl IntoIterator<Item = (PathBuf, bool)>) -> Vec<PathBuf> {
    root.into_iter()
        .filter(|(path, link)| !link && !SYSTEM.iter().any(|system| Path::new(system) == path))
        .map(|(path, _)| path)
        .collect()
}

/// The sandbox settings for one call. Reads are hidden everywhere but the system — every
/// other top-level directory, and within the system's reach home, the working directory, the
/// temporary directories and every file the store has recorded — and reopened for the inputs
/// and `[sandbox] readable` alone; writes are allowed to the outputs alone; the proxy admits
/// the declared hosts alone. `scratch`, when given, is `srt`'s own temporary directory,
/// readable so the command reaches the proxy.
fn settings(config: &Config, named: &Named, scratch: Option<&Path>) -> Result<Value, String> {
    let entries = std::fs::read_dir("/").map_err(|error| format!("/: {error}"))?;
    let mut hidden = outside_the_system(entries.filter_map(Result::ok).map(|entry| {
        let link = entry.file_type().is_ok_and(|kind| kind.is_symlink());
        (entry.path(), link)
    }));
    hidden.extend(["/tmp", "/var/tmp"].iter().map(PathBuf::from));
    hidden.extend(std::env::var_os("HOME").map(PathBuf::from));
    hidden.extend(std::env::current_dir().ok());
    hidden.extend(config.store.parent().map(Path::to_path_buf));
    {
        let locked = Locked::open(&config.store)?;
        hidden.extend(
            locked
                .store
                .files
                .keys()
                .filter(|path| !named.reads.contains(path))
                .cloned(),
        );
    }
    let mut readable: Vec<PathBuf> = named.reads.clone();
    readable.extend(config.sandbox.readable.iter().cloned());
    readable.extend(scratch.map(Path::to_path_buf));
    Ok(json!({
        "filesystem": {
            "denyRead": hidden,
            "allowRead": readable,
            "allowWrite": named.writes,
            "denyWrite": ["/tmp/claude"],
        },
        "network": {
            "allowedDomains": named.hosts,
            "deniedDomains": [],
        },
    }))
}

fn tempfile_in_temp(settings: &Value) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "appa-safebash-{}-{}.json",
        std::process::id(),
        crate::store::now()
    ));
    std::fs::write(&path, serde_json::to_vec(settings).expect("settings serialize"))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

fn drain(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        // Keep one byte past the limit, to know the stream ran over it, and read the rest
        // into a fixed buffer that is dropped, so the command never blocks on a full pipe.
        let mut kept = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let room = (STREAM_LIMIT + 1).saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
        }
        let over = kept.len() > STREAM_LIMIT;
        kept.truncate(STREAM_LIMIT);
        let mut text = String::from_utf8_lossy(&kept).into_owned();
        if over {
            text.push_str("\n[truncated]");
        }
        text
    })
}

/// SafeBash as an MCP server on stdio: newline-delimited JSON-RPC, one tool.
pub fn serve(config: &Config) -> Result<(), String> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let result = match message["method"].as_str() {
            Some("initialize") => Ok(json!({
                "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "file-tracker", "version": env!("CARGO_PKG_VERSION") },
            })),
            Some("ping") => Ok(json!({})),
            Some("tools/list") => Ok(json!({
                "tools": [{ "name": "SafeBash", "description": DESCRIPTION, "inputSchema": schema() }]
            })),
            Some("tools/call") if message["params"]["name"] == "SafeBash" => {
                // A command that ran is a success whatever its exit code: its output reaches
                // the model, and the harness reports an error result as a failed call, whose
                // label the engine never applies. Only a refusal, which ran nothing and
                // carries no file content, is an error.
                let (text, failed) = match run(config, &message["params"]["arguments"]) {
                    Ok(text) => (text, false),
                    Err(refusal) => (format!("SafeBash refused: {refusal}"), true),
                };
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": failed }))
            }
            _ => Err(json!({ "code": -32601, "message": "method not found" })),
        };
        let response = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        writeln!(stdout, "{response}")
            .and_then(|()| stdout.flush())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_top_level_entry_but_the_system_is_hidden() {
        let listing = ["/usr", "/etc", "/opt", "/srv", "/home", "/data", "/bin", "/var"]
            .map(|path| (PathBuf::from(path), path == "/bin"));
        assert_eq!(
            outside_the_system(listing),
            ["/opt", "/srv", "/home", "/data", "/var"].map(PathBuf::from).to_vec()
        );
    }
}
