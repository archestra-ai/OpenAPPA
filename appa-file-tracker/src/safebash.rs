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

use crate::annotate::{Concealing, concealed, guarded, recorded_label};
use crate::config::{Config, Role, ToolRole};
use crate::paths::{Named, covers, named, tree};
use crate::store::Locked;

/// How much of each stream a result carries.
const STREAM_LIMIT: usize = 64 * 1024;

pub const DESCRIPTION: &str = "Runs one shell command in a sandbox. The command can read only the files, \
    directories and Unix sockets listed in input_files (plus system directories such as /usr and /etc, the \
    directories on PATH and installed toolchains), write only the files and directories listed in output_files, and \
    connect only to the hosts listed in network_hosts. Use absolute paths. An output file that exists is read as \
    well, so a command may rewrite it. An output written with a trailing / is a directory: the command reads what it holds and writes \
    under it, as a build directory, node_modules/ or a cache needs (for example ~/.cache/go-build/). A command that \
    reaches a Go, cargo or npm registry keeps what it downloads in that package manager's cache, which every later \
    command reads without declaring it. A credential \
    file such as .env inside a declared directory stays hidden unless the call names it or git tracks it. Declare every \
    file the \
    command reads or writes: any other file in your workspace, home or /tmp is hidden from it, so ls and find there \
    show only what the call declares. $TMPDIR is a private directory for this call alone, removed when it ends. To \
    list a directory, declare it in input_files. Declare a socket such as \
    /var/run/docker.sock or $SSH_AUTH_SOCK in input_files to reach the service behind it.";

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
    let placeholders = prepare(&named)?;
    // The call's own scratch directory holds nothing of the user's. Under `srt` the sandbox
    // runtime opens its proxy's sockets, which the command must reach; under `tmp` the
    // command keeps its temporary files, gone when the call ends.
    let scratch = Removed(unique("appa-safebash"));
    let (runtime, temporary) = (scratch.0.join("srt"), scratch.0.join("tmp"));
    for dir in [&runtime, &temporary] {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    let mut settings = settings(config, &named, Concealing::Restricted)?;
    push(&mut settings["filesystem"]["allowRead"], &runtime);
    push(&mut settings["filesystem"]["allowWrite"], &temporary);
    let file = Removed(tempfile_in_temp(&settings)?);
    let (program, prefix) = config
        .sandbox
        .command
        .split_first()
        .ok_or("the sandbox command is empty")?;
    let mut sandbox = Command::new(program);
    sandbox
        .args(prefix)
        .arg("--settings")
        .arg(&file.0)
        .arg("-c")
        .arg(command)
        .env("TMPDIR", &runtime)
        .env("CLAUDE_CODE_TMPDIR", &temporary)
        .env_remove("CLAUDE_TMPDIR");
    for name in hidden_environment(&named) {
        sandbox.env_remove(name);
    }
    for (name, value) in identity() {
        if std::env::var_os(name).is_none() {
            sandbox.env(name, value);
        }
    }
    if let Some(missing) = missing(config) {
        return Err(missing);
    }
    let finished = supervise(sandbox, config.sandbox.timeout_secs)?;
    let failed = !finished.status.is_some_and(|status| status.success());
    let removed = unwritten(&placeholders, failed);
    let note = match removed.is_empty() || !failed {
        true => String::new(),
        false => format!("\n{}", unwritten_note(&removed)),
    };
    Ok(finished.report(config.sandbox.timeout_secs, &note))
}

/// What the model reads when a command failed and left outputs it was to create untouched:
/// each was an empty file while the command ran, which a tool may refuse, and is gone again.
fn unwritten_note(removed: &[PathBuf]) -> String {
    let names: Vec<String> = removed.iter().map(|path| path.display().to_string()).collect();
    format!(
        "[appa] {} did not exist, so the command found an empty file in its place; it is removed again. A tool that refuses an empty file, such as uv with uv.lock, needs the file's directory declared with a trailing / in output_files instead.",
        names.join(", ")
    )
}

