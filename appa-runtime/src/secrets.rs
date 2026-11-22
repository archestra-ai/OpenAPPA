//! Secret redaction: the stock `redact-secrets` sanitizer's text pass, and the same pass
//! over an annotation's call before it leaves for a model provider.

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

/// Every secret assignment's value and every netrc password replaced, quotes kept. A
/// quoted value that ends at a backslash-escaped quote is also masked to the end of its
/// line: where it really closes depends on syntax no pattern here sees. A line ends at an
/// ASCII line break, which no JSON string holds unescaped. Netrc passwords are found on
/// the text with only the values to their first closing quote masked, so a `password`
/// keyword inside the rest of such a line still finds its password on the next.
fn mask_keyed_values(text: &str) -> Cow<'_, str> {
    let mut plain: Vec<Range<usize>> = Vec::new();
    let mut escaped: Vec<Range<usize>> = Vec::new();
    let mut current_line_end = 0;
    for found in SECRET_ASSIGNMENT.captures_iter(text) {
        let value = found.get(2).expect("the pattern captures a value");
        match value.as_str().as_bytes() {
            [b'"' | b'\'', .., b'\\', _] => {
                plain.push(value.start() + 1..value.end() - 1);
                if value.end() > current_line_end {
                    current_line_end = line_end(text, value.end());
                }
                escaped.push(value.start() + 1..current_line_end);
            }
            [b'"' | b'\'', ..] => plain.push(value.start() + 1..value.end() - 1),
            _ => plain.push(value.range()),
        }
    }
    let masked = mask_spans(text, &plain);
    let mut source = SourceOffsets::new(&plain);
    let passwords: Vec<Range<usize>> = NETRC_PASSWORD
        .captures_iter(&masked)
        .map(|found| {
            let password = found.get(2).expect("the pattern captures a password");
            source.of(password.start())..source.of(password.end())
        })
        .collect();
    match (escaped.is_empty(), passwords.is_empty()) {
        (true, true) => masked,
        _ => {
            let mut spans = plain;
            spans.extend(escaped);
            spans.extend(passwords);
            Cow::Owned(mask_spans(text, &merged(spans)).into_owned())
        }
    }
}

/// The offset of the first ASCII line break at or after `from`, or the end of the text.
fn line_end(text: &str, from: usize) -> usize {
    text[from..]
        .find(['\n', '\x0B', '\x0C', '\r'])
        .map_or(text.len(), |found| from + found)
}

/// The spans sorted, with every overlapping pair joined.
fn merged(mut spans: Vec<Range<usize>>) -> Vec<Range<usize>> {
    spans.sort_by_key(|span| (span.start, span.end));
    spans.dedup_by(|next, kept| match next.start < kept.end {
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
    match SECRET_FIELD.is_match(key) {
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
            (
                "token=\"x\npassword inner\"suffix\"",
                format!("token=\"{masked}\"suffix\""),
            ),
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
    fn a_quoted_secret_with_an_escaped_quote_is_masked_to_the_end_of_its_line() {
        let masked = "[redacted-secret]";
        let cases = [
            (r#"password="ab\"cdefgh" next"#, format!(r#"password="{masked}"#)),
            (r"password='ab\'cdefgh' next", format!("password='{masked}")),
            (r#"PASSWORD="a\"b\"c\"tail">out"#, format!(r#"PASSWORD="{masked}"#)),
            (r"PASSWORD='a\'b\'tail'>out", format!("PASSWORD='{masked}")),
            (
                r#"export DB_PASSWORD="ab\"cdefgh">out"#,
                format!(r#"export DB_PASSWORD="{masked}"#),
            ),
            (
                r#"{"password": "a\\\"b\\c", "user": "me"}"#,
                format!(r#"{{"password": "{masked}"#),
            ),
            (
                "{\"password\":\"ab\\\"cd\u{2028}efgh\"}",
                format!("{{\"password\":\"{masked}"),
            ),
            (
                "DIR_PASSWORD=\"C:\\dir\\\"\nDB_PASSWORD=\"hunter2\"\nUSER=me",
                format!("DIR_PASSWORD=\"{masked}\nDB_PASSWORD=\"{masked}\"\nUSER=me"),
            ),
            (
                "PASSWORD=\"ab\\\"cd\"\r\nUSER=me",
                format!("PASSWORD=\"{masked}\r\nUSER=me"),
            ),
            (
                "token=\"ab\ncd\\\"ef\" tail\nNEXT=1",
                format!("token=\"{masked}\nNEXT=1"),
            ),
            (
                r#"machine api.example.com login token="ab\" password "hunter2""#,
                format!(r#"machine api.example.com login token="{masked}"#),
            ),
            (
                "token=\"ab\\\"cd\nmachine h login me password hunter2",
                format!("token=\"{masked}\nmachine h login me password {masked}"),
            ),
            (
                "machine h login token=\"a\\\"b\" password\nhunter2",
                format!("machine h login token=\"{masked}\n{masked}"),
            ),
            (
                "password=\"a\\\"b\" TOKEN=\"\"\r\npassword hunter2",
                format!("password=\"{masked}\r\npassword {masked}"),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_secrets(input), expected, "input {input:?}");
        }
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
