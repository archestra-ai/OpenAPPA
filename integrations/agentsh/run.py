#!/usr/bin/python3
"""Run one staged command in an agentsh sandbox inside a bwrap namespace."""

import json
import resource
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

# Resource ceilings for the command and its descendants. The namespace is not a resource
# boundary on its own, so each limit is set on the shell that execs the command and inherited
# from there: address space, one file's size, open descriptors, CPU time, core dumps. A shell
# that cannot set one refuses to run the command rather than running it unbounded.
#
# The process count is not here: `sh` is dash on Debian, and dash's `ulimit` has no option for
# it. It is set on the daemon instead (see `apply_process_ceiling`), which the command inherits.
LIMITS = {
    "core": 0,
    "fsize": 65536,
    "data": 2097152,
    "nofile": 256,
    "cpu": 150,
}
LIMIT_SCRIPT = (
    "ulimit -c {core} || exit 125; "
    "ulimit -f {fsize} || exit 125; "
    "ulimit -v {data} || exit 125; "
    "ulimit -n {nofile} || exit 125; "
    "ulimit -t {cpu} || exit 125; "
    'exec /bin/sh -c "$1"'
).format(**LIMITS)

# The process ceiling, applied to the daemon: RLIMIT_NPROC counts threads as well as processes,
# and the daemon is a Go server that spawns the command, so this bounds a fork bomb without
# starving the launcher.
MAX_PROCESSES = 256

# Sizes of the memory-backed mounts the command may write: scratch, and its working directory.
# A tmpfs without a size is host RAM, which the namespace does not bound either. bubblewrap's
# `--size` takes a plain byte count, never a suffixed size.
TMP_SIZE = str(64 << 20)
WORK_SIZE = str(256 << 20)

# How much of a stream the runner echoes back to the runtime. The runtime parses one JSON
# document out of the launcher's stdout, so an unbounded stream is unbounded runtime memory.
MAX_STREAM_BYTES = 1 << 20
TRUNCATION_MARK = "\n[truncated by the APPA runner]\n"

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
        "--size",
        TMP_SIZE,
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
        "--size",
        WORK_SIZE,
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


def bounded(value: dict) -> dict:
    """Cap the diagnostic streams the runtime will parse out of this process's stdout.

    A command may print without bound; the runtime reads one JSON document and holds it in
    memory. Truncation keeps the exit code and the head of each stream, and says so.
    """
    result = value.get("result")
    if isinstance(result, dict):
        for key in ("stdout", "stderr"):
            text = result.get(key)
            if isinstance(text, str) and len(text.encode("utf-8", "surrogatepass")) > MAX_STREAM_BYTES:
                cut = text.encode("utf-8", "surrogatepass")[:MAX_STREAM_BYTES]
                result[key] = cut.decode("utf-8", "replace") + TRUNCATION_MARK
    return value


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


REQUIRED_CONFIG = {
    ("security", "strict"): True,
    ("security", "mode"): "landlock-only",
    ("security", "minimum_mode"): "landlock-only",
    ("sandbox", "enabled"): True,
    ("sandbox", "allow_degraded"): False,
    ("sandbox", "network", "enabled"): False,
    ("sandbox", "unix_sockets", "enabled"): True,
    ("sandbox", "seccomp", "enabled"): True,
    ("sandbox", "seccomp", "mode"): "enforce",
    ("sandbox", "seccomp", "unix_socket", "enabled"): True,
    ("sandbox", "seccomp", "file_monitor", "enabled"): True,
    ("landlock", "enabled"): True,
}


def assert_fail_closed(document: dict) -> None:
    """Refuse a configuration whose patched fail-closed paths would run the command unwrapped.

    The patch keeps upstream's early returns for a disabled unix-socket wrapper, for a full
    ptrace tracer, and for non-Linux hosts. None of them is reachable from the document this
    runner writes, and that is the point: a later edit that weakens one is a refusal here,
    never a silent unconfined run.
    """
    for path, expected in REQUIRED_CONFIG.items():
        node = document
        for key in path:
            if not isinstance(node, dict) or key not in node:
                raise RunnerError("configuration is missing " + ".".join(path))
            node = node[key]
        if node != expected:
            raise RunnerError("configuration weakens " + ".".join(path) + f": {node!r}")
    ptrace = document.get("sandbox", {}).get("ptrace", {})
    if isinstance(ptrace, dict) and ptrace.get("enabled") and not ptrace.get("execve_only"):
        raise RunnerError("configuration enables full ptrace tracing")


def process_ceiling(soft: int, hard: int) -> int:
    """The lowest of the requested process ceiling and whatever the host already allows."""
    wanted = [value for value in (MAX_PROCESSES, soft, hard) if value != resource.RLIM_INFINITY]
    return min(wanted)


def apply_process_ceiling() -> None:
    """Set the one ceiling `sh` cannot set, on this process and everything it spawns.

    dash's `ulimit` has no option for the process count, so it is applied here instead: the
    agentsh daemon is started after this, and the command it spawns inherits the limit.
    """
    soft, hard = resource.getrlimit(resource.RLIMIT_NPROC)
    resource.setrlimit(resource.RLIMIT_NPROC, (process_ceiling(soft, hard), hard))


def inner() -> int:
    command = validate_job(Path("/job"))
    control = Path("/job/control")
    (control / "policies").mkdir(parents=True, exist_ok=False)
    document = config_document()
    assert_fail_closed(document)
    (control / "config.json").write_text(json.dumps(document))
    (control / "policies/appa.yaml").write_text(json.dumps(policy_document()))
    apply_process_ceiling()
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
                "args": ["-c", LIMIT_SCRIPT, "appa-command", command],
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
            json.dumps(bounded({"result": result["result"]}), separators=(",", ":")) + "\n"
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
        stdout, stderr = proc.communicate(timeout=OUTER_TIMEOUT)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
        raise RunnerError("sandbox timed out")
    if proc.returncode != 0:
        # The launcher's own stderr, which carries the inner process's message too: what the
        # operator needs to see when a sandbox cannot start. This process's own generic line
        # is dropped, or it would mask that message. The command's output is never here: it
        # travels in the JSON result.
        lines = [
            line.strip()
            for line in (stderr or "").splitlines()
            if line.strip() and "agentsh runner failed" not in line
        ]
        detail = lines[-1] if lines else ""
        raise RunnerError("sandbox failed: " + detail[:400])
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
    ) as error:
        # The reason, not just the verdict: a launcher that cannot start is an operator's
        # problem, and "it failed" leaves them with nothing to act on.
        print(f"agentsh runner failed: {error}"[:400], file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
