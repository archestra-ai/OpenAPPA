"""Bind the bench policy's sanitizers to one backend and ask them through `appa runtime sanitize`.

The runtime does the asking: the same declaration, prompt, answer schema, deadline, byte cap,
retries, and answer reading a session uses. The harness only writes the deployment, feeds
the values, and keeps every answer line.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass
from pathlib import Path

from .corpus import POLICY, ROOT, Case

REPO_ROOT = ROOT.parents[1]
APPA_BIN = REPO_ROOT / "target" / "debug" / "appa"
# The one variable the runtime reads the `llm` key from; set only in the child's environment.
TOKEN_VARIABLE = "APPA_SANITIZER_BENCH_TOKEN"
# What the shipped Claude Code profile (`marketplace/plugins/claude-code/default.appa.toml`)
# declares for every external. Its `timeout_ms` bounds HTTP and command externals; a
# `claude-code` or `llm` consult runs under its own table's `timeout_ms`, 60 s when unset.
SHIPPED_TIMEOUT_MS = 5000
SHIPPED_MAX_BODY_BYTES = 65536


@dataclass(frozen=True)
class Backend:
    """Who answers every sanitizer of the bench policy, under which limits."""

    builtin: str  # "claude-code", "llm", or "identity" (preflight's stand-in)
    model: str | None = None
    provider: str | None = None
    url: str | None = None
    claude_command: str | None = None
    # The model table's `timeout_ms`; `None` keeps the runtime's default for the builtin.
    consult_timeout_ms: int | None = None
    max_concurrent: int | None = None
    max_body_bytes: int = SHIPPED_MAX_BODY_BYTES

    def describe(self) -> dict:
        return {key: value for key, value in asdict(self).items() if value is not None}


def _toml_string(value: str) -> str:
    return json.dumps(value)


def deployment(backend: Backend, sanitizers: list[str], identity_command: list[str] | None = None) -> str:
    """The bench policy with `[externals]` binding every sanitizer to `backend`."""
    lines = [
        POLICY.read_text().rstrip(),
        "",
        "[externals]",
        f"timeout_ms = {SHIPPED_TIMEOUT_MS}",
        f"max_body_bytes = {backend.max_body_bytes}",
    ]
    for name in sanitizers:
        lines += ["", f"[externals.sanitizers.{name}]"]
        if backend.builtin == "identity":
            assert identity_command is not None, "the identity backend runs a command"
            lines.append(f"command = [{', '.join(_toml_string(part) for part in identity_command)}]")
        else:
            lines.append(f"builtin = {_toml_string(backend.builtin)}")
    limits = []
    if backend.consult_timeout_ms is not None:
        limits.append(f"timeout_ms = {backend.consult_timeout_ms}")
    if backend.max_concurrent is not None:
        limits.append(f"max_concurrent = {backend.max_concurrent}")
    if backend.builtin == "claude-code":
        lines += ["", "[externals.claude_code]"]
        if backend.claude_command is not None:
            lines.append(f"command = {_toml_string(backend.claude_command)}")
        if backend.model is not None:
            lines.append(f"model = {_toml_string(backend.model)}")
        lines += limits
    elif backend.builtin == "llm":
        lines += [
            "",
            "[externals.llm]",
            f"provider = {_toml_string(backend.provider or 'anthropic')}",
            f"model = {_toml_string(backend.model or '')}",
            f"token_env = {_toml_string(TOKEN_VARIABLE)}",
        ]
        if backend.url is not None:
            lines.append(f"url = {_toml_string(backend.url)}")
        lines += limits
    return "\n".join(lines) + "\n"


def resolve_claude_command() -> str | None:
    """The real `claude` executable. A version-manager shim may not run from the consult's
    scratch directory, so the binding names the executable the shim would run."""
    try:
        found = subprocess.run(
            ["mise", "which", "claude"], cwd=REPO_ROOT, capture_output=True, text=True, check=True
        ).stdout.strip()
        if found:
            return found
    except (OSError, subprocess.CalledProcessError):
        pass
    return shutil.which("claude")


def build_appa() -> None:
    subprocess.run(["cargo", "build", "-p", "appa"], cwd=REPO_ROOT, check=True)


IDENTITY_SANITIZER = """\
import json, sys
consult = json.load(sys.stdin)
print(json.dumps({"version": 1, "answer": {"body": consult["artifact"]["body"]}}))
"""


def identity_command(directory: Path) -> list[str]:
    """A stand-in sanitizer that returns every value unchanged: the no-sanitizer baseline."""
    script = directory / "identity_sanitizer.py"
    script.write_text(IDENTITY_SANITIZER)
    return [sys.executable, "-I", str(script)]


def ask(
    cases: list[Case],
    backend: Backend,
    output: Path,
    *,
    repeat: int,
    concurrency: int,
    token: str | None = None,
    appa_bin: Path = APPA_BIN,
) -> list[dict]:
    """Every case asked `repeat` times. Writes `policy.toml`, `values.jsonl`, `answers.jsonl`."""
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    names = sorted({case.sanitizer for case in cases})
    command = identity_command(output) if backend.builtin == "identity" else None
    (output / "policy.toml").write_text(deployment(backend, names, command))
    values = "".join(json.dumps(case.value()) + "\n" for case in cases)
    (output / "values.jsonl").write_text(values)
    env = {key: value for key, value in os.environ.items() if key != TOKEN_VARIABLE}
    if token is not None:
        env[TOKEN_VARIABLE] = token
    answers_path = output / "answers.jsonl"
    with answers_path.open("w") as answers:
        completed = subprocess.run(
            [
                str(appa_bin),
                "runtime",
                "sanitize",
                "--config",
                str(output / "policy.toml"),
                "--repeat",
                str(repeat),
                "--concurrency",
                str(concurrency),
            ],
            input=values,
            stdout=answers,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
            cwd=output,
        )
    if completed.returncode != 0:
        raise RuntimeError(f"appa runtime sanitize failed: {completed.stderr.strip()}")
    return [json.loads(line) for line in answers_path.read_text().splitlines()]
