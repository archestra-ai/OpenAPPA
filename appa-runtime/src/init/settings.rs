//! The user's Claude Code settings file: the hook entries of a protected
//! session and its status line, written beside whatever else the file holds.
//!
//! Every entry names the deployed binary by absolute path in exec form, so
//! Claude Code runs it with no shell and no PATH lookup; the binary reads
//! `APPA_GATE` itself, so an unprotected session pays one process start and
//! nothing else. An entry is this deployment's exactly when its `command` is
//! this deployment's binary path. Foreign entries are kept as the JSON values
//! they are, and the file is rewritten only when its value would change.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::paths::DeploymentPaths;
use super::{Compensation, InitError, Undo, file_before, write_state};

/// What every hook entry needs to know about its deployment.
pub(super) struct HookTarget<'a> {
    pub(super) binary: &'a Path,
    pub(super) url: &'a str,
    pub(super) config: &'a Path,
    pub(super) data_dir: &'a Path,
}

/// Claude Code kills a hook that outruns its timeout and lets the action
/// proceed without reading the exit code, so every timeout sits above the
/// deadline the entry's own client gives up at. SessionStart's covers the
/// runtime start it chains in front of its post, and cannot block anyway.
const SESSION_START_TIMEOUT: Duration = Duration::from_secs(150);
const AUTHORIZATION_TIMEOUT: Duration = Duration::from_secs(130);
const TURN_END_TIMEOUT: Duration = Duration::from_secs(40);

struct Event {
    name: &'static str,
    matcher: Option<&'static str>,
    turn_end: bool,
}

/// The events a protected session posts, after SessionStart. Stop and
/// StopFailure report a finished turn, which decides nothing, so their entry
/// never blocks and takes the shorter deadline.
const EVENTS: [Event; 8] = [
    Event {
        name: "UserPromptSubmit",
        matcher: None,
        turn_end: false,
    },
    Event {
        name: "PreToolUse",
        matcher: Some("*"),
        turn_end: false,
    },
    Event {
        name: "PostToolUse",
        matcher: Some("*"),
        turn_end: false,
    },
    Event {
        name: "PostToolUseFailure",
        matcher: Some("*"),
        turn_end: false,
    },
    Event {
        name: "Stop",
        matcher: None,
        turn_end: true,
    },
    Event {
        name: "StopFailure",
        matcher: None,
        turn_end: true,
    },
    Event {
        name: "SubagentStart",
        matcher: None,
        turn_end: false,
    },
    Event {
        name: "SubagentStop",
        matcher: None,
        turn_end: false,
    },
];

pub(super) fn path(paths: &DeploymentPaths) -> PathBuf {
    paths.claude_dir.join("settings.json")
}

/// The file parses as the object every edit below expects, or the profile is
/// refused before anything is written to it.
pub(super) fn verify(paths: &DeploymentPaths) -> Result<(), InitError> {
    let path = path(paths);
    read(&path).map(drop)
}

/// Register every entry of a protected session, replacing this deployment's
/// earlier ones and leaving every other entry where it is.
pub(super) fn install_hooks(
    paths: &DeploymentPaths,
    target: &HookTarget<'_>,
    compensation: &mut Compensation,
) -> Result<(), InitError> {
    let path = path(paths);
    let binary = portable(target.binary)?;
    edit(&path, Some(compensation), |settings| {
        let hooks = object_entry(settings, "hooks", &path)?;
        for (event, group) in groups(target, binary)? {
            let groups = array_entry(hooks, event, &path)?;
            drop_owned(groups, binary);
            groups.push(group);
        }
        Ok(())
    })
}

/// Drop this deployment's entries from every event, whatever events an earlier
/// install registered them under.
pub(super) fn remove_hooks(paths: &DeploymentPaths, binary: &Path) -> Result<(), InitError> {
    let path = path(paths);
    let binary = portable(binary)?;
    edit(&path, None, |settings| {
        let Some(hooks) = settings.get_mut("hooks") else {
            return Ok(());
        };
        let hooks = hooks
            .as_object_mut()
            .ok_or_else(|| conflict(&path, "hooks must be an object"))?;
        for groups in hooks.values_mut() {
            if let Some(groups) = groups.as_array_mut() {
                drop_owned(groups, binary);
            }
        }
        hooks.retain(|_, groups| groups.as_array().is_none_or(|groups| !groups.is_empty()));
        if hooks.is_empty() {
            settings.remove("hooks");
        }
        Ok(())
    })
}

/// Point the status line at the deployed binary, unless a status line that is
/// not this deployment's is configured, which is the user's and left alone.
pub(super) fn install_statusline(
    paths: &DeploymentPaths,
    target: &HookTarget<'_>,
    compensation: &mut Compensation,
) -> Result<(), InitError> {
    let path = path(paths);
    let command = statusline_command(target.binary, target.url);
    edit(&path, Some(compensation), |settings| {
        if let Some(line) = settings.get("statusLine")
            && !names_binary(line, target.binary)
        {
            return Ok(());
        }
        settings.insert("statusLine".to_owned(), json!({"type": "command", "command": command}));
        Ok(())
    })
}

