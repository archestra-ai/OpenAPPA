#!/usr/bin/env python3
"""Probe the installed Codex hook contract in a disposable project.

The probe asks for one synthetic shell command. It never copies authentication
files, edits the caller's Codex configuration, or runs a command in the caller's
repository. The report contains hook field names and synthetic marker results.
"""

import argparse
import hashlib
import json
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tempfile


HOOK = r'''import json, pathlib, sys
event = json.load(sys.stdin)
path = pathlib.Path(__file__).with_name("events.jsonl")
with path.open("a", encoding="utf-8") as output:
    output.write(json.dumps(event) + "\n")
if event.get("hook_event_name") == "PreToolUse":
    if event.get("tool_name") == "Bash":
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {"command": "printf APPA_PROBE_REWRITTEN"}}}))
    else:
        print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "Probe permits only synthetic Bash"}}))
else:
    print("{}")
'''

ORIGINAL = "printf APPA_PROBE_ORIGINAL"
REWRITTEN = "printf APPA_PROBE_REWRITTEN"


def marker(text):
    """Keep only synthetic observations from otherwise private CLI output."""
    if not isinstance(text, str):
        return "absent"
    if "APPA_PROBE_REWRITTEN" in text:
        return "rewritten"
    if "APPA_PROBE_ORIGINAL" in text:
        return "original"
    return "other"


def codex_identity(executable):
    resolved = pathlib.Path(shutil.which(executable) or executable).resolve()
    version = subprocess.run([str(resolved), "--version"], capture_output=True, text=True, timeout=10, check=False)
    # The probe uses --ignore-user-config. An empty CODEX_HOME reports the same
    # default feature set without inheriting unrelated user config or auth.
    with tempfile.TemporaryDirectory(prefix="appa-codex-features-") as home:
        environment = dict(os.environ, CODEX_HOME=home)
        features = subprocess.run(
            [str(resolved), "features", "list", "-c", "features.hooks=true"],
            capture_output=True, text=True, timeout=10, check=False, env=environment,
        )
    return {
        "version": version.stdout.strip(),
        "binary_sha256": hashlib.sha256(resolved.read_bytes()).hexdigest(),
        "os": platform.platform(),
        "architecture": platform.machine(),
        "enabled_features": [line.split()[0] for line in features.stdout.splitlines() if line.split() and line.split()[-1] == "true"],
        "feature_query_status": features.returncode,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", default="codex")
    args = parser.parse_args()
    identity = codex_identity(args.codex)
    with tempfile.TemporaryDirectory(prefix="appa-codex-probe-") as directory:
        project = pathlib.Path(directory)
        script = project / "hook.py"
        script.write_text(HOOK, encoding="utf-8")
        handler = json.dumps(f"{sys.executable} {script}")
        hook = '[{hooks=[{type="command",command=' + handler + '}]}]'
        command = [
            args.codex, "exec", "--ephemeral", "--ignore-user-config", "--ignore-rules",
            "--skip-git-repo-check", "--dangerously-bypass-hook-trust", "--json",
            "--sandbox", "workspace-write", "-C", str(project),
            "-c", "features.hooks=true", "-c", f"hooks.PreToolUse={hook}",
            "-c", f"hooks.PostToolUse={hook}",
            "Call the shell once to run `printf APPA_PROBE_ORIGINAL`. Then report its stdout marker. Do nothing else.",
        ]
        try:
            completed = subprocess.run(command, cwd=project, capture_output=True, text=True, timeout=120, check=False)
        except subprocess.TimeoutExpired:
            print(json.dumps({"status": "timeout"}))
            return 1
        events_path = project / "events.jsonl"
        events = [json.loads(line) for line in events_path.read_text().splitlines()] if events_path.exists() else []
        jsonl = []
        for line in completed.stdout.splitlines():
            try:
                jsonl.append(json.loads(line))
            except json.JSONDecodeError:
                pass
        completed_items = [event.get("item", {}) for event in jsonl if event.get("type") == "item.completed"]
        model_messages = [item.get("text") for item in completed_items if item.get("type") == "agent_message"]
        command_items = [item for item in completed_items if item.get("type") == "command_execution"]
        pre = [event for event in events if event.get("hook_event_name") == "PreToolUse" and event.get("tool_name") == "Bash"]
        post = [event for event in events if event.get("hook_event_name") == "PostToolUse" and event.get("tool_name") == "Bash"]
        pre_command = pre[0].get("tool_input", {}).get("command") if pre else None
        post_command = post[0].get("tool_input", {}).get("command") if post else None
        post_result = post[0].get("tool_response") if post else None
        valid = (
            completed.returncode == 0 and len(pre) == 1 and len(post) == 1
            and pre_command == ORIGINAL
            and pre[0].get("tool_use_id") == post[0].get("tool_use_id")
            and post_command == REWRITTEN
            and len(command_items) == 1
            and marker(command_items[0].get("command")) == "rewritten"
            and ORIGINAL not in (command_items[0].get("command") or "")
            and marker(command_items[0].get("aggregated_output")) == "rewritten"
            and ORIGINAL not in (command_items[0].get("aggregated_output") or "")
            and marker(post_result) == "rewritten"
            and ORIGINAL not in (post_result or "")
            and bool(model_messages)
            and marker(model_messages[-1]) == "rewritten"
            and ORIGINAL not in (model_messages[-1] or "")
        )
        report = {
            "status": "pass" if valid else "fail",
            "codex": identity,
            "exit_code": completed.returncode,
            "hooks": [
                {
                    "event": event.get("hook_event_name"),
                    "tool": event.get("tool_name"),
                    "keys": sorted(event),
                    "tool_input_keys": sorted(event.get("tool_input", {})) if isinstance(event.get("tool_input"), dict) else None,
                    "tool_response_type": type(event.get("tool_response")).__name__,
                }
                for event in events
            ],
            "rewrite": {
                "before_command": pre_command if pre_command in (ORIGINAL, REWRITTEN) else "unexpected_or_absent",
                "updated_input_command": REWRITTEN,
                "post_hook_command": post_command if post_command in (ORIGINAL, REWRITTEN) else "unexpected_or_absent",
                "post_hook_result": marker(post_result),
                "command_items": [
                    {
                        "command_marker": marker(item.get("command")),
                        "command_field_type": type(item.get("command")).__name__,
                        "output_marker": marker(item.get("aggregated_output")),
                        "original_in_output": ORIGINAL in (item.get("aggregated_output") or ""),
                    }
                    for item in command_items
                ],
                "model_visible_result": marker(model_messages[-1]) if model_messages else "absent",
            },
            "stderr_marker": marker(completed.stderr),
        }
        print(json.dumps(report, indent=2))
        return 0 if valid else 1


if __name__ == "__main__":
    raise SystemExit(main())