/// Why the sandbox `config` names cannot start here, when it cannot: its program is not on
/// `PATH`, or not at the path it names.
pub fn missing(config: &Config) -> Option<String> {
    let program = Path::new(config.sandbox.command.first()?);
    let found = match program.components().count() > 1 {
        true => program.is_file(),
        false => std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file())),
    };
    match (found, program == Path::new("srt")) {
        (true, _) => None,
        (false, true) => Some(
            "SafeBash runs shell commands in Anthropic's sandbox runtime `srt`, which is not installed; \
             install it with: npm install -g @anthropic-ai/sandbox-runtime"
                .to_string(),
        ),
        (false, false) => Some(format!("SafeBash's sandbox {} is not installed", program.display())),
    }
}

/// A call's own scratch directory or settings file, removed however the call returns.
struct Removed(PathBuf);

/// A path under the temporary directory that no other call names.
fn unique(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn push(list: &mut Value, path: &Path) {
    list.as_array_mut()
        .expect("the settings hold their lists")
        .push(json!(path));
}

impl Drop for Removed {
    fn drop(&mut self) {
        let _ = match self.0.is_dir() {
            true => std::fs::remove_dir_all(&self.0),
            false => std::fs::remove_file(&self.0),
        };
    }
}

/// The confinement of one Claude Code `Bash` call, for `clappa`'s preload to hand to Claude
/// Code's built-in sandbox: the same filesystem and network settings SafeBash gives `srt`,
/// in `tmpdir` the call's own temporary directory, which the preload removes when the call
/// ends, and in `prelude` the lines the preload runs ahead of the command to point `TMPDIR`
/// there and give git the person's identity. A call that does not name all three lists,
/// or names an input that is not a file, a directory or a socket, is refused.
///
/// A command the person types at the prompt (`typed`) reaches no hook, so nothing labels
/// its result: it reads and writes the working directory, or the git repository that holds
/// it, reaches no host, and sees no file whose label restricts its audience, so its result
/// needs none.
///
/// In `placeholders` are the absent outputs created for the sandbox to open, each with its
/// modification time; the preload removes each the command did not write.
pub fn sandbox(config: &Config, arguments: &Value) -> Result<Value, String> {
    let role = ToolRole {
        name: "Bash".to_string(),
        role: Role::Run,
        path: None,
    };
    let (named, concealing) = match arguments["typed"].as_bool() == Some(true) {
        true => {
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            let cwd = repository_root(&cwd).unwrap_or(cwd).display().to_string();
            let typed = json!({ "input_files": [cwd], "output_files": [format!("{cwd}/")], "network_hosts": [] });
            (named(&role, &typed)?, Concealing::Labeled)
        }
        false => (named(&role, arguments)?, Concealing::Restricted),
    };
    let placeholders = prepare(&named)?;
    let temporary = unique("appa-bash");
    std::fs::create_dir_all(&temporary).map_err(|error| format!("{}: {error}", temporary.display()))?;
    let mut settings = settings(config, &named, concealing)?;
    push(&mut settings["filesystem"]["allowWrite"], &temporary);
    settings["tmpdir"] = json!(temporary);
    let identity: String = identity()
        .into_iter()
        .map(|(name, value)| {
            let value = value.replace(['"', '\\', '$', '`'], "");
            format!("export {name}=\"${{{name}:-{value}}}\"\n")
        })
        .collect();
    settings["prelude"] = json!(format!("export TMPDIR={}\n{identity}", quoted(&temporary)));
    settings["hiddenEnvironment"] = json!(hidden_environment(&named));
    settings["placeholders"] = placeholders
        .iter()
        .map(|(path, created)| {
            let nanos = created
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos());
            json!({ "path": path, "modified": nanos.to_string() })
        })
        .collect();
    Ok(settings)
}

