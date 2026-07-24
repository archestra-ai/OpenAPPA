"""The benchmarked agents: the demo agents under fixed defense settings.

Fixed configurations, all driven through the demos' existing CLIs — the bench
adds no flags of its own. One shared model (``--model``) keeps the comparison
defense-vs-defense: the appa agent guarded, branching-disabled (the ablation),
and open, plus FIDES with and without its defense.
"""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
POLICIES_DIR = Path(__file__).resolve().parents[2] / "policies"
CORP_SYSTEMS_DIR = REPO_ROOT / "demo" / "corp-systems"
CORP_AGENT_DIR = REPO_ROOT / "demo" / "corporate-agent"
FIDES_DIR = REPO_ROOT / "demo" / "corporate-agent-fides"

CORP_SYSTEMS_BIN = CORP_SYSTEMS_DIR / "target" / "debug" / "corp-systems-mcp"
APPA_CORP_AGENT_BIN = CORP_AGENT_DIR / "target" / "debug" / "appa-corp-agent"
FIDES_BIN = FIDES_DIR / ".venv" / "bin" / "corp-agent-fides"

DEFAULT_MODEL = "openai/gpt-5.6-luna"


@dataclass(frozen=True)
class Agent:
    name: str
    executable: Path
    # Set only for APPA agents: the benchmark policy the runner prunes per episode.
    policy_file: Path | None = None
    # Set only for agents that spawn the MCP server (the appa agent runs the
    # corp systems in-process and takes no --server-bin).
    mcp_server: Path | None = None
    extra_args: tuple[str, ...] = ()


AGENTS: dict[str, Agent] = {
    # The appa agent is appa-corp-agent: the full appa-agent loop with the
    # reserved fork/submit_result tools live. The ablation arm proves
    # branching is what the fork scenarios pay for, and the open arm is the
    # undefended baseline on the same loop.
    "appa": Agent(name="appa", executable=APPA_CORP_AGENT_BIN, policy_file=POLICIES_DIR / "appa.toml"),
    "appa-nofork": Agent(
        name="appa-nofork",
        executable=APPA_CORP_AGENT_BIN,
        policy_file=POLICIES_DIR / "appa.toml",
        extra_args=("--max-forks", "0"),
    ),
    "appa-open": Agent(name="appa-open", executable=APPA_CORP_AGENT_BIN, policy_file=POLICIES_DIR / "open.toml"),
    "fides": Agent(name="fides", executable=FIDES_BIN, mcp_server=CORP_SYSTEMS_BIN),
    "fides-open": Agent(name="fides-open", executable=FIDES_BIN, mcp_server=CORP_SYSTEMS_BIN, extra_args=("--no-defense",)),
}


def build_binaries(agents: list[Agent]) -> None:
    """Build the Rust binaries the selected agents spawn (idempotent, up front —
    never mid-episode, where a cargo build would distort durations)."""
    # The cheap precondition first: a missing FIDES venv must fail in
    # milliseconds, not after minutes of cargo builds.
    if any(agent.executable == FIDES_BIN for agent in agents) and not FIDES_BIN.is_file():
        sys.exit(
            f"missing {FIDES_BIN}\n"
            "The FIDES demo's virtualenv provides the corp-agent-fides entry point.\n"
            f"Create it once:  cd {FIDES_DIR} && uv venv && uv pip install -e ."
        )
    crates = []
    if any(agent.mcp_server is not None for agent in agents):
        crates.append(CORP_SYSTEMS_DIR)
    if any(agent.executable == APPA_CORP_AGENT_BIN for agent in agents):
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
    agent: Agent,
    *,
    prompt: str,
    model: str,
    episode_dir: Path,
) -> list[str]:
    """The subprocess argv for one episode. The episode dir already holds
    ``data/``, ``sink/``, and (for APPA agents) the pruned ``policy.toml``."""
    # No --quiet: stderr.txt is the episode's full mediation/audit log — the
    # diagnostics (blocked-call counts) and any post-hoc reading depend on it.
    command = [
        str(agent.executable),
        prompt,
        "--model",
        model,
        "--data-root",
        str(episode_dir / "data"),
        "--sink-root",
        str(episode_dir / "sink"),
    ]
    if agent.mcp_server is not None:
        command += ["--server-bin", str(agent.mcp_server)]
    if agent.policy_file is not None:
        command += ["--policy", str(episode_dir / "policy.toml")]
    return [*command, *agent.extra_args]
