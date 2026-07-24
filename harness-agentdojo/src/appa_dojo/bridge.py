"""Typed JSON-lines client for the stateful OpenAPPA sidecar."""

import json
import os
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import TypeAlias

_REPO_ROOT = Path(__file__).resolve().parents[3]
_SIDECAR_PACKAGE = "appa-dojo-sidecar"
_DEFAULT_BINARY = _REPO_ROOT / "target" / "release" / _SIDECAR_PACKAGE

_binary_cache: Path | None = None


class SidecarError(RuntimeError):
    """The sidecar rejected a request or exited unexpectedly."""


@dataclass(frozen=True)
class Allowed:
    pass


@dataclass(frozen=True)
class Blocked:
    feedback: str


CheckDecision: TypeAlias = Allowed | Blocked


@dataclass(frozen=True)
class AuthorizedCall:
    tool: str
    arguments: dict[str, object]


@dataclass(frozen=True)
class Declined:
    feedback: str


RemedyDecision: TypeAlias = AuthorizedCall | Declined


@dataclass(frozen=True)
class Admitted:
    content: str


@dataclass(frozen=True)
class Sealed:
    token: str


ReportedResult: TypeAlias = Admitted | Sealed


def resolve_binary() -> Path:
    """Resolve an override or build the workspace sidecar once."""
    global _binary_cache
    if _binary_cache is not None:
        return _binary_cache

    override = os.environ.get("APPA_DOJO_SIDECAR_BIN")
    if override is not None:
        path = Path(override)
        if not path.is_file():
            raise FileNotFoundError(f"APPA_DOJO_SIDECAR_BIN={override} does not exist")
        _binary_cache = path
        return path

    subprocess.run(
        ["cargo", "build", "--release", "--quiet", "-p", _SIDECAR_PACKAGE],
        cwd=_REPO_ROOT,
        check=True,
    )
    if not _DEFAULT_BINARY.is_file():
        raise FileNotFoundError(f"cargo build succeeded but {_DEFAULT_BINARY} is missing")
    _binary_cache = _DEFAULT_BINARY
    return _DEFAULT_BINARY


class SidecarClient:
    """One long-lived sidecar process, reset to a new APPA session per episode."""

    def __init__(self, binary: Path | None = None) -> None:
        self._process = subprocess.Popen(
            [str(binary or resolve_binary())],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            bufsize=1,
        )
        self._active_episode = False

    def open(self, policy: str, tools: list[str], user_prompt: str) -> None:
        response = self._request(
            {
                "command": "open",
                "policy": policy,
                "tools": tools,
                "user_prompt": user_prompt,
            }
        )
        self._require_status(response, "opened")
        self._active_episode = True

    def check(self, tool: str, arguments: dict[str, object]) -> CheckDecision:
        response = self._request({"command": "check", "tool": tool, "arguments": arguments})
        match response.get("status"):
            case "allowed":
                return Allowed()
            case "blocked":
                return Blocked(feedback=self._string(response, "feedback"))
            case status:
                raise SidecarError(f"unexpected check response status: {status!r}")

    def resolve_remedy(self, plan_id: str | None) -> RemedyDecision:
        response = self._request({"command": "resolve_remedy", "plan_id": plan_id})
        match response.get("status"):
            case "authorized":
                call = response.get("call")
                if not isinstance(call, dict):
                    raise SidecarError("authorized response has no call object")
                tool = call.get("tool")
                arguments = call.get("arguments")
                if not isinstance(tool, str) or not isinstance(arguments, dict):
                    raise SidecarError("authorized response has an invalid call")
                return AuthorizedCall(tool=tool, arguments=arguments)
            case "declined":
                return Declined(feedback=self._string(response, "feedback"))
            case status:
                raise SidecarError(f"unexpected remedy response status: {status!r}")

    def new_round(self) -> None:
        """Signal a new model completion. Informed acceptance requires it: an acceptance-carrying
        remedy executes only in a round after the one that surfaced its offer."""
        response = self._request({"command": "new_round"})
        self._require_status(response, "round_begun")

    def report_success(self, body: str) -> ReportedResult:
        return self._report({"kind": "success", "body": body})

    def report_indeterminate(self) -> ReportedResult:
        return self._report({"kind": "indeterminate"})

    def close(self) -> None:
        if self._process.poll() is not None:
            return
        if self._active_episode:
            self._request({"command": "end"})
            self._active_episode = False
        if self._process.stdin is not None:
            self._process.stdin.close()
        self._process.wait(timeout=5)

    def __enter__(self) -> "SidecarClient":
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        self.close()

    def _report(self, outcome: dict[str, object]) -> ReportedResult:
        response = self._request({"command": "report", "outcome": outcome})
        match response.get("status"):
            case "admitted":
                return Admitted(content=self._string(response, "content"))
            case "sealed":
                return Sealed(token=self._string(response, "token"))
            case status:
                raise SidecarError(f"unexpected report response status: {status!r}")

    def _request(self, request: dict[str, object]) -> dict[str, object]:
        stdin = self._process.stdin
        stdout = self._process.stdout
        if stdin is None or stdout is None:
            raise SidecarError("sidecar pipes are unavailable")
        stdin.write(json.dumps(request, separators=(",", ":")) + "\n")
        stdin.flush()
        line = stdout.readline()
        if line == "":
            code = self._process.poll()
            raise SidecarError(f"sidecar exited unexpectedly with status {code}")
        response = json.loads(line)
        if not isinstance(response, dict):
            raise SidecarError("sidecar response is not an object")
        if response.get("status") == "error":
            raise SidecarError(self._string(response, "message"))
        return response

    @staticmethod
    def _string(response: dict[str, object], key: str) -> str:
        value = response.get(key)
        if not isinstance(value, str):
            raise SidecarError(f"sidecar response field {key!r} is not a string")
        return value

    @staticmethod
    def _require_status(response: dict[str, object], expected: str) -> None:
        status = response.get("status")
        if status != expected:
            raise SidecarError(f"unexpected sidecar response status: expected {expected!r}, got {status!r}")
