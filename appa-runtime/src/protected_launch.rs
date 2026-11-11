//! The process boundary behind `clappa`: launch one gated Claude process,
//! remember the last resumable session it ended, and preserve its exit status.

use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};
use uuid::Uuid;

use crate::hook_client::{FORKED_FROM_VAR, LAUNCH_VAR as LAUNCH, PEER_ADDRESS_VAR, session_is_gated};

const GATE: &str = "APPA_GATE";
const MESSAGING_FLAG: &str = "--messaging-socket-path";
/// Agent view moves a session into a background daemon whose environment is not this
/// launch's, so a protected session would run on without the gate or under another launch.
const DISABLE_AGENT_VIEW: &str = "CLAUDE_CODE_DISABLE_AGENT_VIEW";

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Event {
    Start,
    End,
}

/// Run Claude in the foreground. This function terminates the host process so
/// it can preserve a child's signal death as well as an ordinary exit code.
pub fn launch(settings: &Path, data_dir: &Path, arguments: &[OsString]) -> ! {
    let fork = match Fork::of(arguments) {
        Ok(fork) => fork,
        Err(refusal) => {
            eprintln!("clappa: {refusal}");
            std::process::exit(2);
        }
    };
    let token = Uuid::new_v4();
    let launches = data_dir.join("launches");
    let session = launches.join(token.to_string());
    if let Err(error) = fs::create_dir_all(&launches) {
        eprintln!("clappa: cannot prepare {}: {error}", launches.display());
        std::process::exit(1);
    }
    cleanup(&session);
    let messaging = messaging(&token, arguments);

    #[cfg(unix)]
    let signals = Signals::install();
    let mut command = Command::new("claude");
    let shell = match bound(&crate::init::paths::installed_config_path()) {
        true => shell(settings, data_dir, &token),
        false => Shell::Untracked,
    };
    command.args(shell.arguments(settings));
    command.envs(shell.environment());
    if let Some(messaging) = &messaging {
        command
            .arg(MESSAGING_FLAG)
            .arg(&messaging.socket)
            .env(PEER_ADDRESS_VAR, &messaging.address);
    }
    match &fork {
        Fork::Of(session) => command.env(FORKED_FROM_VAR, session),
        Fork::None => command.env_remove(FORKED_FROM_VAR),
    };
    let child = command
        .args(arguments)
        .env(GATE, "1")
        .env(LAUNCH, token.to_string())
        .env(DISABLE_AGENT_VIEW, "1")
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            cleanup(&session);
            close(messaging.as_ref());
            eprintln!("clappa: cannot start Claude Code: {error}");
            std::process::exit(1);
        }
    };
    #[cfg(unix)]
    signals.child(child.id());
    let status = child.wait();
    #[cfg(unix)]
    drop(signals);

    let status = match status {
        Ok(status) => status,
        Err(error) => {
            cleanup(&session);
            close(messaging.as_ref());
            eprintln!("clappa: cannot wait for Claude Code: {error}");
            std::process::exit(1);
        }
    };
    let mut stdout = std::io::stdout();
    print_resume(&session, stdout.is_terminal(), &mut stdout);
    cleanup(&session);
    shell.clean();
    close(messaging.as_ref());
    terminate_as(status)
}

/// The tools a file-tracking session never offers: they run commands, or read or write file
/// content, out of the file-tracker's sight. The policy blocks them at the hook as well.
const UNMEDIATED_TOOLS: &str = "PowerShell,Monitor,Grep,NotebookEdit";

/// The Claude Code releases whose internals the preload has been checked against. A release
/// outside this list runs SafeBash, even when the self-test would pass.
const NATIVE_BASH_RELEASES: &[&str] = &["2.1.266"];

