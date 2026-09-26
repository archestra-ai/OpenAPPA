//! The fresh deployment policy, adapted only where a platform cannot serve one
//! of its builtins.

use std::borrow::Cow;

const TEMPLATE: &str = include_str!("../../marketplace/plugins/claude-code/default.appa.toml");
const UNIX_FALLBACK_BEGIN: &str = "# APPA-UNIX-FALLBACK-BEGIN";
const UNIX_FALLBACK_END: &str = "# APPA-UNIX-FALLBACK-END";

/// The policy written for a fresh deployment on this platform.
pub(crate) fn text() -> Cow<'static, str> {
    for_installed_policy(TEMPLATE).expect("the bundled default policy marks its Unix-only fallback")
}

/// Adapt the selected version's Claude policy, not this executable's embedded version.
pub(crate) fn for_installed_policy(template: &str) -> Result<Cow<'_, str>, &'static str> {
    for_template(template, cfg!(unix))
}

fn for_template(template: &str, supports_claude_subprocess: bool) -> Result<Cow<'_, str>, &'static str> {
    if supports_claude_subprocess {
        return Ok(Cow::Borrowed(template));
    }

    let (before, marked) = template
        .split_once(UNIX_FALLBACK_BEGIN)
        .ok_or("the Claude default policy does not mark its Unix-only fallback")?;
    let (_, after) = marked
        .split_once(UNIX_FALLBACK_END)
        .ok_or("the Claude default policy does not close its Unix-only fallback")?;
    Ok(Cow::Owned(format!(
        "{before}# This platform cannot run the Claude subprocess fallback. Tools not declared above remain fail-closed.{after}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_keeps_the_undeclared_tool_fallback() {
        let config = for_template(TEMPLATE, true).unwrap();
        assert!(config.contains("name = \"claude-code.bash-requirements\""));
        assert!(config.contains("name = \"claude-code.undeclared-tool\""));
        assert!(config.contains("name = \"*\""));
    }

    #[test]
    fn root_credentials_match_the_battery_before_the_bash_annotator() {
        let root: toml::Value = toml::from_str(TEMPLATE).unwrap();
        let battery: toml::Value = toml::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../marketplace/batteries/claude-code/appa.toml"
        )))
        .unwrap();
        let root_rules = root["policy"]["tool"].as_array().unwrap();
        let bare_bash = root_rules
            .iter()
            .position(|rule| rule["name"].as_str() == Some("host/claude-code/Bash"))
            .unwrap();
        let credentials = battery["policy"]["tool"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|rule| {
                rule["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("host/claude-code/Bash("))
                    && rule
                        .get("tags")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|tags| tags.iter().any(|tag| tag.as_str() == Some("credentials")))
            })
            .collect::<Vec<_>>();
        assert!(!credentials.is_empty());
        for credential in credentials {
            let position = root_rules
                .iter()
                .position(|rule| rule["name"] == credential["name"])
                .expect("every battery Bash credential selector is mirrored in the root");
            assert!(
                position < bare_bash,
                "credential selector must precede the bare Bash rule"
            );
            assert_eq!(&root_rules[position], credential);
        }
    }

    #[test]
    fn default_human_authority_can_review_public_audience_expansion() {
        let root: toml::Value = toml::from_str(TEMPLATE).expect("the default config is TOML");
        let policy = toml::to_string(&root["policy"]).expect("the default policy renders");
        let compiled = appa_policy::Config::from_toml_str(&policy).expect("the default policy compiles");
        let authority = compiled
            .engine()
            .registry()
            .authority(&appa_engine::names::AuthorityName::new("hitl"))
            .expect("the default registers the human authority");

        assert!(matches!(
            authority.mandate.reader_ceiling,
            Some(appa_engine::label::DeclaredAudience::Public)
        ));
        assert_eq!(authority.mandate.attends, appa_engine::authority::Attends::Any);
        let trusted = compiled
            .engine()
            .registry()
            .trust_chain()
            .rank_of("trusted")
            .expect("the default chain names trusted");
        assert_eq!(authority.mandate.trust_ceiling, Some(trusted));
    }

    #[test]
    fn platforms_without_the_claude_subprocess_fail_closed_on_undeclared_tools() {
        let config = for_template(TEMPLATE, false).unwrap();
        assert!(!config.contains("name = \"claude-code.bash-requirements\""));
        assert!(!config.contains("name = \"claude-code.undeclared-tool\""));
        assert!(!config.contains("name = \"*\""));

        let directory = tempfile::tempdir().expect("a temporary directory");
        let battery = directory.path().join("batteries/claude-code");
        std::fs::create_dir_all(&battery).expect("the battery directory is created");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../marketplace/batteries/claude-code/appa.toml"
            ),
            battery.join("appa.toml"),
        )
        .expect("the Claude battery is copied");
        let path = directory.path().join("appa.toml");
        std::fs::write(
            &path,
            format!("include = [\"batteries/claude-code/appa.toml\"]\n{config}"),
        )
        .expect("the portable default is written");
        let loaded = crate::config::Config::load(&path).expect("the portable default and battery load");
        assert!(loaded.externals.inputs.is_empty());
        let policy = loaded.policy_file().value();
        assert!(
            !policy["tool"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"].as_str() == Some("host/claude-code/Bash"))
        );
        assert!(
            !policy
                .get("annotator")
                .and_then(toml::Value::as_array)
                .is_some_and(|annotators| annotators
                    .iter()
                    .any(|annotator| { annotator["builtin"].as_str() == Some("claude-code") }))
        );
        crate::api::Runtime::open(loaded, directory.path().join("appa.db"), None)
            .expect("the portable deployment opens");
    }

    #[test]
    fn windows_line_endings_do_not_change_the_platform_markers() {
        let windows = TEMPLATE.replace('\n', "\r\n");
        let portable = for_template(&windows, false).unwrap();
        assert!(!portable.contains("name = \"claude-code.undeclared-tool\""));
        assert!(!portable.contains("name = \"*\""));
    }

    #[test]
    fn installed_policy_uses_the_selected_text_and_refuses_missing_markers() {
        let selected = TEMPLATE.replace("agent_yell = false", "agent_yell = true");
        assert!(for_template(&selected, false).unwrap().contains("agent_yell = true"));
        assert!(for_template("[policy]\nversion = 2\n", false).is_err());
        assert!(for_template("[policy]\nversion = 2\n", true).is_ok());
    }
}
