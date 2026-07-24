"""The systems under test: the two demo agents, each guarded and open.

Four fixed configurations, all driven through the demos' existing CLIs — the
bench adds no flags to either demo. One shared model (``--model``) keeps the
comparison defense-vs-defense.
"""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
CORP_SYSTEMS_DIR = REPO_ROOT / "demo" / "corp-systems"
CORP_AGENT_DIR = REPO_ROOT / "demo" / "corporate-agent"
FIDES_DIR = REPO_ROOT / "demo" / "corporate-agent-fides"

CORP_SYSTEMS_BIN = CORP_SYSTEMS_DIR / "target" / "debug" / "corp-systems-mcp"
CORP_AGENT_BIN = CORP_AGENT_DIR / "target" / "debug" / "corp-agent"
FIDES_BIN = FIDES_DIR / ".venv" / "bin" / "corp-agent-fides"

DEFAULT_MODEL = "openai/gpt-5.6-luna"


@dataclass(frozen=True)
class Sut:
    name: str
    executable: Path
    # Set only for APPA SUTs: the demo policy the runner prunes per episode.
    policy_file: Path | None = None
    extra_args: tuple[str, ...] = ()


SUTS: dict[str, Sut] = {
    "appa": Sut(name="appa", executable=CORP_AGENT_BIN, policy_file=CORP_AGENT_DIR / "appa-policy.toml"),
    "appa-open": Sut(
        name="appa-open", executable=CORP_AGENT_BIN, policy_file=CORP_AGENT_DIR / "appa-policy-open.toml"
    ),
    "fides": Sut(name="fides", executable=FIDES_BIN),
    "fides-open": Sut(name="fides-open", executable=FIDES_BIN, extra_args=("--no-defense",)),
}


def build_binaries(suts: list[Sut]) -> None:
    """Build the Rust binaries the selected SUTs spawn (idempotent, up front —
    never mid-episode, where a cargo build would distort durations)."""
    # The cheap precondition first: a missing FIDES venv must fail in
    # milliseconds, not after minutes of cargo builds.
    if any(sut.executable == FIDES_BIN for sut in suts) and not FIDES_BIN.is_file():
        sys.exit(
            f"missing {FIDES_BIN}\n"
            "The FIDES demo's virtualenv provides the corp-agent-fides entry point.\n"
            f"Create it once:  cd {FIDES_DIR} && uv venv && uv pip install -e ."
        )
    crates = [CORP_SYSTEMS_DIR]  # every SUT spawns the MCP server
    if any(sut.executable == CORP_AGENT_BIN for sut in suts):
        crates.append(CORP_AGENT_DIR)
    # Independent crates, separate target dirs: build concurrently. Pinning
    # CARGO_TARGET_DIR keeps the output at the exact path the bench spawns
    # even when the caller's shell redirects it globally.
    builds = [
        subprocess.Popen(
            ["cargo", "build", "--manifest-path", str(crate / "Cargo.toml")],
            env={**os.environ, "CARGO_TARGET_DIR": str(crate / "target")},
        )
        for crate in crates
    ]
    for build in builds:
        if build.wait() != 0:
            sys.exit("cargo build failed (see output above)")


def command_for(
    sut: Sut,
    *,
    prompt: str,
    model: str,
    episode_dir: Path,
) -> list[str]:
    """The subprocess argv for one episode. The episode dir already holds
    ``data/``, ``sink/``, and (for APPA SUTs) the pruned ``policy.toml``."""
    # No --quiet: stderr.txt is the episode's full mediation/audit log — the
    # diagnostics (blocked-call counts) and any post-hoc reading depend on it.
    command = [
        str(sut.executable),
        prompt,
        "--model",
        model,
        "--data-root",
        str(episode_dir / "data"),
        "--sink-root",
        str(episode_dir / "sink"),
        "--server-bin",
        str(CORP_SYSTEMS_BIN),
    ]
    if sut.policy_file is not None:
        command += ["--policy", str(episode_dir / "policy.toml")]
    return [*command, *sut.extra_args]