/// How long the preload's self-test may take before the session falls back to SafeBash.
const SELF_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How a session runs shell commands.
#[derive(Debug, PartialEq, Eq)]
enum Shell {
    /// The deployment's policy binds no file-tracker: Claude Code runs as configured.
    Untracked,
    /// Claude Code's own Bash, confined per call in its built-in sandbox by the preload, with
    /// `settings` the launch's settings plus the sandbox the preload needs.
    Native { preload: PathBuf, settings: PathBuf },
    /// Bash disabled and SafeBash served by the file-tracker in its place, in `srt`.
    SafeBash,
}

impl Shell {
    /// Claude Code's arguments. Each flag carries its value after `=`, so no list can take in
    /// the person's own arguments.
    fn arguments(&self, settings: &Path) -> Vec<OsString> {
        match self {
            Shell::Untracked => vec!["--settings".into(), settings.into()],
            Shell::Native { settings, .. } => vec![
                "--settings".into(),
                settings.into(),
                format!("--disallowedTools={UNMEDIATED_TOOLS}").into(),
            ],
            Shell::SafeBash => vec![
                "--settings".into(),
                settings.into(),
                format!("--disallowedTools=Bash,{UNMEDIATED_TOOLS}").into(),
                format!(
                    "--mcp-config={}",
                    serde_json::json!({ "mcpServers": { "file-tracker": { "command": executable(), "args": ["file-tracker", "mcp"] } } })
                )
                .into(),
            ],
        }
    }

    fn environment(&self) -> Vec<(&'static str, OsString)> {
        match self {
            Shell::Native { preload, .. } => vec![
                ("BUN_OPTIONS", format!("--preload {}", preload.display()).into()),
                ("APPA_FILE_TRACKER", executable().into()),
            ],
            Shell::Untracked | Shell::SafeBash => Vec::new(),
        }
    }

    fn clean(&self) {
        if let Shell::Native { settings, .. } = self {
            let _ = fs::remove_file(settings);
        }
    }
}

/// Whether the deployment's policy binds the `file-tracker` recorder. A config that does not
/// read binds nothing: the runtime loads the same file and refuses what it cannot read.
fn bound(config: &Path) -> bool {
    fs::read_to_string(config)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .is_some_and(|table| {
            table
                .get("externals")
                .and_then(|externals| externals.get("recorders"))
                .and_then(|recorders| recorders.get("file-tracker"))
                .is_some()
        })
}

/// This launcher is the `appa` executable, which serves the file-tracker too.
fn executable() -> String {
    std::env::current_exe().map_or_else(|_| "appa".into(), |path| path.display().to_string())
}

/// Native Bash when this Claude Code release is one the preload was checked against and the
/// preload's self-test passes on it; SafeBash otherwise, saying why.
fn shell(settings: &Path, data_dir: &Path, token: &Uuid) -> Shell {
    match native(settings, data_dir, token) {
        Ok(shell) => shell,
        Err(reason) => {
            eprintln!("clappa: shell commands run as SafeBash: {reason}");
            Shell::SafeBash
        }
    }
}

