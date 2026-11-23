//! Secret redaction: the stock `redact-secrets` sanitizer's text pass, and the same pass
//! over an annotation's call before it leaves for a model provider.
//!
//! Where a quoted value ends depends on what a backslash does inside the quotes, which
//! the text does not say. The keyed pass reads the text four ways, a backslash escaping
//! or literal in double and in single quotes, finds assignments and netrc entries under
//! each, and masks the union. The guarantee: an assignment's or a netrc entry's secret is
//! masked whenever the text is consistent with at least one of the four readings.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

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
/// at least a key long, and the caller keeps the length exact: a longer run, which is what
/// a path or a URL is, is no 40-character match. Every character outside the alphabet
/// ends a run, `=`, `-`, `_` and `.` included, and the match consumes none of them, so a
/// key is found whatever stands next to it.
static AWS_SECRET_KEY_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/]{40,}").expect("a fixed pattern"));

const AWS_SECRET_KEY_LENGTH: usize = 40;

/// The password of a URL with credentials in its authority: `scheme://user:password@host`.
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(://[^/?#@:\s"']*:)([^/?#@\s"']+)@"#).expect("a fixed pattern"));

/// Whether a backslash inside quotes of one kind escapes the next character, as in JSON
/// or a shell's double quotes, or stands for itself, as in a shell's single quotes.
#[derive(Debug, Clone, Copy)]
enum Backslash {
    Escapes,
    Literal,
}

/// One reading of quoted text: what a backslash does in double and in single quotes.
#[derive(Debug, Clone, Copy)]
struct Reading {
    double: Backslash,
    single: Backslash,
}

/// Every combination. Text written in any of them, JSON, a shell, a `.env` file with
/// Windows paths, reads the same as in one of these.
const READINGS: [Reading; 4] = [
    Reading {
        double: Backslash::Escapes,
        single: Backslash::Escapes,
    },
    Reading {
        double: Backslash::Escapes,
        single: Backslash::Literal,
    },
    Reading {
        double: Backslash::Literal,
        single: Backslash::Escapes,
    },
    Reading {
        double: Backslash::Literal,
        single: Backslash::Literal,
    },
];

impl Backslash {
    /// A value in `quote`s: to its closing quote, or to the end of the text.
    fn quoted(self, quote: char) -> String {
        match self {
            Backslash::Escapes => format!(r"{quote}(?: [^{quote}\\] | \\[\s\S] )*(?: {quote} | \\?\z )"),
            Backslash::Literal => format!(r"{quote}[^{quote}]*(?: {quote} | \z )"),
        }
    }

    fn closes(self, inside: &[u8]) -> bool {
        match self {
            Backslash::Escapes => inside.iter().rev().take_while(|byte| **byte == b'\\').count() % 2 == 0,
            Backslash::Literal => true,
        }
    }
}

/// An assignment whose key names a secret, under one reading: `KEY=value`, `key: value`,
/// or JSON's `"key": "value"`. A key names a secret when it contains one of the longer
/// words, or when `key`, `pass`, `pwd` or `auth` is one of its segments, separated or
/// camel-cased. The value is masked whatever its shape: a quoted value to its closing
/// quote, or to the end of the text; an HTTP credential with its scheme word; a bare one
/// to the next space or delimiter. Its quotes stay.
struct ReadingScan {
    reading: Reading,
    assignment: Regex,
}

static READING_SCANS: LazyLock<[ReadingScan; 4]> = LazyLock::new(|| {
    READINGS.map(|reading| ReadingScan {
        reading,
        assignment: Regex::new(&format!(
            r#"(?ix)
            ( ["']? {SECRET_NAME} ["']? \s*[=:]\s* )
            ( {} | {} | (?: bearer | basic ) \s+ [^\s"'`,;&)\]}}]+ | [^\s"'`,;&)\]}}]+ )
            "#,
            reading.double.quoted('"'),
            reading.single.quoted('\''),
        ))
        .expect("a fixed pattern"),
    })
});

const SECRET_NAME: &str = r"(?: [A-Za-z0-9_.-]* (?: password | passwd | passphrase | secret | token | api_?key | private_?key | access_?key | credential | authorization ) [A-Za-z0-9_.-]*
    | \b (?: [A-Za-z0-9]+ [_.-] )* (?: key | pass | pwd | auth ) (?: [_.-] [A-Za-z0-9]+ )*
    | \b (?-i: [A-Za-z0-9]+ (?: Key | Pass | Pwd | Auth ) ) [A-Za-z0-9]* )";

/// A field name the assignment pattern takes for a secret's: one that ends in a name it
/// matches.
static SECRET_NAME_ENDING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?ix) {SECRET_NAME} $")).expect("a fixed pattern"));

/// A netrc `password` entry, on its own line or after the machine and login it belongs to.
static NETRC_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(\s*(?:machine\s+\S+\s+(?:login\s+\S+\s+)?)?password\s+)(\S+)").expect("a fixed pattern")
});

/// Every secret assignment's value and every netrc password replaced, quotes kept, under
/// every reading at once.
fn mask_keyed_values(text: &str) -> Cow<'_, str> {
    mask_keyed_values_under(text, READING_SCANS.as_slice())
}

