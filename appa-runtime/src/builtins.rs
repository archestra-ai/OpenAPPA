//! Builtin implementations: the stock answers, the `claude-code` model transport, and
//! the module loader.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use appa_builtin::{ABI_VERSION, DescriptorV1, KIND_AUTHORITY, KIND_SANITIZER};

use crate::config::{CLAUDE_CODE_BUILTIN, LLM_BUILTIN, Section};
use crate::consult::{Consult, ConsultBody, ModelPrompt};
use crate::external::NoAnswerReason;

/// The output-buffer bound for one module answer: the configured
/// `max_body_bytes`, but never more than this. Applies to module calls
/// only — the HTTP path's cap behavior is untouched.
pub(crate) const MODULE_OUTPUT_CEILING: usize = 16 * 1024 * 1024;

const REFUSED_MODULE_NAMES: [&str; 6] = [
    "hitl",
    "attest-schema",
    "approve",
    "redact-email",
    CLAUDE_CODE_BUILTIN,
    LLM_BUILTIN,
];

/// The implementation-name grammar: 1..=64 bytes of ASCII lowercase
/// kebab (`^[a-z0-9][a-z0-9-]*$`), matched exactly, never normalized.
pub(crate) fn valid_implementation_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let body_char = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
    (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) && bytes.iter().all(body_char)
}

/// The stock in-process implementations, each of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stock {
    /// `approve` — an authority that approves every call it is asked about.
    Approve,
    /// `redact-email` — a sanitizer replacing every email-like token in the body with a
    /// fixed placeholder. A deliberately simple scan (registration is a trust decision,
    /// not verification).
    RedactEmail,
}

impl Stock {
    pub(crate) fn for_section(section: Section, name: &str) -> Option<Stock> {
        match (section, name) {
            (Section::Authorities, "approve") => Some(Stock::Approve),
            (Section::Sanitizers, "redact-email") => Some(Stock::RedactEmail),
            _ => None,
        }
    }

    /// The answer object, shaped as the wire shapes it, so a component switched between
    /// backends yields identical evidence. A consult of another kind yields no answer.
    pub(crate) fn answer(self, consult: &Consult) -> Option<serde_json::Value> {
        match (self, &consult.body) {
            (Stock::Approve, ConsultBody::Authority { .. }) => Some(serde_json::json!({ "ruling": "approve" })),
            (Stock::RedactEmail, ConsultBody::Sanitizer { artifact, .. }) => {
                Some(serde_json::json!({ "body": redact_email(&artifact.body) }))
            }
            _ => None,
        }
    }
}

fn redact_email(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut word = String::new();
    for ch in input.chars() {
        if ch.is_whitespace() {
            flush_word(&mut word, &mut out);
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    flush_word(&mut word, &mut out);
    out
}

fn flush_word(word: &mut String, out: &mut String) {
    if is_emailish(word) {
        out.push_str("[redacted-email]");
    } else {
        out.push_str(word);
    }
    word.clear();
}

fn is_emailish(token: &str) -> bool {
    match token.find('@') {
        Some(at) => {
            let (local, rest) = token.split_at(at);
            let domain = &rest[1..];
            !local.is_empty() && !domain.starts_with('.') && domain.contains('.')
        }
        None => false,
    }
}

/// Which of the two module-capable kinds a module implements. Casts,
/// dynamic resolvers, and membership take no module — a descriptor
/// naming any other kind is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModuleKind {
    Authority,
    Sanitizer,
}

/// The answer entry point copied out of a loaded library. The copy is
/// sound only while the owning [`libloading::Library`] stays loaded —
/// [`LoadedModule`] holds both, and the registry never drops one.
pub(crate) type AnswerFn = unsafe extern "C" fn(*const u8, usize, *mut u8, usize, *mut usize) -> u32;

pub(crate) struct LoadedModule {
    _library: libloading::Library,
    pub(crate) answer: AnswerFn,
    /// Serializes calls into one module: an ABI-v1 implementation need
    /// not be re-entrant. Held only inside the blocking call, never
    /// across an await.
    pub(crate) gate: Mutex<()>,
}

/// Every module the process loaded, by kind and implementation name.
/// Immutable after open.
pub(crate) struct ModuleRegistry {
    authorities: BTreeMap<String, Arc<LoadedModule>>,
    sanitizers: BTreeMap<String, Arc<LoadedModule>>,
}

impl ModuleRegistry {
    pub(crate) fn empty() -> ModuleRegistry {
        ModuleRegistry {
            authorities: BTreeMap::new(),
            sanitizers: BTreeMap::new(),
        }
    }

    pub(crate) fn authority(&self, name: &str) -> Option<&Arc<LoadedModule>> {
        self.authorities.get(name)
    }

    pub(crate) fn sanitizer(&self, name: &str) -> Option<&Arc<LoadedModule>> {
        self.sanitizers.get(name)
    }
}

/// Why the modules directory refused to load. Every variant names the
/// offending file: a deployment error is loud, never a silently
/// unregistered implementation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ModulesError {
    #[error("the modules directory {path} is unreadable: {detail}")]
    DirectoryUnreadable { path: String, detail: String },
    #[error("the modules directory entry {path} is not a regular {extension} library file")]
    NotALibrary { path: String, extension: &'static str },
    #[error("the module {path} failed to load: {detail}")]
    Unloadable { path: String, detail: String },
    #[error("the module {path} does not export {symbol}: {detail}")]
    MissingSymbol {
        path: String,
        symbol: &'static str,
        detail: String,
    },
    #[error("the module {path} speaks ABI version {found}, not {expected}")]
    AbiMismatch { path: String, found: u32, expected: u32 },
    #[error("the module {path} declares an invalid descriptor: {detail}")]
    InvalidDescriptor { path: String, detail: String },
    #[error("the module {path} claims the name {name:?}, which is already provided")]
    NameTaken { path: String, name: String },
    #[error(
        "the {section} entry {name:?} names the builtin {builtin:?}, which is neither compiled in nor loaded for that kind"
    )]
    UnknownBuiltin {
        section: &'static str,
        name: String,
        builtin: String,
    },
    #[error("{0}")]
    LlmClient(String),
}