pub(super) fn remove_statusline(paths: &DeploymentPaths, binary: &Path) -> Result<(), InitError> {
    let path = path(paths);
    edit(&path, None, |settings| {
        if settings
            .get("statusLine")
            .is_some_and(|line| names_binary(line, binary))
        {
            settings.remove("statusLine");
        }
        Ok(())
    })
}

/// The status line is a shell string, the one place Claude Code gives no exec
/// form, so the binary path is quoted for the shell that runs it.
pub(super) fn statusline_command(binary: &Path, url: &str) -> String {
    let head = statusline_head(binary);
    if cfg!(windows) {
        format!("{head} --deployment-url {}\"", ps_literal(url))
    } else {
        format!("{head} --deployment-url {}", sh_literal(url))
    }
}

/// The command up to its arguments: the deployed binary, run as the status
/// line and nothing else.
fn statusline_head(binary: &Path) -> String {
    let binary = binary.to_string_lossy();
    if cfg!(windows) {
        format!(
            "powershell.exe -NoProfile -Command \"& {} statusline",
            ps_literal(&binary)
        )
    } else {
        format!("{} statusline", sh_literal(&binary))
    }
}

/// A status line is this deployment's when its command is the deployed binary
/// run as the status line, whatever endpoint an earlier install gave it. A
/// command of the user's own that runs the binary among other things is theirs.
fn names_binary(line: &Value, binary: &Path) -> bool {
    let head = statusline_head(binary);
    line.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.starts_with(&head))
}

/// Total for any UTF-8 path: single-quoted, with embedded `'` closed, escaped
/// and reopened. Spaces, `$`, backticks, quotes and newlines are all
/// representable, so rendering never refuses a path that got this far.
pub(crate) fn sh_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Total for any UTF-8 path: single-quoted, with embedded `'` doubled.
pub(crate) fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// One group per event, in the shape Claude Code reads: `matcher` where the
/// event takes one, then the entries.
fn groups(target: &HookTarget<'_>, binary: &str) -> Result<Vec<(&'static str, Value)>, InitError> {
    let url = target.url;
    let entry = |args: Vec<String>, timeout: Duration| json!({"type": "command", "command": binary, "args": args, "timeout": timeout.as_secs()});
    let mut groups = Vec::with_capacity(EVENTS.len() + 1);
    // The start of the deployed runtime and the first post share one process:
    // Claude Code runs an event's entries in parallel, so a separate start
    // entry could not be ordered before the post. The advice entry beside it
    // only prints, and blocks nothing.
    let session_start = json!({"hooks": [
        entry(
            vec![
                "hook".to_owned(),
                "--deployment-url".to_owned(),
                url.to_owned(),
                "--ensure-runtime".to_owned(),
                "--config".to_owned(),
                portable(target.config)?.to_owned(),
                "--data-dir".to_owned(),
                portable(target.data_dir)?.to_owned(),
            ],
            SESSION_START_TIMEOUT,
        ),
        json!({"type": "command", "command": binary, "args": ["session-context"]}),
    ]});
    groups.push(("SessionStart", session_start));
    for event in EVENTS {
        let mut args = vec!["hook".to_owned(), "--deployment-url".to_owned(), url.to_owned()];
        let timeout = if event.turn_end {
            args.push("--turn-end".to_owned());
            TURN_END_TIMEOUT
        } else {
            AUTHORIZATION_TIMEOUT
        };
        let mut group = Map::new();
        if let Some(matcher) = event.matcher {
            group.insert("matcher".to_owned(), Value::String(matcher.to_owned()));
        }
        group.insert("hooks".to_owned(), json!([entry(args, timeout)]));
        groups.push((event.name, Value::Object(group)));
    }
    Ok(groups)
}

/// Drop this deployment's entries from every group, and the groups that held
/// nothing else. A group that was empty to begin with is not ours to judge.
fn drop_owned(groups: &mut Vec<Value>, binary: &str) {
    groups.retain_mut(|group| {
        let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = hooks.len();
        hooks.retain(|hook| hook.get("command").and_then(Value::as_str) != Some(binary));
        !(before > 0 && hooks.is_empty())
    });
}