fn mask_keyed_values_under<'a>(text: &'a str, scans: &[ReadingScan]) -> Cow<'a, str> {
    let mut spans: Vec<Range<usize>> = NETRC_PASSWORD
        .captures_iter(text)
        .map(|found| found.get(2).expect("the pattern captures a password").range())
        .collect();
    for scan in scans {
        spans.extend(keyed_spans(text, scan));
    }
    mask_spans(text, &merged(spans))
}

/// One reading's spans: every assignment's value, and every netrc password found on the
/// text with those values masked, so a value with a space in it cannot hide the entry.
fn keyed_spans(text: &str, scan: &ReadingScan) -> Vec<Range<usize>> {
    let values: Vec<Range<usize>> = scan
        .assignment
        .captures_iter(text)
        .map(|found| value_span(found.get(2).expect("the pattern captures a value"), scan.reading))
        .collect();
    let masked = mask_spans(text, &values);
    let mut source = SourceOffsets::new(&values);
    let passwords: Vec<Range<usize>> = NETRC_PASSWORD
        .captures_iter(&masked)
        .map(|found| {
            let password = found.get(2).expect("the pattern captures a password");
            source.of(password.start())..source.of(password.end())
        })
        .collect();
    values.into_iter().chain(passwords).collect()
}

/// A value inside its quotes: up to a closing quote, or to the end of the text.
fn value_span(value: regex::Match<'_>, reading: Reading) -> Range<usize> {
    let backslash = match value.as_str().as_bytes().first() {
        Some(b'"') => reading.double,
        Some(b'\'') => reading.single,
        _ => return value.range(),
    };
    match value.as_str().as_bytes() {
        [open, inside @ .., close] if open == close && backslash.closes(inside) => value.start() + 1..value.end() - 1,
        _ => value.start() + 1..value.end(),
    }
}

/// The spans sorted, with every overlapping or adjacent pair joined.
fn merged(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by(|next, kept| match next.start <= kept.end {
        true => {
            kept.end = kept.end.max(next.end);
            true
        }
        false => false,
    });
    spans
}

/// The text with each span, sorted and disjoint, replaced by the placeholder.
fn mask_spans<'a>(text: &'a str, spans: &[Range<usize>]) -> Cow<'a, str> {
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut masked = String::with_capacity(text.len());
    let mut copied = 0;
    for span in spans {
        masked.push_str(&text[copied..span.start]);
        masked.push_str(SECRET_PLACEHOLDER);
        copied = span.end;
    }
    masked.push_str(&text[copied..]);
    Cow::Owned(masked)
}