/// Loads every module in the directory. `None` — no directory
/// configured — is the stock-only deployment and loads nothing. Any
/// invalid entry refuses the whole load: a partially loaded deployment
/// never opens.
pub(crate) fn load(dir: Option<&Path>) -> Result<ModuleRegistry, ModulesError> {
    let Some(dir) = dir else {
        return Ok(ModuleRegistry::empty());
    };
    let display = dir.display().to_string();
    let entries = std::fs::read_dir(dir).map_err(|error| ModulesError::DirectoryUnreadable {
        path: display.clone(),
        detail: error.to_string(),
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| ModulesError::DirectoryUnreadable {
            path: display.clone(),
            detail: error.to_string(),
        })?;
        paths.push(entry.path());
    }
    paths.sort();

    let mut registry = ModuleRegistry::empty();
    for path in paths {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let (name, kind, module) = load_one(&path)?;
        // Implementation names are one namespace across both kinds: a
        // sanitizer reusing an authority module's name would make
        // "which implementation answers" depend on the section, and
        // the loader promised duplicate names refuse.
        if registry.authorities.contains_key(&name) || registry.sanitizers.contains_key(&name) {
            return Err(ModulesError::NameTaken {
                path: path.display().to_string(),
                name,
            });
        }
        let table = match kind {
            ModuleKind::Authority => &mut registry.authorities,
            ModuleKind::Sanitizer => &mut registry.sanitizers,
        };
        table.insert(name, Arc::new(module));
        tracing::debug!(path = %path.display(), "builtin module loaded");
    }
    Ok(registry)
}

fn load_one(path: &Path) -> Result<(String, ModuleKind, LoadedModule), ModulesError> {
    let display = path.display().to_string();
    let extension = std::env::consts::DLL_EXTENSION;
    let is_regular = std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);
    if !is_regular || path.extension().and_then(|ext| ext.to_str()) != Some(extension) {
        return Err(ModulesError::NotALibrary {
            path: display,
            extension,
        });
    }

    let library = open_library(path).map_err(|error| ModulesError::Unloadable {
        path: display.clone(),
        detail: error.to_string(),
    })?;

    let version: unsafe extern "C" fn() -> u32 = *symbol(
        &library,
        &display,
        "appa_builtin_abi_version",
        b"appa_builtin_abi_version\0",
    )?;
    let found = unsafe { version() };
    if found != ABI_VERSION {
        return Err(ModulesError::AbiMismatch {
            path: display,
            found,
            expected: ABI_VERSION,
        });
    }

    let descriptor_fn: unsafe extern "C" fn() -> DescriptorV1 = *symbol(
        &library,
        &display,
        "appa_builtin_descriptor_v1",
        b"appa_builtin_descriptor_v1\0",
    )?;
    let descriptor = unsafe { descriptor_fn() };
    let (name, kind) = validate_descriptor(&descriptor).map_err(|detail| ModulesError::InvalidDescriptor {
        path: display.clone(),
        detail,
    })?;
    if REFUSED_MODULE_NAMES.contains(&name.as_str()) {
        return Err(ModulesError::NameTaken { path: display, name });
    }

    let answer: AnswerFn = *symbol(
        &library,
        &display,
        "appa_builtin_answer_v1",
        b"appa_builtin_answer_v1\0",
    )?;

    Ok((
        name,
        kind,
        LoadedModule {
            _library: library,
            answer,
            gate: Mutex::new(()),
        },
    ))
}

