//! Builtin implementations: the stock answers and the module loader.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use appa_builtin::{ABI_VERSION, DescriptorV1, KIND_AUTHORITY, KIND_SANITIZER};
use regex::Regex;

use crate::config::{CLAUDE_CODE_BUILTIN, LLM_BUILTIN, Section};
use crate::consult::{Consult, ConsultBody};

/// The output-buffer bound for one module answer: the configured
/// `max_body_bytes`, but never more than this. Applies to module calls
/// only — the HTTP path's cap behavior is untouched.
pub(crate) const MODULE_OUTPUT_CEILING: usize = 16 * 1024 * 1024;

const REFUSED_MODULE_NAMES: [&str; 7] = [
    "hitl",
    "attest-schema",
    "approve",
    "redact-email",
    "redact-secrets",
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
    /// `redact-secrets` — a sanitizer replacing credentials in the body with a fixed
    /// placeholder: private-key blocks, tokens of well-known shapes, the AWS secret
    /// access key, the value of an assignment whose key names a secret, and any long
    /// high-entropy run. Same trust decision as `redact-email`: a detector, not a proof
    /// of absence.
    RedactSecrets,
}

impl Stock {
    pub(crate) fn for_section(section: Section, name: &str) -> Option<Stock> {
        match (section, name) {
            (Section::Authorities, "approve") => Some(Stock::Approve),
            (Section::Sanitizers, "redact-email") => Some(Stock::RedactEmail),
            (Section::Sanitizers, "redact-secrets") => Some(Stock::RedactSecrets),
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
            (Stock::RedactSecrets, ConsultBody::Sanitizer { artifact, .. }) => {
                Some(serde_json::json!({ "body": redact_secrets(&artifact.body) }))
            }
            _ => None,
        }
    }
}

const SECRET_PLACEHOLDER: &str = "[redacted-secret]";

/// A PEM-armored private key, the whole block.
static PRIVATE_KEY_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----").expect("a fixed pattern")
});

/// Tokens whose issuer fixes their shape: AWS access keys, GitHub, Anthropic, OpenAI,
/// Slack, Google, GitLab and npm tokens, and JWTs.
static KNOWN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
          AKIA[0-9A-Z]{16}
        | gh[pousr]_[A-Za-z0-9]{36,}
        | github_pat_[A-Za-z0-9_]{22,}
        | sk-ant-[A-Za-z0-9_-]{20,}
        | sk-[A-Za-z0-9_-]{20,}
        | xox[abprs]-[A-Za-z0-9-]{10,}
        | AIza[0-9A-Za-z_-]{35}
        | glpat-[A-Za-z0-9_-]{20,}
        | npm_[A-Za-z0-9]{36}
        | eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+
        ",
    )
    .expect("a fixed pattern")
});

/// The AWS secret access key: the one well-known credential with no prefix to recognize
/// it by, and 40 characters of base64 instead. `/` belongs to that alphabet, so the
/// candidate run below cannot see such a key whole — the run breaks at every slash and
/// each piece falls under the length floor. The pattern takes a whole run of the alphabet
/// and the caller keeps the length exact: a longer run, which is what a path or a URL is,
/// is no 40-character match. Every character outside the alphabet ends a run, `=`, `-`,
/// `_` and `.` included, and the match consumes none of them, so a key is found whatever
/// stands next to it.
static AWS_SECRET_KEY_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/]+").expect("a fixed pattern"));

const AWS_SECRET_KEY_LENGTH: usize = 40;

