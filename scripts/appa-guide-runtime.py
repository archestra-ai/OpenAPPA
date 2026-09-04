#!/usr/bin/env python3
"""Fixed runtime operations for appa-guide on kagent."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.request import Request, urlopen


RUNTIME_URL = os.environ.get("APPA_GUIDE_RUNTIME_URL", "http://127.0.0.1:8787").rstrip("/")
REFRESH = "/usr/local/bin/appa-refresh-batteries"
CANDIDATE = Path(os.environ.get("APPA_GUIDE_REFRESH_CANDIDATE", "/var/lib/appa/.appa-guide-refresh-candidate"))
STABLE_TAG = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
MUTATING_COMMANDS = {
    "appa-guide-reload",
    "appa-guide-refresh-check",
    "appa-guide-refresh-stage",
    "appa-guide-refresh-commit",
    "appa-guide-refresh-rollback",
}


def runtime_request(path: str, method: str = "GET") -> bytes:
    request = Request(RUNTIME_URL + path, data=b"" if method == "POST" else None, method=method)
    with urlopen(request, timeout=30) as response:
        return response.read()


def print_section(name: str, content: bytes) -> None:
    print(f"--- {name} ---")
    sys.stdout.flush()
    sys.stdout.buffer.write(content)
    if not content.endswith(b"\n"):
        print()


def optional_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8").strip() or None
    except FileNotFoundError:
        return None


def refresh_state() -> dict:
    target = Path(os.environ.get("APPA_BATTERY_REFRESH_TARGET", "/var/lib/appa/release-batteries"))
    previous = target.with_name(f".{target.name}.previous")
    return {
        "supported": Path(REFRESH).is_file(),
        "release": optional_text(target / ".appa-release"),
        "candidate": optional_text(CANDIDATE),
        "pending_previous_layer": previous.is_dir() and not previous.is_symlink(),
    }


def inspect() -> None:
    config = os.environ.get("APPA_CONFIG")
    if not config:
        raise RuntimeError("APPA_CONFIG is not set")
    print_section("root config", Path(config).read_bytes())
    described = subprocess.run(
        ["/usr/local/bin/appa", "describe", "--adapter", "kagent"],
        check=True,
        capture_output=True,
    )
    print_section("description", described.stdout)
    batteries = runtime_request("/batteries")
    parsed = json.loads(batteries)
    print_section("batteries", (json.dumps(parsed, sort_keys=True) + "\n").encode())
    print_section("battery refresh state", (json.dumps(refresh_state(), sort_keys=True) + "\n").encode())


def annotate_command(request: dict) -> dict:
    if request.get("version") != 1 or request.get("kind") != "annotation":
        raise ValueError("expected an annotation consult at version 1")
    if request.get("name") != "appa-guide-command":
        raise ValueError("unexpected annotator name")
    artifact = request.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    command = arguments.get("command") if isinstance(arguments, dict) else None
    pod_name = arguments.get("pod_name") if isinstance(arguments, dict) else None
    namespace = arguments.get("namespace") if isinstance(arguments, dict) else None
    expected_pod = os.environ.get("APPA_GUIDE_POD_NAME")
    expected_namespace = os.environ.get("APPA_GUIDE_POD_NAMESPACE")
    if not expected_pod or not expected_namespace:
        raise ValueError("runtime pod identity is not configured")
    if pod_name != expected_pod or namespace != expected_namespace:
        raise ValueError("command target is not this appa-runtime pod")
    if command == "appa-guide-inspect":
        attention = []
    elif command in MUTATING_COMMANDS:
        attention = ["human-approval"]
    else:
        raise ValueError("command execution is not an appa-guide runtime operation")
    return {
        "version": 1,
        "answer": {
            "delta": {},
            "requires": {"trust": "trusted", "history": [], "attention": attention},
            "emits": [],
        },
    }


def refresh_check() -> None:
    checked = subprocess.run([REFRESH, "--check"], check=True, capture_output=True, text=True)
    tag = checked.stdout.strip()
    if not STABLE_TAG.fullmatch(tag):
        raise RuntimeError(f"battery refresh returned an invalid release tag: {tag!r}")
    CANDIDATE.parent.mkdir(parents=True, exist_ok=True)
    temporary = CANDIDATE.with_suffix(".new")
    temporary.write_text(tag + "\n", encoding="utf-8")
    temporary.chmod(0o600)
    temporary.replace(CANDIDATE)
    print(tag)


def refresh_stage() -> None:
    try:
        tag = CANDIDATE.read_text(encoding="utf-8").strip()
    except FileNotFoundError as error:
        raise RuntimeError("run appa-guide-refresh-check before staging") from error
    if not STABLE_TAG.fullmatch(tag):
        raise RuntimeError("the recorded battery release tag is invalid")
    subprocess.run([REFRESH, "--tag", tag], check=True)
    CANDIDATE.unlink()


def main() -> int:
    action = Path(sys.argv[0]).name
    try:
        if action == "appa-guide-command-annotator":
            json.dump(annotate_command(json.load(sys.stdin)), sys.stdout)
            print()
        elif action == "appa-guide-inspect":
            inspect()
        elif action == "appa-guide-reload":
            print_section("reload", runtime_request("/reload", "POST"))
        elif action == "appa-guide-refresh-check":
            refresh_check()
        elif action == "appa-guide-refresh-stage":
            refresh_stage()
        elif action == "appa-guide-refresh-commit":
            subprocess.run([REFRESH, "--commit"], check=True)
        elif action == "appa-guide-refresh-rollback":
            subprocess.run([REFRESH, "--rollback"], check=True)
        else:
            raise RuntimeError(f"unsupported appa-guide runtime operation: {action}")
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError, json.JSONDecodeError) as error:
        print(f"{action}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