fn symbol<'lib, T>(
    library: &'lib libloading::Library,
    path: &str,
    name: &'static str,
    bytes: &[u8],
) -> Result<libloading::Symbol<'lib, T>, ModulesError> {
    unsafe { library.get::<T>(bytes) }.map_err(|error| ModulesError::MissingSymbol {
        path: path.to_string(),
        symbol: name,
        detail: error.to_string(),
    })
}

fn validate_descriptor(descriptor: &DescriptorV1) -> Result<(String, ModuleKind), String> {
    let kind = match descriptor.kind {
        KIND_AUTHORITY => ModuleKind::Authority,
        KIND_SANITIZER => ModuleKind::Sanitizer,
        other => return Err(format!("kind {other} is not authority (1) or sanitizer (2)")),
    };
    if descriptor.name_ptr.is_null() {
        return Err("the name pointer is null".to_string());
    }
    if descriptor.name_len == 0 || descriptor.name_len > 64 {
        return Err(format!("the name length {} is outside 1..=64", descriptor.name_len));
    }
    let bytes = unsafe { std::slice::from_raw_parts(descriptor.name_ptr, descriptor.name_len) };
    let name = std::str::from_utf8(bytes).map_err(|_| "the name is not UTF-8".to_string())?;
    if !valid_implementation_name(name) {
        return Err(format!("the name {name:?} is not lowercase kebab within 64 bytes"));
    }
    Ok((name.to_string(), kind))
}

#[cfg(unix)]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
    unsafe { Library::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map(libloading::Library::from)
}

#[cfg(not(unix))]
fn open_library(path: &Path) -> Result<libloading::Library, libloading::Error> {
    unsafe { libloading::Library::new(path) }
}

#[derive(Debug, serde::Deserialize)]
struct ClaudeResultEnvelope {
    structured_output: Option<serde_json::Value>,
}

/// The stock `claude-code` model transport: one isolated, tool-less `claude` process per
/// consult, answering under the consult's own output schema. The deployment may override
/// the executable (a service environment often has no usable `PATH`), the model, and the
/// consult's time budget.
#[derive(Debug, Clone)]
pub(crate) struct ClaudeCodeBackend {
    pub(crate) command: std::path::PathBuf,
    pub(crate) model: String,
    pub(crate) timeout: std::time::Duration,
    pub(crate) max_body_bytes: usize,
}

impl ClaudeCodeBackend {
    pub(crate) async fn consult(
        &self,
        prompt: &ModelPrompt,
        deadline: tokio::time::Instant,
    ) -> Result<serde_json::Value, NoAnswerReason> {
        run_claude_code(self, prompt, deadline).await
    }
}

pub(crate) async fn run_claude_code(
    backend: &ClaudeCodeBackend,
    prompt: &ModelPrompt,
    deadline: tokio::time::Instant,
) -> Result<serde_json::Value, NoAnswerReason> {
    use std::process::Stdio;

    let schema = serde_json::to_string(&prompt.schema).map_err(|_| NoAnswerReason::Malformed)?;
    let work = tempfile::tempdir().map_err(|_| NoAnswerReason::Transport)?;
    let mut command = tokio::process::Command::new(&backend.command);
    command
        .arg("-p")
        .arg("--model")
        .arg(&backend.model)
        .arg("--safe-mode")
        .arg("--setting-sources")
        .arg("")
        .arg("--disable-slash-commands")
        .arg("--tools")
        .arg("")
        .arg("--permission-mode")
        .arg("dontAsk")
        .arg("--no-session-persistence")
        .arg("--output-format")
        .arg("json")
        .arg("--json-schema")
        .arg(schema)
        .arg("--system-prompt")
        .arg(&prompt.system)
        .current_dir(work.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    start_own_process_group(&mut command);
    // No APPA secret or wiring variable reaches the model: the child needs its own
    // credentials and HOME, never this runtime's bearer tokens.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("APPA_") {
            command.env_remove(key);
        }
    }
    tracing::debug!("claude consult starts");
    let mut child = command.spawn().map_err(|_| {
        tracing::warn!(command = %backend.command.display(), "the claude executable did not start");
        NoAnswerReason::Unreachable
    })?;
    let process_group = child.id().and_then(|pid| i32::try_from(pid).ok());
    let exchanged = tokio::time::timeout_at(deadline, exchange(&mut child, prompt, backend.max_body_bytes)).await;
    // Every outcome ends the consult's whole process group: no helper the CLI spawned
    // outlives the answer. On success the child is a zombie by now, so the group id is
    // still its own.
    terminate_process_group(process_group);
    let output = match exchanged {
        Ok(Ok(output)) => output,
        Ok(Err(reason)) => {
            let _ = child.kill().await;
            return Err(reason);
        }
        Err(_) => {
            let _ = child.kill().await;
            tracing::warn!("claude consult timed out and was terminated");
            return Err(NoAnswerReason::Timeout);
        }
    };
    let status = child.wait().await.map_err(|_| NoAnswerReason::Transport)?;
    if !status.success() {
        tracing::debug!(code = ?status.code(), "claude exited without an answer");
        return Err(NoAnswerReason::Transport);
    }
    let envelope: ClaudeResultEnvelope = serde_json::from_slice(&output).map_err(|_| NoAnswerReason::Malformed)?;
    envelope.structured_output.ok_or(NoAnswerReason::Malformed)
}