/// The root of the git work tree that holds `cwd`, the nearest directory at or above it with
/// a `.git`, when that lies above `cwd` and does not hold the home: a command typed in a
/// subdirectory of a repository works on the repository, as git there does.
fn repository_root(cwd: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    cwd.ancestors()
        .take_while(|dir| !home.as_ref().is_some_and(|home| covers(dir, home)))
        .find(|dir| dir.join(".git").exists())
        .filter(|root| *root != cwd)
        .map(Path::to_path_buf)
}

/// The placeholders a command did not write, each removed, so the output stays absent as it
/// was before the call: every one still empty when the command `failed`, which a tool such as
/// uv may have opened and refused, and otherwise each left empty and untouched, so a file the
/// command made empty on purpose stays.
fn unwritten(placeholders: &[(PathBuf, std::time::SystemTime)], failed: bool) -> Vec<PathBuf> {
    placeholders
        .iter()
        .filter(|(path, created)| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.len() == 0
                    && (failed || metadata.modified().ok().as_ref() == Some(created))
            })
        })
        .filter(|(path, _)| std::fs::remove_file(path).is_ok())
        .map(|(path, _)| path.clone())
        .collect()
}

/// Check what a call names and create its absent outputs, so the sandbox can open each for
/// writing: every input an existing file, directory or socket, every output a regular file,
/// a directory, or absent. Each output it creates is returned with its modification time.
fn prepare(named: &Named) -> Result<Vec<(PathBuf, std::time::SystemTime)>, String> {
    let mut placeholders = Vec::new();
    for directory in &named.directories {
        std::fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    }
    for input in &named.reads {
        match std::fs::metadata(input) {
            Ok(metadata) if metadata.is_file() || metadata.is_dir() || is_socket(&metadata.file_type()) => {}
            Ok(_) => {
                return Err(format!(
                    "input {} is not a file, a directory or a socket",
                    input.display()
                ));
            }
            Err(error) => return Err(format!("input {}: {error}", input.display())),
        }
    }
    for output in named.writes.iter().filter(|output| !named.directories.contains(output)) {
        if std::fs::metadata(output).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(format!(
                "output {} is not a regular file: write a directory with a trailing /",
                output.display()
            ));
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        // An absent output is created for the sandbox to open; the file it leaves is an
        // ordinary one. A program a command builds is declared by its directory.
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o644);
        let absent = std::fs::symlink_metadata(output).is_err();
        let file = options
            .open(output)
            .map_err(|error| format!("{}: {error}", output.display()))?;
        if absent && let Ok(created) = file.metadata().and_then(|metadata| metadata.modified()) {
            placeholders.push((output.clone(), created));
        }
    }
    Ok(placeholders)
}