/// The password of a URL with credentials in its authority: `scheme://user:password@host`.
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(://[^/?#@:\s"']*:)([^/?#@\s"']+)@"#).expect("a fixed pattern"));

/// An assignment whose key names a secret: `KEY=value`, `key: value`, or JSON's
/// `"key": "value"`. A key names a secret when it contains one of the longer words, or
/// when `key`, `pass`, `pwd` or `auth` is one of its segments, separated or camel-cased.
/// The value is masked whatever its shape: a quoted value to its closing quote, an HTTP
/// credential with its scheme word, a bare one to the next space or delimiter; its
/// quotes stay.
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        ( ["']?
          (?: [A-Za-z0-9_.-]* (?: password | passwd | passphrase | secret | token | api_?key | private_?key | access_?key | credential | authorization ) [A-Za-z0-9_.-]*
            | \b (?: [A-Za-z0-9]+ [_.-] )* (?: key | pass | pwd | auth ) (?: [_.-] [A-Za-z0-9]+ )*
            | \b (?-i: [A-Za-z0-9]+ (?: Key | Pass | Pwd | Auth ) ) [A-Za-z0-9]* )
          ["']? \s*[=:]\s* )
        ( "[^"]*" | '[^']*' | (?: bearer | basic ) \s+ [^\s"'`,;&)\]}]+ | [^\s"'`,;&)\]}]+ )
        "#,
    )
    .expect("a fixed pattern")
});

/// A netrc `password` entry, on its own line or after the machine and login it belongs to.
static NETRC_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(\s*(?:machine\s+\S+\s+(?:login\s+\S+\s+)?)?password\s+)(\S+)").expect("a fixed pattern")
});

/// A run long enough to be a credential and shaped like one; the entropy test decides. `=`
/// is a separator here, not base64 padding: the run before it is what carries the entropy.
static CANDIDATE_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+_-]{20,}").expect("a fixed pattern"));

fn redact_secrets(input: &str) -> String {
    // Keyed passes run before the shape passes: a value keyed by a secret name is masked
    // whole, and a placeholder never becomes a later pass's value.
    let masked = PRIVATE_KEY_BLOCK.replace_all(input, SECRET_PLACEHOLDER);
    let masked = URL_PASSWORD.replace_all(&masked, |found: &regex::Captures<'_>| {
        format!("{}{SECRET_PLACEHOLDER}@", &found[1])
    });
    let masked = SECRET_ASSIGNMENT.replace_all(&masked, |found: &regex::Captures<'_>| {
        let quote = match found[2].as_bytes().first() {
            Some(b'"') => "\"",
            Some(b'\'') => "'",
            _ => "",
        };
        format!("{}{quote}{SECRET_PLACEHOLDER}{quote}", &found[1])
    });
    let masked = NETRC_PASSWORD.replace_all(&masked, |found: &regex::Captures<'_>| {
        format!("{}{SECRET_PLACEHOLDER}", &found[1])
    });
    let masked = KNOWN_TOKEN.replace_all(&masked, SECRET_PLACEHOLDER);
    let masked = AWS_SECRET_KEY_RUN.replace_all(&masked, |found: &regex::Captures<'_>| {
        let run = &found[0];
        if run.len() == AWS_SECRET_KEY_LENGTH && is_base64_key(run) {
            SECRET_PLACEHOLDER.to_string()
        } else {
            run.to_string()
        }
    });
    CANDIDATE_RUN
        .replace_all(&masked, |found: &regex::Captures<'_>| {
            let run = &found[0];
            if looks_random(run) {
                SECRET_PLACEHOLDER.to_string()
            } else {
                run.to_string()
            }
        })
        .into_owned()
}

/// Whether a run the length of an AWS secret access key is one, rather than a path of the
/// same length: base64 over 40 characters draws on all three of the alphabet's classes,
/// and clears the entropy floor. A path long enough to reach 40 characters between two
/// delimiters rarely carries both an upper-case letter and a digit.
fn is_base64_key(run: &str) -> bool {
    run.bytes().any(|byte| byte.is_ascii_digit())
        && run.bytes().any(|byte| byte.is_ascii_uppercase())
        && run.bytes().any(|byte| byte.is_ascii_lowercase())
        && looks_random(run)
}

/// Shannon entropy over the run's bytes, against a floor that depends on its alphabet: a
/// hex string cannot exceed 4 bits per byte, so it clears a lower bar over a longer run.
fn looks_random(run: &str) -> bool {
    let hex = run.bytes().all(|byte| byte.is_ascii_hexdigit());
    let (floor, minimum_length) = if hex { (3.0, 32) } else { (4.0, 20) };
    run.len() >= minimum_length && shannon_entropy(run.as_bytes()) >= floor
}

fn shannon_entropy(bytes: &[u8]) -> f64 {
    let mut counts = [0usize; 256];
    for byte in bytes {
        counts[usize::from(*byte)] += 1;
    }
    let total = bytes.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / total;
            -p * p.log2()
        })
        .sum()
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

/// Which of the two module-capable kinds a module implements. Annotators and
/// audience sources take no module — a descriptor naming any other kind is
/// refused.
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            Stock::for_section(Section::Sanitizers, "redact-secrets"),
            Some(Stock::RedactSecrets)
        );
        for (section, name) in [
            (Section::Authorities, "redact-secrets"),
            (Section::Authorities, "hitl"),
            (Section::Authorities, "redact-email"),
            (Section::Sanitizers, "approve"),
            (Section::Annotators, "redact-email"),
            (Section::Audience, "approve"),
        ] {
            assert_eq!(Stock::for_section(section, name), None, "{section:?}/{name}");
        }
    }

    #[test]
    fn redact_secrets_masks_keys_tokens_secret_assignments_and_random_runs() {
        let masked = "[redacted-secret]";
        let cases = [
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXkt\ndjEAAAAABG5vbmU=\n-----END OPENSSH PRIVATE KEY-----\n",
                format!("{masked}\n"),
            ),
            (
                "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
                format!("aws_access_key_id = {masked}"),
            ),
            (
                "export GITHUB_TOKEN=ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789",
                format!("export GITHUB_TOKEN={masked}"),
            ),
            (
                "ANTHROPIC_API_KEY=\"sk-ant-api03-abcdefghijklmnopqrstuvwxyz\"",
                format!("ANTHROPIC_API_KEY=\"{masked}\""),
            ),
            (
                "SLACK_BOT_TOKEN: xoxb-1234567890-abcdefghij",
                format!("SLACK_BOT_TOKEN: {masked}"),
            ),
            (
                "jwt=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                format!("jwt={masked}"),
            ),
            (
                "machine api.example.com login me password hunter2",
                format!("machine api.example.com login me password {masked}"),
            ),
            ("DB_PASSWORD='p@ss'", format!("DB_PASSWORD='{masked}'")),
            (
                "{\"password\": \"hunter2\", \"user\": \"me\"}",
                format!("{{\"password\": \"{masked}\", \"user\": \"me\"}}"),
            ),
            (
                "{\"auths\": {\"ghcr.io\": {\"auth\": \"dXNlcjpwYXNzd29yZA==\"}}}",
                format!("{{\"auths\": {{\"ghcr.io\": {{\"auth\": \"{masked}\"}}}}}}"),
            ),
            ("GITHUB_KEY=abc123", format!("GITHUB_KEY={masked}")),
            ("db.pass: hunter2", format!("db.pass: {masked}")),
            (
                "DB_PASSWORD=\"correct horse battery staple\"\nNEXT=1",
                format!("DB_PASSWORD=\"{masked}\"\nNEXT=1"),
            ),
            (
                "{\"privateKey\": \"hunter2\", \"accessKey\": \"hunter2\", \"sshKey\": \"hunter2\"}",
                format!("{{\"privateKey\": \"{masked}\", \"accessKey\": \"{masked}\", \"sshKey\": \"{masked}\"}}"),
            ),
            ("PASSPHRASE=hunter2", format!("PASSPHRASE={masked}")),
            ("SshKey=hunter2", format!("SshKey={masked}")),
            (
                "Authorization: Bearer hunter2\nAUTHORIZATION=hunter2\n",
                format!("Authorization: {masked}\nAUTHORIZATION={masked}\n"),
            ),
            (
                "https://example.com/?token=abc&foo=bar",
                format!("https://example.com/?token={masked}&foo=bar"),
            ),
            ("{\"token\": 12345}", format!("{{\"token\": {masked}}}")),
            (
                "https://example.com:8080?q=a@b",
                "https://example.com:8080?q=a@b".to_string(),
            ),
            (
                "machine api.example.com\n  login me\n  password hunter2\n",
                format!("machine api.example.com\n  login me\n  password {masked}\n"),
            ),
            (
                "the password is hunter2 and the secret sauce is good",
                "the password is hunter2 and the secret sauce is good".to_string(),
            ),
            (
                "DATABASE_URL=postgres://app:Sup3r-Secret-123@db.internal:5432/app",
                format!("DATABASE_URL=postgres://app:{masked}@db.internal:5432/app"),
            ),
            (
                "redis://:hunter2@cache.internal:6379/0",
                format!("redis://:{masked}@cache.internal:6379/0"),
            ),
            (
                "monkey=banana\nkeyboard=us\nAUTHOR=me\nPASSPORT_OFFICE=closed\nhttps://host:8080/path\n",
                "monkey=banana\nkeyboard=us\nAUTHOR=me\nPASSPORT_OFFICE=closed\nhttps://host:8080/path\n".to_string(),
            ),
            (
                "SESSION=c3VwZXJzZWNyZXQtcmFuZG9tLXZhbHVl",
                format!("SESSION={masked}"),
            ),
            (
                "COMMIT=3f7a9c2e1b8d4f6a0c5e7b9d1f3a5c7e9b2d4f68",
                format!("COMMIT={masked}"),
            ),
            (
                "DATABASE_HOST=db.internal.example.com\nLOG_LEVEL=debug\nFEATURE_FLAGS=configuration_management_enabled\n",
                "DATABASE_HOST=db.internal.example.com\nLOG_LEVEL=debug\nFEATURE_FLAGS=configuration_management_enabled\n"
                    .to_string(),
            ),
            (
                "see https://docs.example.com/reference/authentication for the flow",
                "see https://docs.example.com/reference/authentication for the flow".to_string(),
            ),
            // The AWS secret access key names no issuer and carries slashes, so only its
            // own shape finds it. Bare is the case the candidate run cannot reach.
            (
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                masked.to_string(),
            ),
            (
                "error: signature mismatch for wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY here",
                format!("error: signature mismatch for {masked} here"),
            ),
            (
                "{\"SecretAccessKey\": \"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\"}",
                format!("{{\"SecretAccessKey\": \"{masked}\"}}"),
            ),
            // `=` ends a run like any other delimiter, under a name that names no secret.
            (
                "Sig=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                format!("Sig={masked}"),
            ),
            // One delimiter between two runs serves both: the run before a key may be
            // another key or a commit id.
            (
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY Zq3/tVb8LmXw2Rk/9pHsYcD4nGfJ7uEoA1iTz/Bx",
                format!("{masked} {masked}"),
            ),
            (
                "3f7a9c2e1b8d4f6a0c5e7b9d1f3a5c7e9b2d4f68,Zq3/tVb8LmXw2Rk/9pHsYcD4nGfJ7uEoA1iTz/Bx",
                format!("{masked},{masked}"),
            ),
            // A path and a URL are runs of the same alphabet; neither holds 40 characters
            // of it between two delimiters.
            (
                "/Users/person/dev/OpenAPPA-public/appa-runtime/src/builtins.rs",
                "/Users/person/dev/OpenAPPA-public/appa-runtime/src/builtins.rs".to_string(),
            ),
            (
                "https://github.com/anthropics/claude-code/blob/main/README.md",
                "https://github.com/anthropics/claude-code/blob/main/README.md".to_string(),
            ),
            // Exactly 40 characters of the alphabet, and still a path: no digit.
            (
                "Sources/Adapters/ClaudeCode/Renders/Body",
                "Sources/Adapters/ClaudeCode/Renders/Body".to_string(),
            ),
            ("", String::new()),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_secrets(input), expected, "input {input:?}");
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
