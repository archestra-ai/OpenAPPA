#!/usr/bin/python3
"""Run one staged command in an agentsh sandbox inside a bwrap namespace."""

import json
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ENV = {"PATH": "/usr/bin:/bin", "HOME": "/job/work", "LANG": "C.UTF-8"}
BWRAP = "/usr/bin/bwrap"
PYTHON = "/usr/bin/python3"
STARTUP_TIMEOUT = 10.0
EXEC_TIMEOUT = 120.0
OUTER_TIMEOUT = 140.0

BLOCKED_SYSCALLS = [
    "socket",
    "socketpair",
    "connect",
    "keyctl",
    "add_key",
    "request_key",
    "ptrace",
    "process_vm_readv",
    "process_vm_writev",
    "pidfd_getfd",
    "kill",
    "tkill",
    "tgkill",
    "pidfd_send_signal",
    "mount",
    "umount2",
    "pivot_root",
    "chroot",
    "setns",
    "unshare",
    "semget",
    "semop",
    "semtimedop",
    "semctl",
    "msgget",
    "msgsnd",
    "msgrcv",
    "msgctl",
    "shmget",
    "shmat",
    "shmdt",
    "shmctl",
    "bpf",
    "perf_event_open",
    "io_uring_setup",
]


class RunnerError(Exception):
    pass


def _regular_file(path: Path) -> bool:
    try:
        return stat.S_ISREG(path.lstat().st_mode)
    except OSError:
        return False


