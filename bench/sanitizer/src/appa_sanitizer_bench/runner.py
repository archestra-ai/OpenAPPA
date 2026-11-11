"""Bind the bench policy's sanitizers to one backend and ask them through `appa runtime sanitize`.

The runtime does the asking: the same declaration, prompt, answer schema, deadline, byte cap,
retries, and answer reading a session uses. The harness only writes the deployment, feeds
the values, and keeps every answer line.

A run directory is what `publish` archives, so it holds no machine-local path and no raw
corpus value. The values are reproducible from the commit and the corpus digest the run
records, and reach the runtime on standard input only. The archived answers carry every
`drop` span of their case as `REDACTED-DROP-<index>`: the synthetic secrets the corpus plants
would otherwise be refused by the archive's credential scan. Scoring reads the answers before
this masking.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path

from .corpus import POLICY, ROOT, Case, included, mask, sanitizers

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

    builtin: str  # "claude-code", "llm", or "identity" (the no-rewrite stand-in)
    model: str | None = None
    provider: str | None = None
    url: str | None = None
    # The directory of the real `claude` executable, put first on the runtime's PATH. Never
    # written into the deployment or the summary: a run directory holds no local path.
    claude_dir: str | None = None
    # The model table's `timeout_ms`; `None` keeps the runtime's default for the builtin.
    consult_timeout_ms: int | None = None
    max_concurrent: int | None = None
    max_body_bytes: int = SHIPPED_MAX_BODY_BYTES

    def describe(self) -> dict:
        return {key: value for key, value in asdict(self).items() if value is not None and key != "claude_dir"}


def _toml_string(value: str) -> str:
    return json.dumps(value)


IDENTITY_SCRIPT = "identity_sanitizer.py"
# Resolved from PATH and run from the policy's directory, where `ask` writes the script.
IDENTITY_COMMAND = ("python3", "-I", IDENTITY_SCRIPT)
IDENTITY_SANITIZER = """\
import json, sys
consult = json.load(sys.stdin)
print(json.dumps({"version": 1, "answer": {"body": consult["artifact"]["body"]}}))
"""


def deployment(backend: Backend) -> str:
    """The bench policy with `[externals]` binding every sanitizer it composes to `backend`.

    A run that asks only some sanitizers still binds them all, so every run is asked under the
    same composed policy: a battery sanitizer nothing binds is left out of it, with the
    confinements only it covered."""
    lines = [
        POLICY.read_text().rstrip(),
        "",
        "[externals]",
        f"timeout_ms = {SHIPPED_TIMEOUT_MS}",
        f"max_body_bytes = {backend.max_body_bytes}",
    ]
    for name in sanitizers():
        lines += ["", f"[externals.sanitizers.{_toml_string(name)}]"]
        if backend.builtin == "identity":
            lines.append(f"command = [{', '.join(_toml_string(part) for part in IDENTITY_COMMAND)}]")
        else:
            lines.append(f"builtin = {_toml_string(backend.builtin)}")
    limits = []
    if backend.consult_timeout_ms is not None:
        limits.append(f"timeout_ms = {backend.consult_timeout_ms}")
    if backend.max_concurrent is not None:
        limits.append(f"max_concurrent = {backend.max_concurrent}")
    if backend.builtin == "claude-code":
        lines += ["", "[externals.claude_code]"]
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
    scratch directory, so the runtime is given the directory of the executable it would run."""
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
    """Every case asked `repeat` times; the raw answer rows, unmasked.

    Writes `policy.toml` and the shipped batteries it includes, `values.sha256` (the digest of
    the values fed on standard input), and the masked `answers.jsonl` into `output`.
    """
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    if backend.builtin == "identity":
        (output / IDENTITY_SCRIPT).write_text(IDENTITY_SANITIZER)
    for include, shipped in included().items():
        (output / include).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(shipped, output / include)
    (output / "policy.toml").write_text(deployment(backend))
    values = "".join(json.dumps(case.value()) + "\n" for case in cases)
    (output / "values.sha256").write_text(hashlib.sha256(values.encode()).hexdigest() + "\n")
    # The includes resolve in the run directory only: a batteries directory named in the
    # environment would be searched first.
    env = {key: value for key, value in os.environ.items() if key not in (TOKEN_VARIABLE, "APPA_BATTERIES_DIR")}
    if token is not None:
        env[TOKEN_VARIABLE] = token
    if backend.claude_dir is not None:
        env["PATH"] = os.pathsep.join([backend.claude_dir, env.get("PATH", "")])
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
        capture_output=True,
        text=True,
        env=env,
        cwd=output,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"appa runtime sanitize failed: {completed.stderr.strip()}")
    rows = [json.loads(line) for line in completed.stdout.splitlines()]
    drops = {case.id: case.drop for case in cases}
    with (output / "answers.jsonl").open("w") as answers:
        for row in rows:
            answers.write(json.dumps(mask(row, drops.get(row.get("id"), ()))) + "\n")
    return rows