/// Apply one edit to the settings object, and write the file only when the
/// value changed, recording its previous bytes when a compensation is given.
fn edit(
    path: &Path,
    compensation: Option<&mut Compensation>,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), InitError>,
) -> Result<(), InitError> {
    let before = file_before(path)?;
    let mut settings = parse(path, before.as_deref())?;
    let original = settings.clone();
    edit(&mut settings)?;
    if settings == original {
        return Ok(());
    }
    if let Some(compensation) = compensation {
        compensation.record(Undo::File {
            path: path.to_path_buf(),
            before,
        });
    }
    let mut encoded = serde_json::to_vec_pretty(&Value::Object(settings)).expect("JSON values encode");
    encoded.push(b'\n');
    write_state(path, &encoded)
}

fn read(path: &Path) -> Result<Map<String, Value>, InitError> {
    let bytes = file_before(path)?;
    parse(path, bytes.as_deref())
}

/// An absent file is an empty object; anything else must be an object.
fn parse(path: &Path, bytes: Option<&[u8]>) -> Result<Map<String, Value>, InitError> {
    let Some(bytes) = bytes else {
        return Ok(Map::new());
    };
    let value: Value = serde_json::from_slice(bytes).map_err(|error| conflict(path, &error.to_string()))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(conflict(path, "expected a JSON object")),
    }
}

fn object_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>, InitError> {
    object
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| conflict(path, &format!("{key} must be an object")))
}

fn array_entry<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Vec<Value>, InitError> {
    object
        .entry(key)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| conflict(path, &format!("hooks.{key} must be an array")))
}

/// A hook command is exact bytes to Claude Code; a path this process cannot
/// spell as UTF-8 cannot be written as one.
fn portable(path: &Path) -> Result<&str, InitError> {
    path.to_str()
        .ok_or_else(|| InitError::UnportablePath { path: path.to_owned() })
}