/// The variables that give git the person's name and email, which it reads from `~/.gitconfig`:
/// the file stays hidden, since it may hold credentials, and only the identity crosses.
fn identity() -> Vec<(&'static str, String)> {
    let setting = |key: &str| {
        Command::new("git")
            .args(["config", "--global", "--get", key])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let mut found = Vec::new();
    for (key, names) in [
        ("user.name", ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"]),
        ("user.email", ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"]),
    ] {
        if let Some(value) = setting(key) {
            found.extend(names.map(|name| (name, value.clone())));
        }
    }
    found
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(unix)]
fn is_socket(kind: &std::fs::FileType) -> bool {
    std::os::unix::fs::FileTypeExt::is_socket(kind)
}

#[cfg(not(unix))]
fn is_socket(_: &std::fs::FileType) -> bool {
    false
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

/// How many files recorded with a label that a call reads uncounted a toolchain or a download
/// cache may hold and still open to it: each is hidden by a mount of its own, and the sandbox
/// takes a bounded number of them.
const TOOLCHAIN_HOLES: usize = 500;

/// The system directories a credential can live in, walked for the files a path rule labels.
const SYSTEM_CONFIGURATION: &[&str] = &["/etc", "/run", "/private/etc"];

/// Every regular file under [`SYSTEM_CONFIGURATION`] that a path rule labels, and with
/// `sockets` every socket there: the system stays readable, its credentials do not, at any
/// depth, and a service is reached only through a socket the call declares. Symbolic links
/// are not followed, so the walk ends; a linked file is left to its target.
fn system_files_to_hide(config: &Config, sockets: bool) -> Vec<PathBuf> {
    let labeled: Vec<_> = config
        .paths
        .iter()
        .filter(|rule| rule.trust.is_some() || rule.audience.is_some())
        .collect();
    let mut found = Vec::new();
    let mut pending: Vec<PathBuf> = SYSTEM_CONFIGURATION.iter().map(PathBuf::from).collect();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if (sockets && is_socket(&kind))
                || (kind.is_file() && labeled.iter().any(|rule| rule.matches(&path)))
            {
                found.push(path);
            }
        }
    }
    found
}

/// The environment variables a command never sees: by name, the ones that hold credentials —
/// a word of the name such as `TOKEN`, `SECRET`, `PASSWORD`, `KEY` or `AUTH`, and every
/// `ANTHROPIC_` and `APPA_` variable. A command's environment is no declared input, so what
/// it would carry is withheld rather than labeled. A variable that holds the path of a
/// declared input, as `SSH_AUTH_SOCK` holds a declared agent socket, carries only that path
/// and reaches the command.
pub fn hidden_environment(named: &Named) -> Vec<String> {
    const WORDS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "PASS",
        "CREDENTIAL",
        "CREDENTIALS",
        "KEY",
        "APIKEY",
        "AUTH",
        "COOKIE",
        "SESSION",
    ];
    let declared = |value: &std::ffi::OsStr| {
        let path = value.to_string_lossy();
        let path = path.strip_prefix("unix://").unwrap_or(&path);
        crate::paths::resolve(path).is_ok_and(|path| named.reads.contains(&path))
    };
    std::env::vars_os()
        .filter(|(_, value)| !declared(value))
        .filter_map(|(name, _)| name.into_string().ok())
        .filter(|name| {
            let upper = name.to_ascii_uppercase();
            upper.starts_with("ANTHROPIC_")
                || upper.starts_with("APPA_")
                || upper.split('_').any(|word| WORDS.contains(&word))
        })
        .collect()
}

/// Every top-level entry of `/` that is not [`SYSTEM`]. A symbolic link is left to its
/// target, which is a top-level entry of its own or under one.
fn outside_the_system(root: impl IntoIterator<Item = (PathBuf, bool)>) -> Vec<PathBuf> {
    root.into_iter()
        .filter(|(path, link)| !link && !SYSTEM.iter().any(|system| Path::new(system) == path))
        .map(|(path, _)| path)
        .collect()
}

/// The directories a command reads beside its inputs and the system: the directories on
/// `PATH`, the installed toolchain each `bin` among them belongs to — a Go, Python or Node
/// installation reads its own library beside its programs — `[sandbox] readable`, and the
/// package managers' download caches that exist. Each is assumed to hold no credentials and
/// no instructions: no path rule applies under it. A directory that is the home or the
/// working directory, holds one of them or the store, or lies under the working directory is
/// not opened, and neither is a directory directly under the home, such as `~/.cargo`, which
/// holds the person's own configuration beside the programs.
pub fn toolchains(config: &Config) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let guarded: Vec<PathBuf> = home
        .iter()
        .chain(&cwd)
        .cloned()
        .chain(config.store.parent().map(Path::to_path_buf))
        .collect();
    let opens = |dir: &Path| {
        dir.is_absolute()
            && dir.is_dir()
            && !SYSTEM.iter().any(|system| covers(Path::new(system), dir))
            && !guarded.iter().any(|guard| covers(dir, guard))
            && !cwd.as_ref().is_some_and(|cwd| covers(cwd, dir))
    };
    let mut found: Vec<PathBuf> = Vec::new();
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    for dir in on_path {
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !opens(&dir) {
            continue;
        }
        let root = dir
            .parent()
            .filter(|_| dir.file_name().is_some_and(|name| name == "bin"))
            .filter(|root| opens(root) && root.parent() != home.as_deref());
        found.push(root.map_or(dir.clone(), Path::to_path_buf));
    }
    // A readable path or a cache that holds the home, the working directory or the store, as
    // a variable set to the home would name, opens nothing.
    let caches = crate::paths::caches().into_iter().map(|(cache, _)| cache);
    found.extend(
        config
            .sandbox
            .readable
            .iter()
            .cloned()
            .chain(caches.filter(|cache| cache.is_dir()))
            .filter(|dir| !guarded.iter().any(|guard| covers(dir, guard))),
    );
    found.sort();
    found.dedup();
    found
}