fn native(settings: &Path, data_dir: &Path, token: &Uuid) -> Result<Shell, String> {
    let version = crate::child_process::output(Command::new("claude").arg("--version"))
        .map_err(|error| format!("cannot run claude --version: {error}"))?;
    let release = String::from_utf8_lossy(&version.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    if !NATIVE_BASH_RELEASES.contains(&release.as_str()) {
        return Err(format!(
            "Claude Code {release} is not a release the Bash preload was checked against ({})",
            NATIVE_BASH_RELEASES.join(", ")
        ));
    }
    let preload = data_dir.join("claude-bash.js");
    let staged = data_dir.join(format!("claude-bash.js.{token}"));
    fs::write(&staged, appa_file_tracker::CLAUDE_BASH_PRELOAD)
        .and_then(|()| fs::rename(&staged, &preload))
        .map_err(|error| format!("cannot write {}: {error}", preload.display()))?;
    self_test(&preload)?;
    let mut merged: serde_json::Map<String, Value> = fs::read_to_string(settings)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    merged.insert("sandbox".into(), sandbox_settings());
    let launch_settings = data_dir.join("launches").join(format!("{token}.settings.json"));
    fs::write(&launch_settings, Value::Object(merged).to_string())
        .map_err(|error| format!("cannot write {}: {error}", launch_settings.display()))?;
    Ok(Shell::Native {
        preload,
        settings: launch_settings,
    })
}

/// The sandbox the preload confines each Bash call in. A command that cannot run in it is
/// refused, never run outside it; a host outside its call's list is refused, never asked
/// about.
fn sandbox_settings() -> Value {
    serde_json::json!({
        "enabled": true,
        "failIfUnavailable": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": { "strictAllowlist": true },
    })
}

/// Run the preload's self-test in a Claude Code process that exits before it starts a
/// session, with settings of its own that only enable the sandbox.
fn self_test(preload: &Path) -> Result<(), String> {
    let home = tempfile::tempdir().map_err(|error| error.to_string())?;
    fs::write(
        home.path().join("settings.json"),
        serde_json::json!({ "sandbox": sandbox_settings() }).to_string(),
    )
    .map_err(|error| error.to_string())?;
    let mut test = Command::new("claude");
    test.arg("--version")
        .env("CLAUDE_CONFIG_DIR", home.path())
        .env("BUN_OPTIONS", format!("--preload {}", preload.display()))
        .env("APPA_FILE_TRACKER", executable())
        .env("APPA_CLAUDE_BASH_SELFTEST", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child =
        crate::child_process::spawn(&mut test).map_err(|error| format!("cannot run the self-test: {error}"))?;
    let deadline = std::time::Instant::now() + SELF_TEST_TIMEOUT;
    while child.try_wait().map_err(|error| error.to_string())?.is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the Bash preload's self-test timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    let report = String::from_utf8_lossy(&output.stdout);
    let line = report.lines().find(|line| line.starts_with("appa-claude-bash: "));
    match (output.status.success(), line) {
        (true, Some("appa-claude-bash: ok")) => Ok(()),
        (_, Some(line)) => Err(format!(
            "the Bash preload's self-test failed: {}",
            &line["appa-claude-bash: ".len()..]
        )),
        (_, None) => Err("the Bash preload's self-test did not run".into()),
    }
}

/// The session a launch forks, as Claude Code's own arguments name it.
#[derive(Debug, PartialEq, Eq)]
enum Fork {
    None,
    Of(String),
}

impl Fork {
    /// A fork copies a conversation APPA can carry the label of only when the launch names
    /// it: `--continue` and the resume picker pick it inside Claude Code, out of this
    /// launcher's sight.
    fn of(arguments: &[OsString]) -> Result<Self, String> {
        let arguments: Vec<&str> = arguments.iter().filter_map(|argument| argument.to_str()).collect();
        if !arguments.contains(&"--fork-session") {
            return Ok(Self::None);
        }
        let resumed = arguments.iter().enumerate().find_map(|(at, argument)| match *argument {
            "--resume" | "-r" => arguments
                .get(at + 1)
                .filter(|id| !id.starts_with('-'))
                .map(|id| id.to_string()),
            argument => argument.strip_prefix("--resume=").map(str::to_string),
        });
        match resumed.filter(|id| !id.is_empty()) {
            Some(session) => Ok(Self::Of(session)),
            None => Err("--fork-session under OpenAPPA needs the session it forks: --resume <session-id>".to_string()),
        }
    }
}

/// The messaging endpoint `clappa` gives one Claude process, and the address
/// its peers send to.
struct Messaging {
    socket: PathBuf,
    address: OsString,
}

/// `None` when the user chose their own endpoint, or when no private one can
/// be made; the session then starts without a peer address.
fn messaging(token: &Uuid, arguments: &[OsString]) -> Option<Messaging> {
    if chooses_own_endpoint(arguments) {
        return None;
    }
    match endpoint(token) {
        Ok(messaging) => Some(messaging),
        Err(reason) => {
            eprintln!("clappa: peer messages are off for this session: {reason}");
            None
        }
    }
}

fn chooses_own_endpoint(arguments: &[OsString]) -> bool {
    arguments.iter().any(|argument| {
        argument.to_str().is_some_and(|argument| {
            argument == MESSAGING_FLAG
                || argument
                    .strip_prefix(MESSAGING_FLAG)
                    .is_some_and(|rest| rest.starts_with('='))
        })
    })
}

fn endpoint_name(token: &Uuid) -> String {
    token.simple().to_string()[..12].to_owned()
}

#[cfg(unix)]
fn endpoint(token: &Uuid) -> Result<Messaging, String> {
    const MAX_SOCKET_PATH: usize = 104;
    let socket = private_socket_dir()?.join(format!("{}.sock", endpoint_name(token)));
    if socket.as_os_str().len() >= MAX_SOCKET_PATH {
        return Err(format!("{} is longer than a Unix socket path allows", socket.display()));
    }
    let mut address = OsString::from("uds:");
    address.push(&socket);
    Ok(Messaging { socket, address })
}

#[cfg(unix)]
fn private_socket_dir() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let uid = unsafe { libc::getuid() };
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("appa"),
        None => PathBuf::from(format!("/tmp/appa-{uid}")),
    };
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("cannot create {}: {error}", dir.display())),
    }
    let metadata = fs::symlink_metadata(&dir).map_err(|error| format!("cannot inspect {}: {error}", dir.display()))?;
    match (metadata.is_dir(), metadata.uid() == uid, metadata.mode() & 0o777) {
        (true, true, 0o700) => Ok(dir),
        _ => Err(format!(
            "{} is not a directory private to this user (mode 0700)",
            dir.display()
        )),
    }
}