fn conflict(path: &Path, message: &str) -> InitError {
    InitError::NativeState {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(root: &Path) -> (DeploymentPaths, PathBuf) {
        let paths = DeploymentPaths {
            install_dir: root.join("bin"),
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            claude_dir: root.join("claude"),
        };
        fs::create_dir_all(&paths.claude_dir).unwrap();
        let binary = paths.data_dir.join("bin/appa");
        (paths, binary)
    }

    fn settings(paths: &DeploymentPaths) -> Map<String, Value> {
        read(&path(paths)).unwrap()
    }

    fn write(paths: &DeploymentPaths, value: Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(path(paths), &bytes).unwrap();
        bytes
    }

    #[test]
    fn hook_entries_are_written_once_and_foreign_entries_survive_install_and_removal() {
        let root = tempfile::tempdir().unwrap();
        let (paths, binary) = fixture(root.path());
        let foreign_group = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "lint"}]});
        let foreign_hook = json!({"type": "command", "command": "audit", "timeout": 3});
        let original = json!({
            "theme": "dark",
            "hooks": {
                "PreToolUse": [foreign_group.clone()],
                // A group this deployment shares with a foreign entry keeps that entry.
                "Stop": [{"hooks": [foreign_hook.clone(), {"type": "command", "command": binary, "args": ["hook", "--turn-end"]}]}],
                "Notification": [{"hooks": [{"type": "command", "command": "notify"}]}]
            }
        });
        write(&paths, original.clone());
        let target = HookTarget {
            binary: &binary,
            url: "http://127.0.0.1:1",
            config: &paths.config_dir.join("appa.toml"),
            data_dir: &paths.data_dir,
        };

        let mut compensation = Compensation::default();
        install_hooks(&paths, &target, &mut compensation).unwrap();
        assert_eq!(compensation.done.len(), 1);
        let installed = settings(&paths);
        assert_eq!(installed["theme"], "dark");
        assert_eq!(installed["hooks"]["Notification"], original["hooks"]["Notification"]);
        let pre_tool_use = installed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use[0], foreign_group);
        assert_eq!(pre_tool_use[1]["matcher"], "*");
        assert_eq!(pre_tool_use[1]["hooks"][0]["command"], binary.to_str().unwrap());
        let stop = installed["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop[0], json!({"hooks": [foreign_hook.clone()]}));
        assert_eq!(
            stop[1]["hooks"][0]["args"],
            json!(["hook", "--deployment-url", "http://127.0.0.1:1", "--turn-end"])
        );
        assert_eq!(stop.len(), 2);
        let session_start = &installed["hooks"]["SessionStart"][0]["hooks"];
        assert_eq!(session_start[0]["args"][3], "--ensure-runtime");
        assert_eq!(session_start[1]["args"], json!(["session-context"]));

        // The same install again changes nothing and records nothing.
        let bytes = fs::read(path(&paths)).unwrap();
        let mut again = Compensation::default();
        install_hooks(&paths, &target, &mut again).unwrap();
        assert!(again.done.is_empty());
        assert_eq!(fs::read(path(&paths)).unwrap(), bytes);

        remove_hooks(&paths, &binary).unwrap();
        let removed = settings(&paths);
        assert_eq!(removed["theme"], "dark");
        assert_eq!(removed["hooks"]["PreToolUse"], json!([foreign_group]));
        assert_eq!(removed["hooks"]["Stop"], json!([{"hooks": [foreign_hook]}]));
        assert_eq!(removed["hooks"]["Notification"], original["hooks"]["Notification"]);
        assert!(removed["hooks"].get("SessionStart").is_none());

        // With nothing foreign, removal leaves no empty `hooks` behind.
        write(&paths, json!({}));
        install_hooks(&paths, &target, &mut Compensation::default()).unwrap();
        remove_hooks(&paths, &binary).unwrap();
        assert_eq!(settings(&paths), Map::new());
    }

    #[test]
    fn a_status_line_that_is_not_this_deployments_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let (paths, binary) = fixture(root.path());
        let target = HookTarget {
            binary: &binary,
            url: "http://127.0.0.1:1",
            config: &paths.config_dir.join("appa.toml"),
            data_dir: &paths.data_dir,
        };
        let mut compensation = Compensation::default();
        // A command of the user's own, and one of theirs that runs this deployment's
        // status line among other things: both are left alone.
        let composed = format!(
            "input=$(cat); printf '%s' \"$input\" | my-status; printf '%s' \"$input\" | {}",
            statusline_command(&binary, "http://127.0.0.1:1")
        );
        for command in ["my-status", composed.as_str()] {
            let custom = json!({"statusLine": {"type": "command", "command": command, "padding": 0}});
            let bytes = write(&paths, custom.clone());
            install_statusline(&paths, &target, &mut compensation).unwrap();
            assert!(compensation.done.is_empty(), "{command}");
            assert_eq!(fs::read(path(&paths)).unwrap(), bytes, "{command}");
            remove_statusline(&paths, &binary).unwrap();
            assert_eq!(fs::read(path(&paths)).unwrap(), bytes, "{command}");
        }

        // Absent, then this deployment's under an earlier URL: written, then repaired.
        write(&paths, json!({}));
        install_statusline(&paths, &target, &mut compensation).unwrap();
        let command = settings(&paths)["statusLine"]["command"].as_str().unwrap().to_owned();
        assert_eq!(command, statusline_command(&binary, "http://127.0.0.1:1"));
        let earlier = HookTarget {
            url: "http://127.0.0.1:2",
            ..target
        };
        install_statusline(&paths, &earlier, &mut compensation).unwrap();
        assert_eq!(
            settings(&paths)["statusLine"]["command"],
            statusline_command(&binary, "http://127.0.0.1:2")
        );
        assert_eq!(compensation.done.len(), 2);
        remove_statusline(&paths, &binary).unwrap();
        assert!(settings(&paths).get("statusLine").is_none());
    }

    #[test]
    fn a_malformed_settings_file_is_refused_before_it_is_written() {
        let root = tempfile::tempdir().unwrap();
        let (paths, binary) = fixture(root.path());
        for bytes in ["{", "[]", "\"text\""] {
            fs::write(path(&paths), bytes).unwrap();
            assert!(verify(&paths).is_err(), "{bytes:?} must be refused");
            assert!(remove_hooks(&paths, &binary).is_err());
            assert_eq!(fs::read_to_string(path(&paths)).unwrap(), bytes);
        }
        fs::remove_file(path(&paths)).unwrap();
        verify(&paths).unwrap();
    }

    /// The runtime start budget and the client's own deadlines are the
    /// timeouts' floor; a change to either side that crosses it fails here,
    /// not as a hook the harness kills and then lets proceed.
    #[test]
    fn timeouts_outlast_every_client_deadline() {
        use crate::hook_client::{AUTHORIZATION_BUDGET, TURN_END_BUDGET};
        use crate::runtime_start::{START_BUDGET, STOP_BUDGET};
        assert!(SESSION_START_TIMEOUT >= STOP_BUDGET + START_BUDGET + AUTHORIZATION_BUDGET);
        assert!(AUTHORIZATION_TIMEOUT > AUTHORIZATION_BUDGET);
        assert!(TURN_END_TIMEOUT > TURN_END_BUDGET);
    }

    #[cfg(unix)]
    #[test]
    fn the_status_line_command_carries_a_hostile_binary_path_as_one_word() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("it's a dir/appa");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(
            &binary,
            "#!/bin/sh\nfor argument; do printf '%s\\n' \"$argument\"; done\n",
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(statusline_command(&binary, "http://127.0.0.1:1"))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "statusline\n--deployment-url\nhttp://127.0.0.1:1\n"
        );
    }

    #[test]
    fn literals_carry_hostile_characters() {
        let awkward = "it's $HOME `here` \"there\"\nnext";
        assert_eq!(sh_literal(awkward), "'it'\\''s $HOME `here` \"there\"\nnext'");
        assert_eq!(ps_literal("it's"), "'it''s'");
    }
}
