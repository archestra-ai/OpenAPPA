//! The fresh deployment policy, adapted only where a platform cannot serve one
//! of its builtins.

use std::borrow::Cow;

const TEMPLATE: &str = include_str!("../../integrations/claude-code/examples/claude-code.appa.toml");
const UNIX_FALLBACK_BEGIN: &str = "# APPA-UNIX-FALLBACK-BEGIN";
const UNIX_FALLBACK_END: &str = "# APPA-UNIX-FALLBACK-END";

/// The policy written for a fresh deployment on this platform.
pub(crate) fn text() -> Cow<'static, str> {
    for_platform(cfg!(unix))
}

fn for_platform(supports_claude_subprocess: bool) -> Cow<'static, str> {
    for_template(TEMPLATE, supports_claude_subprocess)
}

fn for_template(template: &str, supports_claude_subprocess: bool) -> Cow<'_, str> {
    if supports_claude_subprocess {
        return Cow::Borrowed(template);
    }

    let (before, marked) = template
        .split_once(UNIX_FALLBACK_BEGIN)
        .expect("the default policy marks its Unix-only fallback");
    let (_, after) = marked
        .split_once(UNIX_FALLBACK_END)
        .expect("the default policy closes its Unix-only fallback");
    Cow::Owned(format!(
        "{before}# This platform cannot run the Claude subprocess fallback. Tools not declared above remain fail-closed.{after}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_keeps_the_undeclared_tool_fallback() {
        let config = for_platform(true);
        assert!(config.contains("name = \"claude-code.bash-requirements\""));
        assert!(config.contains("name = \"claude-code.undeclared-tool\""));
        assert!(config.contains("name = \"*\""));
    }

    #[test]
    fn platforms_without_the_claude_subprocess_fail_closed_on_undeclared_tools() {
        let config = for_platform(false);
        assert!(!config.contains("name = \"claude-code.bash-requirements\""));
        assert!(!config.contains("name = \"claude-code.undeclared-tool\""));
        assert!(!config.contains("name = \"*\""));

        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("appa.toml");
        std::fs::write(&path, config.as_bytes()).expect("the portable default is written");
        crate::config::Config::load(&path).expect("the portable default config loads");
    }

    #[test]
    fn windows_line_endings_do_not_change_the_platform_markers() {
        let windows = TEMPLATE.replace('\n', "\r\n");
        let portable = for_template(&windows, false);
        assert!(!portable.contains("name = \"claude-code.undeclared-tool\""));
        assert!(!portable.contains("name = \"*\""));
    }
}