/// The parts of `root` a command reads when `holes` must stay out of its sight: `root` whole
/// when no hole lies in it, nothing when it is one, and otherwise the same of each entry it
/// holds. A symbolic link among the entries is left out, since what it names is not `root`'s.
fn around(root: &Path, holes: &[PathBuf]) -> Vec<PathBuf> {
    if holes.iter().any(|hole| hole == root) {
        return Vec::new();
    }
    if !holes.iter().any(|hole| covers(root, hole)) {
        return vec![root.to_path_buf()];
    }
    std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| !kind.is_symlink()))
        .flat_map(|entry| around(&entry.path(), holes))
        .collect()
}

/// The sandbox settings for one call. Reads are hidden everywhere but the system — every
/// other top-level directory, and within the system's reach home and the working directory
/// unless a declared input holds them, the temporary directories, every socket and labeled
/// file under `/etc` and `/run`, and every file the store has recorded with a label — and
/// reopened for the inputs, the toolchains, `[sandbox] readable` and the download caches
/// alone. Within a directory the call reads, what [`concealed`] names stays hidden unless the
/// call names it, so the command sees exactly what the annotation labeled. Writes are allowed
/// to the outputs alone, and within a directory output not to what [`guarded`] keeps. The
/// proxy admits the declared hosts alone, and a command connects to the sockets it declares
/// alone.
fn settings(config: &Config, named: &Named, concealing: Concealing) -> Result<Value, String> {
    let locked = Locked::open(&config.store)?;
    let store = &locked.store;
    let declared = |path: &PathBuf| named.reads.iter().any(|read| covers(read, path));
    let listed = |dir: &str| -> Vec<(PathBuf, bool)> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| (entry.path(), entry.file_type().is_ok_and(|kind| kind.is_symlink())))
            .collect()
    };
    // `/var` is hidden entry by entry, so its links into the system — `/var/run`, through
    // which programs reach the system's sockets — keep working.
    let mut hidden = outside_the_system(listed("/").into_iter().filter(|(path, _)| path != Path::new("/var")));
    hidden.extend(
        listed("/var")
            .into_iter()
            .filter(|(_, link)| !link)
            .map(|(path, _)| path),
    );
    hidden.extend(["/tmp", "/var/tmp"].iter().map(PathBuf::from));
    hidden.extend(std::fs::canonicalize(std::env::temp_dir()));
    let sockets: Vec<&PathBuf> = named
        .reads
        .iter()
        .filter(|path| std::fs::metadata(path).is_ok_and(|metadata| is_socket(&metadata.file_type())))
        .collect();
    hidden.extend(
        system_files_to_hide(config, !sockets.is_empty())
            .into_iter()
            .filter(|path| !declared(path)),
    );
    // A declared directory that holds the home or the working directory opens it: the
    // sandbox would otherwise mount an empty directory over it, and git there would find
    // no repository above it.
    hidden.extend(
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .into_iter()
            .chain(std::env::current_dir().ok())
            .filter(|path| !declared(path)),
    );
    let store_dir = config.store.parent().map(Path::to_path_buf);
    hidden.extend(store_dir.clone());
    // A recorded file the call does not declare carries a label no annotation counted: it is
    // hidden unless its label is one `concealing` lets a command read uncounted, so a
    // toolchain or a download cache shows what recorded writes left there unlabeled.
    let unseen: Vec<PathBuf> = store
        .files
        .iter()
        .filter(|(path, recorded)| !declared(path) && concealing.hides(&recorded.label, &store.ranks))
        .map(|(path, _)| path.clone())
        .collect();
    // A toolchain opens to a call that does not declare it around each such file in it, a
    // mount each, so one that holds more than [`TOOLCHAIN_HOLES`] stays closed, and so does
    // one under a recorded directory whose label it hides.
    let closed = |toolchain: &PathBuf| {
        concealing.hides(&recorded_label(store, toolchain), &store.ranks)
            || unseen.iter().filter(|path| covers(toolchain, path)).count() > TOOLCHAIN_HOLES
    };

    let toolchains = toolchains(config);
    let covered = |path: &PathBuf| toolchains.iter().any(|toolchain| covers(toolchain, path));
    let mut secret = Vec::new();
    for read in named.reads.iter().filter(|read| read.is_dir() && !covered(read)) {
        secret.extend(concealed(config, store, read, &tree(read)?, &named.reads, concealing));
    }
    let mut kept = Vec::new();
    for directory in named.directories.iter().filter(|directory| !covered(directory)) {
        kept.extend(guarded(config, &tree(directory)?, &named.writes));
    }
    // A read directory is opened around what it conceals and around the outputs in it, which
    // are opened for writing: a writable path inside one opened read-only stays read-only.
    let holes: Vec<PathBuf> = secret.iter().chain(&named.writes).chain(&store_dir).cloned().collect();
    let mut readable: Vec<PathBuf> = named
        .reads
        .iter()
        .filter(|read| !named.writes.contains(read))
        .flat_map(|read| around(read, &holes))
        .collect();
    // Nor does a toolchain open that another call, whose records have not settled, may be
    // writing, and a download cache the call writes without declaring it is written only
    // where it opens.
    readable.extend(
        toolchains
            .iter()
            .filter(|toolchain| {
                let writes = named
                    .writes
                    .iter()
                    .any(|write| covers(toolchain, write) || covers(write, toolchain));
                !closed(toolchain) && (writes || store.holder(toolchain).is_none())
            })
            .cloned(),
    );
    readable.sort();
    readable.dedup();
    let writable: Vec<&PathBuf> = named
        .writes
        .iter()
        .filter(|write| named.reads.contains(write) || !named.directories.contains(write) || !closed(write))
        .collect();
    hidden.extend(unseen);
    hidden.extend(secret);
    let mut unwritable = vec![PathBuf::from("/tmp/claude")];
    unwritable.extend(kept);
    let mut network = json!({
        "allowedDomains": named.hosts,
        "deniedDomains": [],
    });
    if !sockets.is_empty() {
        // Seatbelt admits each socket by its path. Linux filters sockets by family only, so a
        // command reaches every socket it can see: the sockets it does not declare are hidden.
        match cfg!(target_os = "macos") {
            true => network["allowUnixSockets"] = json!(sockets),
            false => network["allowAllUnixSockets"] = json!(true),
        }
    }
    Ok(json!({
        "filesystem": {
            "denyRead": hidden,
            "allowRead": readable,
            "allowWrite": writable,
            "denyWrite": unwritable,
        },
        "network": network,
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

    /// A cache full of files that path rules match, such as a crate's test certificates and
    /// `.env` examples, opens as one mount for every kind of call, declared or not: a typed
    /// command once got a mount around each such file, past what bubblewrap accepts. Only a
    /// file a recorded write labeled stays hidden from a call that does not declare the
    /// cache, and a call that declares it carries that label; a cache holding more such
    /// files than the sandbox takes mounts stays closed instead.
    #[test]
    fn a_cache_opens_whole_and_hides_only_what_was_recorded_with_a_label() {
        use crate::label::{Label, Ranks};
        use crate::store::{Recorded, version};
        let dir = unique("appa-ft-cache");
        let cache = dir.join("cache");
        for index in 0..500 {
            let tests = cache.join(format!("crate-{index}/tests"));
            std::fs::create_dir_all(&tests).unwrap();
            std::fs::write(tests.join("cert.pem"), "test certificate").unwrap();
            std::fs::write(cache.join(format!("crate-{index}/.env.example")), "KEY=example").unwrap();
        }
        let config_path = dir.join("file-tracker.toml");
        std::fs::write(
            &config_path,
            format!(
                "store = {:?}\n[sandbox]\nreadable = [{:?}]\n",
                dir.join("store.json").display().to_string(),
                cache.display().to_string()
            ),
        )
        .unwrap();
        let config = Config::load(Some(&config_path)).unwrap();
        let (private, public) = (cache.join("crate-0/private.txt"), cache.join("crate-1/public.txt"));
        let mut locked = Locked::open(&config.store).unwrap();
        locked.store.ranks = Ranks(vec!["suspicious".to_string(), "trusted".to_string()]);
        for (path, audience) in [(&private, vec![vec!["self".to_string()]]), (&public, vec![])] {
            std::fs::write(path, "downloaded").unwrap();
            let label = Label {
                trust: Some("trusted".to_string()),
                audience,
            };
            let version = version(path);
            locked.store.files.insert(path.clone(), Recorded { label, version });
        }
        locked.save().unwrap();

        let under = |settings: &Value, list: &str| -> Vec<PathBuf> {
            let paths = settings["filesystem"][list].as_array().unwrap().iter();
            paths
                .map(|path| PathBuf::from(path.as_str().unwrap()))
                .filter(|path| covers(&cache, path))
                .collect()
        };
        let calls = [
            (json!({ "command": "true", "typed": true }), vec![private.clone()]),
            (
                json!({ "input_files": [], "output_files": [], "network_hosts": [] }),
                vec![private.clone()],
            ),
            (
                json!({ "input_files": [cache], "output_files": [], "network_hosts": [] }),
                vec![],
            ),
        ];
        for (call, hidden) in calls {
            let settings = sandbox(&config, &call).unwrap();
            let _ = std::fs::remove_dir_all(settings["tmpdir"].as_str().unwrap());
            assert_eq!(under(&settings, "allowRead"), std::slice::from_ref(&cache), "{call}");
            assert_eq!(under(&settings, "denyRead"), hidden, "{call}");
        }

        let request = json!({
            "kind": "annotation",
            "artifact": { "args": {
                "name": "host/claude-code/Bash",
                "arguments": { "command": "true", "input_files": [cache], "output_files": [], "network_hosts": [] },
            }},
            "declaration": { "trust_ranks": ["suspicious", "trusted"], "audiences": ["self"] },
        });
        let answer = crate::annotate::annotate(&config, &request).unwrap();
        assert_eq!(answer["answer"]["delta"]["audience"], json!(["self"]));

        // Past the mounts a cache may take, it stays closed to a call that does not declare it.
        let mut locked = Locked::open(&config.store).unwrap();
        for index in 0..TOOLCHAIN_HOLES {
            let recorded = locked.store.files[&private].clone();
            locked
                .store
                .files
                .insert(cache.join(format!("crate-{index}/more")), recorded);
        }
        locked.save().unwrap();
        let call = json!({ "input_files": [], "output_files": [], "network_hosts": [] });
        let settings = sandbox(&config, &call).unwrap();
        let _ = std::fs::remove_dir_all(settings["tmpdir"].as_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(under(&settings, "allowRead"), Vec::<PathBuf>::new());
    }
}