/// Offsets in [`mask_spans`]'s output mapped back to its input, asked in nondecreasing
/// order. None falls inside a placeholder: a netrc password starts after whitespace and
/// ends before it or at the end of the text, and the placeholder holds no whitespace.
struct SourceOffsets<'a> {
    spans: std::slice::Iter<'a, Range<usize>>,
    next: Option<&'a Range<usize>>,
    source_passed: usize,
    masked_passed: usize,
}

impl<'a> SourceOffsets<'a> {
    fn new(spans: &'a [Range<usize>]) -> SourceOffsets<'a> {
        let mut spans = spans.iter();
        let next = spans.next();
        SourceOffsets {
            spans,
            next,
            source_passed: 0,
            masked_passed: 0,
        }
    }

    fn of(&mut self, offset: usize) -> usize {
        while let Some(span) = self
            .next
            .filter(|span| offset > span.start + self.masked_passed - self.source_passed)
        {
            self.source_passed += span.len();
            self.masked_passed += SECRET_PLACEHOLDER.len();
            self.next = self.spans.next();
        }
        offset + self.source_passed - self.masked_passed
    }
}

/// A run long enough to be a credential and shaped like one; the entropy test decides. `=`
/// is a separator here, not base64 padding: the run before it is what carries the entropy.
static CANDIDATE_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+_-]{20,}").expect("a fixed pattern"));

pub(crate) fn redact_secrets(input: &str) -> String {
    // Keyed passes run before the shape passes: a value keyed by a secret name is masked
    // whole, and a placeholder never becomes a later pass's value.
    let masked = PRIVATE_KEY_BLOCK.replace_all(input, SECRET_PLACEHOLDER);
    let masked = URL_PASSWORD.replace_all(&masked, |found: &regex::Captures<'_>| {
        format!("{}{SECRET_PLACEHOLDER}@", &found[1])
    });
    let masked = mask_keyed_values(&masked);
    let masked = KNOWN_TOKEN.replace_all(&masked, SECRET_PLACEHOLDER);
    let masked = mask_aws_secret_keys(&masked);
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

/// An object key through the passes that know a credential by its own shape. The keyed
/// and entropy passes stay out: a key is a name, and an ordinary name such as
/// `Customer-Billing-Zone_V2` clears the entropy floor.
fn redact_key(key: &str) -> String {
    let masked = PRIVATE_KEY_BLOCK.replace_all(key, SECRET_PLACEHOLDER);
    let masked = URL_PASSWORD.replace_all(&masked, |found: &regex::Captures<'_>| {
        format!("{}{SECRET_PLACEHOLDER}@", &found[1])
    });
    let masked = KNOWN_TOKEN.replace_all(&masked, SECRET_PLACEHOLDER);
    mask_aws_secret_keys(&masked).into_owned()
}

fn mask_aws_secret_keys(text: &str) -> Cow<'_, str> {
    AWS_SECRET_KEY_RUN.replace_all(text, |found: &regex::Captures<'_>| {
        let run = &found[0];
        if run.len() == AWS_SECRET_KEY_LENGTH && is_base64_key(run) {
            SECRET_PLACEHOLDER.to_string()
        } else {
            run.to_string()
        }
    })
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

/// A field whose name contains one of these words, or is `auth`, holds a secret, whatever
/// its value.
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)api[_-]?key|secret|token|password|passwd|passphrase|authorization|cookie|credential|private[_-]?key|access[_-]?key|^auth$",
    )
    .expect("a fixed pattern")
});

/// An annotation's declared inputs, or its context, as they leave for a model provider:
/// every string through [`redact_secrets`], every object key below the top level through
/// [`redact_key`], and the whole value of a field named for a secret replaced. Top-level
/// keys are the declaration's own names, which the provider reads in the declaration as
/// written, so they leave as written too.
pub(crate) fn redact_args(args: &serde_json::Value) -> serde_json::Value {
    match args {
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), redact_field(key, value)))
                .collect(),
        ),
        other => redact_value(other),
    }
}

