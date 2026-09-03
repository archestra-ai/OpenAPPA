"""The benchmarked agents: the demo agents under fixed defense settings.

Fixed configurations, all driven through the demos' existing CLIs — the bench
adds no flags of its own. One shared model (``--model``) keeps the comparison
defense-vs-defense: the appa agent guarded, branching-disabled, remedy-disabled,
and open, plus FIDES middleware-only, native auto-hide, and unmediated modes.
"""

from __future__ import annotations

import os
import subprocess
import sys
from dataclasses import dataclass
from enum import Enum
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[4]
POLICIES_DIR = Path(__file__).resolve().parents[2] / "policies"
CORP_SYSTEMS_DIR = REPO_ROOT / "bench" / "corp-systems"
CORP_AGENT_DIR = REPO_ROOT / "bench" / "corp-agent"
FIDES_DIR = REPO_ROOT / "bench" / "corp-agent-fides"

CORP_SYSTEMS_BIN = CORP_SYSTEMS_DIR / "target" / "debug" / "corp-systems-mcp"
APPA_CORP_AGENT_BIN = CORP_AGENT_DIR / "target" / "debug" / "appa-corp-agent"
FIDES_BIN = FIDES_DIR / ".venv" / "bin" / "corp-agent-fides"

DEFAULT_MODEL = "openai/gpt-5.6-luna"


class PolicyTarget(Enum):
    APPA_GUARDED = "appa-guarded"
    APPA_OPEN = "appa-open"
    FIDES = "fides"
    NONE = "none"


@dataclass(frozen=True)
class Agent:
    name: str
    executable: Path
    policy_target: PolicyTarget
    # Set only for APPA agents: the benchmark policy the runner prunes per episode.
    policy_file: Path | None = None
    # Set only for agents that spawn the MCP server (the appa agent runs the
    # corp systems in-process and takes no --server-bin).
    mcp_server: Path | None = None
    extra_args: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not isinstance(self.policy_target, PolicyTarget):
            raise TypeError(f"{self.name}: policy_target must be a PolicyTarget")
        match self.policy_target:
            case PolicyTarget.APPA_GUARDED | PolicyTarget.APPA_OPEN:
                if self.policy_file is None:
                    raise ValueError(f"{self.name}: APPA agents require a source policy")
            case PolicyTarget.FIDES | PolicyTarget.NONE:
                if self.policy_file is not None:
                    raise ValueError(f"{self.name}: only APPA agents can declare policy_file")


AGENTS: dict[str, Agent] = {
    # The appa agent is appa-corp-agent: the full appa-example-agent loop with
    # the registered fork tool and runtime remedies live. The two ablations
    # independently remove each recovery mechanism while retaining the same
    # guarded policy; the open arm removes policy restrictions.
    "appa": Agent(
        name="appa",
        executable=APPA_CORP_AGENT_BIN,
        policy_target=PolicyTarget.APPA_GUARDED,
        policy_file=POLICIES_DIR / "appa.toml",
    ),
    "appa-nofork": Agent(
        name="appa-nofork",
        executable=APPA_CORP_AGENT_BIN,
        policy_target=PolicyTarget.APPA_GUARDED,
        policy_file=POLICIES_DIR / "appa.toml",
        extra_args=("--max-forks", "0"),
    ),
    "appa-noremedy": Agent(
        name="appa-noremedy",
        executable=APPA_CORP_AGENT_BIN,
        policy_target=PolicyTarget.APPA_GUARDED,
        policy_file=POLICIES_DIR / "appa.toml",
        extra_args=("--no-remedies",),
    ),
    "appa-open": Agent(
        name="appa-open",
        executable=APPA_CORP_AGENT_BIN,
        policy_target=PolicyTarget.APPA_OPEN,
        policy_file=POLICIES_DIR / "open.toml",
    ),
    "fides-middleware": Agent(
        name="fides-middleware",
        executable=FIDES_BIN,
        policy_target=PolicyTarget.FIDES,
        mcp_server=CORP_SYSTEMS_BIN,
        extra_args=("--mode", "middleware-only"),
    ),
    "fides-native": Agent(
        name="fides-native",
        executable=FIDES_BIN,
        policy_target=PolicyTarget.FIDES,
        mcp_server=CORP_SYSTEMS_BIN,
        extra_args=("--mode", "native-auto-hide"),
    ),
    "fides-open": Agent(
        name="fides-open",
        executable=FIDES_BIN,
        policy_target=PolicyTarget.FIDES,
        mcp_server=CORP_SYSTEMS_BIN,
        extra_args=("--mode", "unmediated"),
    ),
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
    policy_path: Path | None,
) -> list[str]:
    """The subprocess argv for one episode. The episode dir already holds
    ``data/``, ``sink/``, and (for APPA agents) the pruned ``policy.toml``."""
    # No --quiet: stderr.txt is the episode's full mediation/audit log — the
    # diagnostics (blocked-call counts) and any post-hoc reading depend on it.
    episode_dir = episode_dir.resolve()
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
    match agent.policy_target:
        case PolicyTarget.APPA_GUARDED | PolicyTarget.APPA_OPEN:
            if policy_path is None:
                raise ValueError(f"{agent.name}: APPA agents require a staged policy")
            command += [
                "--policy",
                str(policy_path.resolve()),
                "--status-file",
                str((episode_dir / "agent-status.json").resolve()),
            ]
        case PolicyTarget.FIDES:
            if policy_path is not None:
                command += ["--profile", str(policy_path.resolve())]
        case PolicyTarget.NONE:
            if policy_path is not None:
                raise ValueError(f"{agent.name}: policy-free agents cannot receive a staged policy")
    return [*command, *agent.extra_args]