def validate_job(job: Path) -> str:
    if not job.is_absolute() or not job.is_dir():
        raise RunnerError("invalid job directory")
    inputs, output, request = job / "inputs", job / "output", job / "request.json"
    if not inputs.is_dir() or not output.is_dir() or not _regular_file(request):
        raise RunnerError("invalid job layout")
    if any(output.iterdir()):
        raise RunnerError("output directory is not empty")
    for entry in inputs.rglob("*"):
        if entry.is_symlink() or (not entry.is_dir() and not _regular_file(entry)):
            raise RunnerError("inputs contain a non-regular file")
    try:
        value = json.loads(request.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise RunnerError("invalid request") from exc
    if (
        not isinstance(value, dict)
        or set(value) != {"command"}
        or not isinstance(value["command"], str)
    ):
        raise RunnerError("invalid request")
    return value["command"]


def bwrap_command(backend: Path, job: Path, runner: Path) -> list[str]:
    if not backend.is_absolute() or not job.is_absolute() or not runner.is_absolute():
        raise RunnerError("sandbox paths must be absolute")
    args = [
        BWRAP,
        "--unshare-all",
        "--as-pid-1",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--new-session",
        "--clearenv",
    ]
    for path in ("/usr", "/bin", "/lib", "/lib64"):
        args += ["--ro-bind", path, path]
    args += [
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--ro-bind",
        str(backend),
        "/appa",
        "--ro-bind",
        str(runner),
        "/runner/run.py",
        "--bind",
        str(job),
        "/job",
        "--tmpfs",
        "/job/work",
        "--ro-bind",
        str(job / "inputs"),
        "/job/work/inputs",
        "--bind",
        str(job / "output"),
        "/job/work/output",
        "--setenv",
        "PATH",
        ENV["PATH"],
        "--setenv",
        "HOME",
        ENV["HOME"],
        "--setenv",
        "LANG",
        ENV["LANG"],
        "--chdir",
        "/job/work",
        PYTHON,
        "/runner/run.py",
        "--inner",
    ]
    return args


def config_document() -> dict:
    tools = ["/usr", "/bin", "/lib", "/lib64"]
    return {
        "server": {
            "http": {"addr": "127.0.0.1:18080"},
            "grpc": {"enabled": False},
            "unix_socket": {"enabled": False},
        },
        "auth": {"type": "none"},
        "logging": {
            "level": "error",
            "format": "json",
            "output": "/job/control/server.log",
        },
        "audit": {
            "enabled": True,
            "output": "/job/control/audit.jsonl",
            "storage": {"sqlite_path": "/job/control/events.db"},
        },
        "sessions": {"base_dir": "/job/control/sessions"},
        "policies": {
            "dir": "/job/control/policies",
            "default": "appa",
            "detect_project_root": False,
        },
        "security": {
            "strict": True,
            "mode": "landlock-only",
            "minimum_mode": "landlock-only",
        },
        "sandbox": {
            "enabled": True,
            "allow_degraded": False,
            "fuse": {"enabled": False},
            "network": {"enabled": False},
            "cgroups": {"enabled": False},
            "unix_sockets": {"enabled": True, "wrapper_bin": "/appa/agentsh-unixwrap"},
            "seccomp": {
                "enabled": True,
                "mode": "enforce",
                "wait_killable": False,
                "unix_socket": {"enabled": True, "action": "enforce"},
                "file_monitor": {
                    "enabled": True,
                    "enforce_without_fuse": True,
                    "openat_emulation": True,
                    "intercept_metadata": True,
                    "write_only_opens": False,
                    "block_io_uring": True,
                },
                "syscalls": {
                    "default_action": "allow",
                    "block": BLOCKED_SYSCALLS,
                    "on_block": "errno",
                },
            },
        },
        "landlock": {
            "enabled": True,
            "allow_execute": tools,
            "allow_read": tools,
            "allow_write": [],
            "network": {"allow_connect_tcp": False, "allow_bind_tcp": False},
        },
        "proxy": {"mode": "disabled"},
        "health": {"path": "/health", "readiness_path": "/ready"},
    }


def policy_document() -> dict:
    read_ops = ["read", "open", "stat", "list", "readlink", "access"]
    write_ops = [
        "write",
        "create",
        "mkdir",
        "chmod",
        "rename",
        "delete",
        "rmdir",
        "link",
        "symlink",
    ]
    return {
        "version": 1,
        "name": "appa",
        "file_rules": [
            {
                "name": "toolchain-read",
                "paths": ["/usr/**", "/bin/**", "/lib/**", "/lib64/**"],
                "operations": read_ops,
                "decision": "allow",
            },
            {
                "name": "dev-null",
                "paths": ["/dev/null"],
                "operations": read_ops + ["write"],
                "decision": "allow",
            },
            # Directory-node inspection must not derive a Landlock grant for its parent /job.
            {
                "name": "directory-nodes",
                "paths": ["/job/work", "/usr", "/bin", "/lib", "/lib64"],
                "operations": ["open", "stat", "list", "access", "readlink"],
                "decision": "allow",
            },
            {
                "name": "workspace",
                "paths": ["/job/work/**"],
                "operations": read_ops + write_ops,
                "decision": "allow",
            },
            {
                "name": "deny-other-files",
                "paths": ["**"],
                "operations": ["*"],
                "decision": "deny",
            },
        ],
        "network_rules": [
            {"name": "deny-network", "domains": ["*"], "decision": "deny"}
        ],
        "command_rules": [
            {"name": "allow-shell", "commands": ["sh"], "decision": "allow"}
        ],
    }


def _http_json(method: str, path: str, body=None, timeout=2.0):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        "http://127.0.0.1:18080" + path,
        data=data,
        method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        if path == "/ready":
            return response.read().strip() == b"ready"
        return json.loads(response.read())


def _stop_daemon(daemon) -> None:
    if daemon.poll() is None:
        daemon.terminate()
        try:
            daemon.wait(timeout=3)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait()


def inner() -> int:
    command = validate_job(Path("/job"))
    control = Path("/job/control")
    (control / "policies").mkdir(parents=True, exist_ok=False)
    (control / "config.json").write_text(json.dumps(config_document()))
    (control / "policies/appa.yaml").write_text(json.dumps(policy_document()))
    with open(control / "launcher.log", "wb") as log:
        daemon = subprocess.Popen(
            ["/appa/agentsh", "server", "--config", "/job/control/config.json"],
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            env=ENV,
            close_fds=True,
        )
    try:
        deadline = time.monotonic() + STARTUP_TIMEOUT
        while True:
            if daemon.poll() is not None:
                raise RunnerError("agentsh failed to start")
            try:
                _http_json("GET", "/ready", timeout=0.25)
                break
            except (OSError, ValueError, urllib.error.URLError):
                if time.monotonic() >= deadline:
                    raise RunnerError("agentsh startup timed out")
                time.sleep(0.05)
        session = _http_json(
            "POST",
            "/api/v1/sessions",
            {
                "workspace": "/job/work",
                "policy": "appa",
                "real_paths": True,
                "detect_project_root": False,
            },
            timeout=2,
        )
        if not isinstance(session, dict) or not isinstance(session.get("id"), str):
            raise RunnerError("malformed session response")
        result = _http_json(
            "POST",
            "/api/v1/sessions/{}/exec".format(session["id"]),
            {
                "command": "/bin/sh",
                "args": ["-c", command],
                "working_dir": "/job/work",
                "timeout": "120s",
                "include_events": "none",
                "env": ENV,
            },
            timeout=EXEC_TIMEOUT,
        )
        if (
            not isinstance(result, dict)
            or not isinstance(result.get("result"), dict)
            or not isinstance(result["result"].get("exit_code"), int)
        ):
            raise RunnerError("malformed exec response")
        sys.stdout.write(
            json.dumps({"result": result["result"]}, separators=(",", ":")) + "\n"
        )
        return 0
    finally:
        _stop_daemon(daemon)


def outer(backend: Path, job: Path) -> int:
    validate_job(job)
    if (
        not backend.is_absolute()
        or not _regular_file(backend / "agentsh")
        or not _regular_file(backend / "agentsh-unixwrap")
    ):
        raise RunnerError("invalid backend")
    cmd = bwrap_command(backend, job, Path(__file__).resolve())
    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=ENV,
        close_fds=True,
        text=True,
    )
    try:
        stdout, _stderr = proc.communicate(timeout=OUTER_TIMEOUT)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        raise RunnerError("sandbox timed out")
    if proc.returncode != 0:
        raise RunnerError("sandbox failed")
    try:
        value = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise RunnerError("sandbox returned malformed output") from exc
    if (
        not isinstance(value, dict)
        or not isinstance(value.get("result"), dict)
        or not isinstance(value["result"].get("exit_code"), int)
    ):
        raise RunnerError("sandbox returned malformed output")
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    return 0


def main() -> int:
    try:
        if sys.argv == [sys.argv[0], "--inner"]:
            return inner()
        if len(sys.argv) != 3:
            raise RunnerError("usage: run.py BACKEND_DIR JOB_DIR")
        return outer(
            Path(sys.argv[1]).resolve(strict=True),
            Path(sys.argv[2]).resolve(strict=True),
        )
    except (
        RunnerError,
        OSError,
        ValueError,
        TypeError,
        subprocess.SubprocessError,
        urllib.error.URLError,
    ):
        print("agentsh runner failed", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