// Unverified: the address form Claude Code uses for a named pipe is assumed.
#[cfg(windows)]
fn endpoint(token: &Uuid) -> Result<Messaging, String> {
    let name = format!("appa-{}", endpoint_name(token));
    Ok(Messaging {
        socket: PathBuf::from(format!(r"\\.\pipe\{name}")),
        address: OsString::from(format!("pipe:{name}")),
    })
}

fn close(messaging: Option<&Messaging>) {
    #[cfg(unix)]
    if let Some(messaging) = messaging
        && let Err(error) = remove_file(&messaging.socket)
    {
        eprintln!("clappa: cannot remove the session's messaging socket: {error}");
    }
    #[cfg(not(unix))]
    let _ = messaging;
}

/// Record one lifecycle edge without contacting or starting the runtime.
pub fn record(data_dir: &Path, event: Event) -> ExitCode {
    if !session_is_gated() {
        return ExitCode::SUCCESS;
    }
    let Some(token) = std::env::var_os(LAUNCH).and_then(|token| token.into_string().ok()) else {
        return ExitCode::SUCCESS;
    };
    let Ok(token) = Uuid::parse_str(&token) else {
        return ExitCode::SUCCESS;
    };
    let path = data_dir.join("launches").join(token.to_string());
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().take(1024 * 1024).read_to_end(&mut input) {
        cleanup(&path);
        eprintln!("appa record-launch: cannot read the hook event: {error}");
        return ExitCode::FAILURE;
    }
    match record_event(&path, event, &input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            cleanup(&path);
            eprintln!("appa record-launch: cannot record the session: {error}");
            ExitCode::FAILURE
        }
    }
}

