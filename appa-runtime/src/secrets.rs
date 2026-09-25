//! Secret redaction: the stock `redact-secrets` sanitizer's text pass, and the same pass
//! over an annotation's call before it leaves for a model provider.

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

pub(crate) fn redact_secrets(input: &str) -> String {
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

/// A field whose name contains one of these words, or is `auth`, holds a secret, whatever
/// its value.
static SECRET_FIELD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)api[_-]?key|secret|token|password|passwd|passphrase|authorization|cookie|credential|private[_-]?key|access[_-]?key|^auth$",
    )
    .expect("a fixed pattern")
});

/// An annotation's `args` as they leave for a model provider: every string through
/// [`redact_secrets`], and the whole value of a field named for a secret replaced. The
/// complete call's tool `name` leaves as written.
pub(crate) fn redact_args(args: &serde_json::Value) -> serde_json::Value {
    let mut redacted = redact_value(args);
    if let (Some(name), serde_json::Value::Object(fields)) =
        (args.get("name").filter(|name| name.is_string()), &mut redacted)
    {
        fields.insert("name".to_string(), name.clone());
    }
    redacted
}

fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(redact_secrets(text)),
        serde_json::Value::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| match SECRET_FIELD.is_match(key) {
                    true => (key.clone(), serde_json::Value::String(SECRET_PLACEHOLDER.to_string())),
                    false => (key.clone(), redact_value(value)),
                })
                .collect(),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(items.iter().map(redact_value).collect()),
        other => other.clone(),
    }
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
    fn a_call_leaves_with_every_string_redacted_and_every_secret_field_masked() {
        let token = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let masked = "[redacted-secret]";
        assert_eq!(
            redact_args(&json!({
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
    }
}