/// The stock `redact-secrets` sanitizer's derivation. A body that is JSON is redacted as
/// its tree, so the derivation stays JSON of the same shape; any other body as text.
pub(crate) fn redact_body(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => redact_value(&value).to_string(),
        Err(_) => redact_secrets(body),
    }
}

/// An annotation's complete call as it leaves for a model provider: [`redact_args`], with
/// the tool `name` left as written.
pub(crate) fn redact_call(call: &serde_json::Value) -> serde_json::Value {
    let mut redacted = redact_args(call);
    if let (Some(name), serde_json::Value::Object(fields)) =
        (call.get("name").filter(|name| name.is_string()), &mut redacted)
    {
        fields.insert("name".to_string(), name.clone());
    }
    redacted
}

fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(redact_secrets(text)),
        serde_json::Value::Object(fields) => serde_json::Value::Object(redact_keys(fields)),
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

fn redact_field(key: &str, value: &serde_json::Value) -> serde_json::Value {
    match SECRET_FIELD.is_match(key) || SECRET_NAME_ENDING.is_match(key) {
        true => serde_json::Value::String(SECRET_PLACEHOLDER.to_string()),
        false => redact_value(value),
    }
}

/// Keys that [`redact_key`] leaves alone keep their names; a rewritten key that collides
/// with one already kept takes its base's next free `-2`, `-3`, … suffix, so every entry
/// survives, numbered in the map's key order.
fn redact_keys(fields: &serde_json::Map<String, serde_json::Value>) -> serde_json::Map<String, serde_json::Value> {
    let (unchanged, rewritten): (Vec<_>, Vec<_>) = fields
        .iter()
        .map(|(key, value)| (redact_key(key), key, value))
        .partition(|(redacted_key, key, _)| redacted_key == *key);
    let mut redacted: serde_json::Map<String, serde_json::Value> = unchanged
        .into_iter()
        .map(|(redacted_key, key, value)| (redacted_key, redact_field(key, value)))
        .collect();
    let mut next_suffix: HashMap<String, usize> = HashMap::new();
    for (base, key, value) in rewritten {
        let suffix = next_suffix.entry(base.clone()).or_insert(1);
        let free = loop {
            let candidate = match *suffix {
                1 => base.clone(),
                n => format!("{base}-{n}"),
            };
            *suffix += 1;
            if !redacted.contains_key(&candidate) {
                break candidate;
            }
        };
        redacted.insert(free, redact_field(key, value));
    }
    redacted
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
            ("DB_PASSWORD=\"\"", format!("DB_PASSWORD=\"{masked}\"")),
            ("token=\"x\npassword inner\"suffix\"", format!("token=\"{masked}")),
            (
                "machine h login token=\"\" password hunter2",
                format!("machine h login token=\"{masked}\" password {masked}"),
            ),
            (
                "machine h login me password api_key=abc\nnext",
                format!("machine h login me password {masked}\nnext"),
            ),
            ("password=token=\"public\"", format!("password={masked}\"public\"")),
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
            ("wJalrXUtnFEMI+K7MDENG/bPxRfiCYEXAMPLEKEY", masked.to_string()),
            // A run longer than a key is taken whole, and no 40-character piece of it is
            // masked.
            (
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEX/AMPLEKEY12",
                "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEX/AMPLEKEY12".to_string(),
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
    fn a_quoted_secret_is_masked_under_every_quote_reading() {
        let masked = "[redacted-secret]";
        let cases = [
            (r#"password="ab\"cdefgh" next"#, format!(r#"password="{masked}" next"#)),
            (r"password='ab\'cdefgh' next", format!("password='{masked}' next")),
            (r#"PASSWORD="a\"b\"c\"tail">out"#, format!(r#"PASSWORD="{masked}">out"#)),
            (r"PASSWORD='a\'b\'tail'>out", format!("PASSWORD='{masked}'>out")),
            (
                r#"export DB_PASSWORD="ab\"cdefgh">out"#,
                format!(r#"export DB_PASSWORD="{masked}">out"#),
            ),
            (
                r#"{"password": "a\\\"b\\c", "user": "me"}"#,
                format!(r#"{{"password": "{masked}", "user": "me"}}"#),
            ),
            (
                "{\"password\":\"ab\\\"cd\u{2028}efgh\"}",
                format!("{{\"password\":\"{masked}\"}}"),
            ),
            (r#"password="abc\\" next"#, format!(r#"password="{masked}" next"#)),
            (r#"password="a\\\"b" next"#, format!(r#"password="{masked}" next"#)),
            (r"password='abc\\' next", format!("password='{masked}' next")),
            (
                r#"{"password":"abc\\","user":"me"}"#,
                format!(r#"{{"password":"{masked}","user":"me"}}"#),
            ),
            ("PASSWORD=\"a\\\"b\nshort tail\"", format!("PASSWORD=\"{masked}\"")),
            (
                "DIR_PASSWORD=\"C:\\dir\\\"\nDB_PASSWORD=\"hunter2\"\nUSER=me",
                format!("DIR_PASSWORD=\"{masked}\"{masked}\"\nUSER=me"),
            ),
            (
                "PASSWORD=\"ab\\\"cd\"\r\nUSER=me",
                format!("PASSWORD=\"{masked}\"\r\nUSER=me"),
            ),
            (
                "token=\"ab\ncd\\\"ef\" tail\nNEXT=1",
                format!("token=\"{masked}\" tail\nNEXT=1"),
            ),
            (
                "token=\"ab\\\"cd\nmachine h login me password hunter2",
                format!("token=\"{masked}"),
            ),
            (
                r#"machine api.example.com login token="ab\" password "hunter2""#,
                format!(r#"machine api.example.com login token="{masked}"#),
            ),
            (
                "machine h login token=\"a\\\"b\" password\nhunter2",
                format!("machine h login token=\"{masked}\" password\n{masked}"),
            ),
            (
                "password=\"a\\\"b\" TOKEN=\"\"\r\npassword hunter2",
                format!("password=\"{masked}\" TOKEN=\"{masked}\"\r\npassword {masked}"),
            ),
            (
                "PASSWORD=\"x\\\"TOKEN='y\"\nDB_PASSWORD='hunter2'",
                format!("PASSWORD=\"{masked}'{masked}'"),
            ),
            (
                "PASSWORD=\"x\\\"TOKEN='foo\"\nPASSWORD=\"a\\\"b\" TOKEN=\n  'hunter2'",
                format!("PASSWORD=\"{masked}'{masked}'"),
            ),
            (
                "PASSWORD=\"x\\\"TOKEN='y\"\nmachine h login me' password hunter2",
                format!("PASSWORD=\"{masked}' password {masked}"),
            ),
            (
                "PASSWORD=\"x\\\"TOKEN='y\"\nmachine h login o'neil password\nhunter2",
                format!("PASSWORD=\"{masked}'neil password\n{masked}"),
            ),
            (
                "{\"password\":\"ab\\\"cd\",\"token\":\n  \"hunter2\"}",
                format!("{{\"password\":\"{masked}\",\"token\":\n  \"{masked}\"}}"),
            ),
            (
                "{\"password\":\"x\\\"TOKEN='y\",\n \"secret\":\"a\\\"b\",\"token\":\n \"'hunter2\"}",
                format!("{{\"password\":\"{masked}\"}}"),
            ),
            (
                "DIR_PASSWORD=\"C:\\dir\\\"\nDB_PASSWORD='x\"y\\'hunter2'",
                format!("DIR_PASSWORD=\"{masked}'"),
            ),
            (
                "DIR_PASSWORD=\"C:\\dir\\\"\nmachine h login token=\"a b\" password hunter2",
                format!("DIR_PASSWORD=\"{masked}\"{masked}\" password {masked}"),
            ),
            (
                "DIR_PASSWORD=\"C:\\dir\\\" DB_PASSWORD='x\n\" hunter2'",
                format!("DIR_PASSWORD=\"{masked}'"),
            ),
            (
                "machine TOKEN=\"a\\\"b\" login SECRET=\"foo bar\" password\nhunter2",
                format!("machine TOKEN=\"{masked}\" login SECRET=\"{masked}\" password\n{masked}"),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_secrets(input), expected, "input {input:?}");
        }
    }

    /// Each reading alone leaves a credential that another reading finds, in the order of
    /// [`READINGS`]: double then single quotes, a backslash escaping (E) or literal (L).
    #[test]
    fn every_quote_reading_alone_leaves_a_credential_the_union_masks() {
        let m = "[redacted-secret]";
        let cases = [
            (
                "DIR_PASSWORD=\"C:\\dir\\\"\nDB_PASSWORD=\"hunter2\"",
                format!("DIR_PASSWORD=\"{m}\"{m}\""),
                [
                    format!("DIR_PASSWORD=\"{m}\"hunter2\""),
                    format!("DIR_PASSWORD=\"{m}\"hunter2\""),
                    format!("DIR_PASSWORD=\"{m}\"\nDB_PASSWORD=\"{m}\""),
                    format!("DIR_PASSWORD=\"{m}\"\nDB_PASSWORD=\"{m}\""),
                ],
            ),
            (
                "{\"password\":\"ab\\\"cd\",\"token\":\n  \"hunter2\"}",
                format!("{{\"password\":\"{m}\",\"token\":\n  \"{m}\"}}"),
                [
                    format!("{{\"password\":\"{m}\",\"token\":\n  \"{m}\"}}"),
                    format!("{{\"password\":\"{m}\",\"token\":\n  \"{m}\"}}"),
                    format!("{{\"password\":\"{m}\"cd\",\"token\":\n  \"{m}\"}}"),
                    format!("{{\"password\":\"{m}\"cd\",\"token\":\n  \"{m}\"}}"),
                ],
            ),
            (
                "password='ab\\'cd' token='hunter2'",
                format!("password='{m}' token='{m}'"),
                [
                    format!("password='{m}' token='{m}'"),
                    format!("password='{m}'cd' token='{m}'"),
                    format!("password='{m}' token='{m}'"),
                    format!("password='{m}'cd' token='{m}'"),
                ],
            ),
            (
                "password='C:\\dir\\' token='hunter2'",
                format!("password='{m}'{m}'"),
                [
                    format!("password='{m}'hunter2'"),
                    format!("password='{m}' token='{m}'"),
                    format!("password='{m}'hunter2'"),
                    format!("password='{m}' token='{m}'"),
                ],
            ),
        ];
        for (input, union, alone) in cases {
            assert_eq!(mask_keyed_values(input), union, "input {input:?}");
            for (scan, expected) in READING_SCANS.iter().zip(alone) {
                assert_eq!(
                    mask_keyed_values_under(input, std::slice::from_ref(scan)),
                    expected,
                    "input {input:?} under {:?}",
                    scan.reading
                );
            }
        }
    }

    #[test]
    fn a_json_body_is_redacted_as_its_tree_and_stays_json_of_the_same_shape() {
        let masked = "[redacted-secret]";
        let cases = [
            (
                r#"{"password":"a\"b","user":"me"}"#,
                json!({"password": masked, "user": "me"}),
            ),
            (
                r#"{"password":"abc\\","user":"me"}"#,
                json!({"password": masked, "user": "me"}),
            ),
            (
                r#"{"db pass":"hunter2","author":"Ada","oauth_scope":"read","keyboard":"us","password hint":"pet"}"#,
                json!({"db pass": masked, "author": "Ada", "oauth_scope": "read", "keyboard": "us", "password hint": masked}),
            ),
            (
                r#"{"sshKey":"hunter2","db.pass":"hunter2","note":"token=abc","api_key":12345,"n":[1,"x"]}"#,
                json!({"sshKey": masked, "db.pass": masked, "note": format!("token={masked}"), "api_key": masked, "n": [1, "x"]}),
            ),
        ];
        for (body, expected) in cases {
            let redacted: serde_json::Value =
                serde_json::from_str(&redact_body(body)).unwrap_or_else(|error| panic!("{error}: {body}"));
            assert_eq!(redacted, expected, "body {body:?}");
        }
        assert_eq!(redact_body("export TOKEN=abc"), format!("export TOKEN={masked}"));
    }

    #[test]
    fn a_call_leaves_with_every_string_redacted_and_every_secret_field_masked() {
        let token = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let masked = "[redacted-secret]";
        assert_eq!(
            redact_call(&json!({
                "name": "mcp__claude_ai_Google_Drive__search_files",
                "description": format!("uses {token}"),
                "arguments": {
                    "list": [token, {"deep": token}], "n": 7, "flag": true, "none": null,
                    "password": "hunter2",
                    "api_key": 12345,
                    "headers": {"Authorization": "Bearer abc", "Accept": "*/*"},
                    "hosts": [{"name": "db", "Client-Secret": {"v": "s"}}, {"sessionCookie": "c", "port": 5432}],
                    "db_passwd": "p",
                    "passphrase": "p",
                    "AWS_ACCESS_KEY": "k",
                    "Auth": "a",
                    "author": "Ada",
                    "oauth_scope": "read",
                },
            })),
            json!({
                "name": "mcp__claude_ai_Google_Drive__search_files",
                "description": format!("uses {masked}"),
                "arguments": {
                    "list": [masked, {"deep": masked}], "n": 7, "flag": true, "none": null,
                    "password": masked,
                    "api_key": masked,
                    "headers": {"Authorization": masked, "Accept": "*/*"},
                    "hosts": [{"name": "db", "Client-Secret": masked}, {"sessionCookie": masked, "port": 5432}],
                    "db_passwd": masked,
                    "passphrase": masked,
                    "AWS_ACCESS_KEY": masked,
                    "Auth": masked,
                    "author": "Ada",
                    "oauth_scope": "read",
                },
            })
        );
        assert_eq!(
            redact_args(&json!({"query": format!("key {token}"), "established": "internal"})),
            json!({"query": format!("key {masked}"), "established": "internal"}),
            "one value per declared input is redacted the same way"
        );
        assert_eq!(
            redact_args(&json!({"name": token, "query": "x"})),
            json!({"name": masked, "query": "x"}),
            "a declared input named `name` is no tool name"
        );
    }

    #[test]
    fn a_credential_used_as_a_key_is_redacted_and_keys_that_redact_alike_keep_one_entry_each() {
        let masked = "[redacted-secret]";
        assert_eq!(
            redact_call(&json!({
                "name": "write",
                "arguments": {
                    "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789": 1,
                    "ghp_ZyXwVuTsRqPoNmLkJiHgFeDcBa9876543210": 2,
                    "AKIAIOSFODNN7EXAMPLE": 3,
                    "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY": 4,
                    format!("{masked}-2"): 5,
                    "Customer-Billing-Zone_V2": {"configuration_management_enabled": 6},
                    "https://alice:hunter2@example.com": 7,
                },
            })),
            json!({
                "name": "write",
                "arguments": {
                    masked: 3,
                    format!("{masked}-2"): masked,
                    format!("{masked}-3"): 1,
                    format!("{masked}-4"): 2,
                    format!("{masked}-5"): 4,
                    "Customer-Billing-Zone_V2": {"configuration_management_enabled": 6},
                    format!("https://alice:{masked}@example.com"): 7,
                },
            })
        );
        assert_eq!(
            redact_args(&json!({"AKIAIOSFODNN7EXAMPLE": {"AKIAIOSFODNN7EXAMPLE": "write"}})),
            json!({"AKIAIOSFODNN7EXAMPLE": {masked: "write"}}),
            "a top-level key is a declared input's name"
        );
    }
}