fn record_event(path: &Path, event: Event, input: &[u8]) -> std::io::Result<()> {
    if event == Event::Start {
        return remove_file(path);
    }
    let hook: Value = match serde_json::from_slice(input) {
        Ok(hook) => hook,
        Err(_) => return remove_file(path),
    };
    let session = hook
        .get("session_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok());
    let transcript = hook.get("transcript_path").and_then(Value::as_str).map(PathBuf::from);
    match (session, transcript) {
        (Some(session), Some(transcript)) if transcript.is_file() => fs::write(path, session.to_string()),
        _ => remove_file(path),
    }
}

fn print_resume(path: &Path, terminal: bool, output: &mut impl Write) {
    let session = fs::read_to_string(path)
        .ok()
        .and_then(|session| Uuid::parse_str(session.trim()).ok());
    if terminal && let Some(session) = session {
        let _ = write!(
            output,
            "\nResume with OpenAPPA protection:\nclappa --resume {session}\n"
        );
        let _ = output.flush();
    }
}

fn remove_file(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn cleanup(path: &Path) {
    if let Err(error) = remove_file(path) {
        eprintln!("clappa: cannot remove {}: {error}", path.display());
    }
}

#[cfg(unix)]
fn terminate_as(status: ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(not(unix))]
fn terminate_as(status: ExitStatus) -> ! {
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(unix)]
struct Signals {
    term: libc::sighandler_t,
    hup: libc::sighandler_t,
}

#[cfg(unix)]
static CHILD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// A signal not yet forwarded. One that arrives before the child's pid is stored waits here
/// for [`Signals::child`] to send it, so it reaches Claude Code rather than being dropped.
#[cfg(unix)]
static PENDING: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Forward `signal` to the child. The signal is recorded before the pid is read, and whichever
/// of this handler and [`Signals::child`] takes the record back sends it: a signal that lands
/// while the pid is stored reaches the child exactly once.
#[cfg(unix)]
extern "C" fn forward(signal: libc::c_int) {
    use std::sync::atomic::Ordering::SeqCst;
    PENDING.store(signal, SeqCst);
    let child = CHILD.load(SeqCst);
    if child > 0 {
        send(child, PENDING.swap(0, SeqCst));
    }
}

/// Send `signal` to `child` unless it is the empty record.
#[cfg(unix)]
fn send(child: libc::pid_t, signal: libc::c_int) {
    if signal != 0 {
        unsafe { libc::kill(child, signal) };
    }
}

#[cfg(unix)]
impl Signals {
    fn install() -> Self {
        unsafe {
            Self {
                term: libc::signal(libc::SIGTERM, forward as *const () as libc::sighandler_t),
                hup: libc::signal(libc::SIGHUP, forward as *const () as libc::sighandler_t),
            }
        }
    }

    /// Forward later signals to `pid`, and one that arrived before it was known.
    fn child(&self, pid: u32) {
        use std::sync::atomic::Ordering::SeqCst;
        let pid = pid as libc::pid_t;
        CHILD.store(pid, SeqCst);
        send(pid, PENDING.swap(0, SeqCst));
    }
}

#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        CHILD.store(0, std::sync::atomic::Ordering::SeqCst);
        PENDING.store(0, std::sync::atomic::Ordering::SeqCst);
        unsafe {
            libc::signal(libc::SIGTERM, self.term);
            libc::signal(libc::SIGHUP, self.hup);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped Claude Code policy launches Claude Code without its unmediated tools and
    /// with SafeBash; a policy that binds no file-tracker launches it unchanged.
    #[test]
    fn only_a_policy_binding_the_file_tracker_disables_the_unmediated_tools() {
        let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../marketplace/plugins/claude-code");
        assert!(bound(&plugin.join("default.appa.toml")));
        let untracked = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(untracked.path(), "[policy]\nversion = 2\n").unwrap();
        assert!(!bound(untracked.path()));

        let settings = Path::new("hooks.json");
        let fallback = Shell::SafeBash.arguments(settings);
        assert_eq!(
            fallback[2],
            "--disallowedTools=Bash,PowerShell,Monitor,Grep,NotebookEdit"
        );
        let servers: Value =
            serde_json::from_str(fallback[3].to_str().unwrap().strip_prefix("--mcp-config=").unwrap()).unwrap();
        assert_eq!(
            servers["mcpServers"]["file-tracker"]["args"],
            serde_json::json!(["file-tracker", "mcp"])
        );
        let native = Shell::Native {
            preload: "claude-bash.js".into(),
            settings: "launch.json".into(),
        };
        assert_eq!(
            native.arguments(settings),
            [
                "--settings",
                "launch.json",
                "--disallowedTools=PowerShell,Monitor,Grep,NotebookEdit"
            ]
            .map(OsString::from)
        );
        assert!(Shell::Untracked.environment().is_empty());
    }

    /// A signal that lands between spawning Claude Code and storing its pid reaches it once the
    /// pid is stored: the launcher never waits on a child that missed its termination.
    #[cfg(unix)]
    #[test]
    fn a_signal_before_the_child_is_known_reaches_it_once_it_is() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = Command::new("sleep").arg("30").spawn().expect("sleep starts");
        let signals = Signals {
            term: libc::SIG_DFL,
            hup: libc::SIG_DFL,
        };
        forward(libc::SIGTERM);
        signals.child(child.id());
        assert_eq!(child.wait().expect("the child is reaped").signal(), Some(libc::SIGTERM));
    }

    fn arguments(spelled: &[&str]) -> Vec<OsString> {
        spelled.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_fork_names_the_session_it_resumes_or_is_refused() {
        assert_eq!(Fork::of(&arguments(&["--resume", "abc"])), Ok(Fork::None));
        assert_eq!(
            Fork::of(&arguments(&["--resume", "abc", "--fork-session"])),
            Ok(Fork::Of("abc".to_string()))
        );
        assert_eq!(
            Fork::of(&arguments(&["--fork-session", "-r", "abc"])),
            Ok(Fork::Of("abc".to_string()))
        );
        assert_eq!(
            Fork::of(&arguments(&["--resume=abc", "--fork-session", "-p", "hi"])),
            Ok(Fork::Of("abc".to_string()))
        );
        for refused in [
            &["--continue", "--fork-session"][..],
            &["--resume", "--fork-session"],
            &["--fork-session"],
        ] {
            assert!(Fork::of(&arguments(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn end_records_only_a_session_with_an_existing_transcript() {
        let root = tempfile::tempdir().unwrap();
        let transcript = root.path().join("transcript.jsonl");
        let launch = root.path().join("launch");
        let session = Uuid::new_v4();
        fs::write(&transcript, "").unwrap();
        let event = serde_json::json!({"session_id": session, "transcript_path": transcript});
        record_event(&launch, Event::End, event.to_string().as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&launch).unwrap(), session.to_string());

        fs::remove_file(&transcript).unwrap();
        record_event(&launch, Event::End, event.to_string().as_bytes()).unwrap();
        assert!(!launch.exists(), "an empty session removes an earlier resumable id");
    }

    #[test]
    fn either_spelling_of_the_users_messaging_flag_is_their_own_endpoint() {
        let arguments = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(chooses_own_endpoint(&arguments(&["--messaging-socket-path", "/s"])));
        assert!(chooses_own_endpoint(&arguments(&["--messaging-socket-path=/s"])));
        assert!(!chooses_own_endpoint(&arguments(&[
            "--messaging-socket-pathx",
            "--resume"
        ])));
    }

    #[test]
    fn start_removes_the_previous_session() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        fs::write(&launch, Uuid::new_v4().to_string()).unwrap();
        record_event(&launch, Event::Start, br#"{"ignored":true}"#).unwrap();
        assert!(!launch.exists());
    }

    #[test]
    fn resume_block_requires_both_a_session_file_and_a_terminal() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        let session = Uuid::new_v4();
        let mut output = Vec::new();
        print_resume(&launch, true, &mut output);
        assert!(output.is_empty());

        fs::write(&launch, session.to_string()).unwrap();
        print_resume(&launch, false, &mut output);
        assert!(output.is_empty());
        print_resume(&launch, true, &mut output);
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("\nResume with OpenAPPA protection:\nclappa --resume {session}\n")
        );
    }
}