/// Feed the artifact, read the capped answer, and see the child out — without reaping
/// it, so the caller's group cleanup still addresses the right group.
async fn exchange(
    child: &mut tokio::process::Child,
    prompt: &ModelPrompt,
    max_body_bytes: usize,
) -> Result<Vec<u8>, NoAnswerReason> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stdin = child.stdin.take().ok_or(NoAnswerReason::Transport)?;
    let mut stdout = child.stdout.take().ok_or(NoAnswerReason::Transport)?;
    stdin
        .write_all(prompt.input.as_bytes())
        .await
        .map_err(|_| NoAnswerReason::Transport)?;
    stdin.shutdown().await.map_err(|_| NoAnswerReason::Transport)?;
    drop(stdin);
    // Read the capped output before waiting: a child writing past the cap is killed
    // right away, so a full pipe can never wedge the wait into the timeout.
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = stdout.read(&mut chunk).await.map_err(|_| NoAnswerReason::Transport)?;
        if read == 0 {
            break;
        }
        if output.len().saturating_add(read) > max_body_bytes {
            return Err(NoAnswerReason::Oversized);
        }
        output.extend_from_slice(&chunk[..read]);
    }
    observe_exit(child).await?;
    Ok(output)
}

#[cfg(unix)]
fn start_own_process_group(command: &mut tokio::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn start_own_process_group(_: &mut tokio::process::Command) {}

/// The consult started a fresh process group whose id is the child's pid; a negative pid
/// addresses the whole group. SIGKILL is deliberate: this runs after every outcome.
#[cfg(unix)]
fn terminate_process_group(process_group: Option<i32>) {
    if let Some(process_group) = process_group {
        unsafe {
            libc::kill(-process_group, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_process_group(_: Option<i32>) {}

#[cfg(unix)]
async fn observe_exit(child: &tokio::process::Child) -> Result<(), NoAnswerReason> {
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or(NoAnswerReason::Transport)?;
    crate::external::wait_for_child_exit(pid).await
}

#[cfg(not(unix))]
async fn observe_exit(_: &tokio::process::Child) -> Result<(), NoAnswerReason> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper the CLI leaves running — here a backgrounded `sleep` whose pid the fake
    /// records — must not survive the consult that started it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_claude_consult_takes_its_helpers_down_with_it() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("a temp dir");
        let pid_file = dir.path().join("helper.pid");
        let fake = dir.path().join("fake-claude");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\ncat > /dev/null\nsleep 30 > /dev/null 2>&1 &\necho $! > {}\nprintf '%s' '{{\"structured_output\":{{\"ruling\":\"approve\",\"reason\":\"ok\"}}}}'\n",
                pid_file.display()
            ),
        )
        .expect("the fake claude writes");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("the fake is executable");
        let backend = ClaudeCodeBackend {
            command: fake,
            model: "m".to_string(),
            timeout: std::time::Duration::from_secs(5),
            max_body_bytes: 65_536,
        };
        let prompt = ModelPrompt {
            system: "rule".to_string(),
            input: "{}".to_string(),
            schema: serde_json::json!({"type": "object"}),
        };

        let answer = backend
            .consult(&prompt, tokio::time::Instant::now() + std::time::Duration::from_secs(5))
            .await;
        assert_eq!(answer, Ok(serde_json::json!({"ruling": "approve", "reason": "ok"})));

        let helper: i32 = std::fs::read_to_string(&pid_file)
            .expect("the fake recorded its helper")
            .trim()
            .parse()
            .expect("a pid");
        let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let alive = unsafe { libc::kill(helper, 0) } == 0;
            if !alive {
                break;
            }
            assert!(std::time::Instant::now() < gone_by, "the helper outlived the consult");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn the_name_grammar_is_lowercase_kebab_within_64_bytes() {
        for name in ["approve", "redact-email", "a", "pii-2", "0day"] {
            assert!(valid_implementation_name(name), "{name:?} is valid");
        }
        let long = "a".repeat(65);
        for name in ["", "-lead", "Upper", "under_score", "dot.name", "spa ce", long.as_str()] {
            assert!(!valid_implementation_name(name), "{name:?} is invalid");
        }
        assert!(valid_implementation_name(&"a".repeat(64)));
    }

    #[test]
    fn the_stock_names_belong_to_one_section_each() {
        assert_eq!(
            Stock::for_section(Section::Authorities, "approve"),
            Some(Stock::Approve)
        );
        assert_eq!(
            Stock::for_section(Section::Sanitizers, "redact-email"),
            Some(Stock::RedactEmail)
        );
        for (section, name) in [
            (Section::Authorities, "hitl"),
            (Section::Authorities, "redact-email"),
            (Section::Sanitizers, "approve"),
            (Section::Casts, "approve"),
            (Section::Dynamic, "redact-email"),
            (Section::Membership, "approve"),
        ] {
            assert_eq!(Stock::for_section(section, name), None, "{section:?}/{name}");
        }
    }

    #[test]
    fn redact_email_masks_email_like_tokens_only() {
        let cases = [
            ("write to alice@corp.example today", "write to [redacted-email] today"),
            ("no addresses here", "no addresses here"),
            ("a@b.c and d@e.f", "[redacted-email] and [redacted-email]"),
            ("not@here (no dot)", "not@here (no dot)"),
            ("@leading.dot stays", "@leading.dot stays"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_email(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn a_missing_directory_refuses_and_no_directory_loads_nothing() {
        let registry = load(None).expect("no directory is the stock-only deployment");
        assert!(registry.authority("anything").is_none());

        let missing = std::path::Path::new("/nonexistent/appa-modules-test");
        match load(Some(missing)) {
            Err(ModulesError::DirectoryUnreadable { .. }) => {}
            Err(other) => panic!("a missing directory must refuse as unreadable, got {other}"),
            Ok(_) => panic!("a missing directory must refuse"),
        }
    }

    #[test]
    fn a_non_library_file_in_the_directory_refuses_the_load() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        std::fs::write(dir.path().join("README.md"), "not a module").expect("the file writes");
        match load(Some(dir.path())) {
            Err(ModulesError::NotALibrary { path, .. }) => assert!(path.ends_with("README.md")),
            Err(other) => panic!("a stray file must refuse as not-a-library, got {other}"),
            Ok(_) => panic!("a stray file must refuse"),
        }
    }

    #[test]
    fn a_file_with_the_library_suffix_but_no_module_inside_refuses() {
        let dir = tempfile::tempdir().expect("a temp dir is creatable");
        let name = format!("libfake.{}", std::env::consts::DLL_EXTENSION);
        std::fs::write(dir.path().join(&name), b"garbage bytes").expect("the file writes");
        match load(Some(dir.path())) {
            Err(ModulesError::Unloadable { .. }) => {}
            Err(other) => panic!("garbage must refuse as unloadable, got {other}"),
            Ok(_) => panic!("garbage must refuse"),
        }
    }

    #[test]
    fn descriptor_validation_names_each_refusal() {
        let name = b"ok-name";
        let good = DescriptorV1 {
            name_ptr: name.as_ptr(),
            name_len: name.len(),
            kind: KIND_SANITIZER,
        };
        assert_eq!(
            validate_descriptor(&good).expect("the descriptor is valid"),
            ("ok-name".to_string(), ModuleKind::Sanitizer),
        );

        let bad_kind = DescriptorV1 { kind: 9, ..good };
        assert!(validate_descriptor(&bad_kind).is_err());

        let null_name = DescriptorV1 {
            name_ptr: std::ptr::null(),
            ..good
        };
        assert!(validate_descriptor(&null_name).is_err());

        let oversized = DescriptorV1 { name_len: 65, ..good };
        assert!(validate_descriptor(&oversized).is_err());

        let bad_grammar = b"Not-Kebab";
        let ungrammatical = DescriptorV1 {
            name_ptr: bad_grammar.as_ptr(),
            name_len: bad_grammar.len(),
            kind: KIND_AUTHORITY,
        };
        assert!(validate_descriptor(&ungrammatical).is_err());
    }
}
