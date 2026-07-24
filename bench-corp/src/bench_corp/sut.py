"""The systems under test: the two demo agents, each guarded and open.

Four fixed configurations, all driven through the demos' existing CLIs — the
bench adds no flags to either demo. One shared model (``--model``) keeps the
comparison defense-vs-defense.
"""

from __future__ import annotations

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
    kind: str  # "appa" | "fides"
    # appa: the demo policy file to prune per episode; fides: extra CLI flags.
    policy_file: Path | None = None
    extra_args: tuple[str, ...] = ()


SUTS: dict[str, Sut] = {
    "appa": Sut(name="appa", kind="appa", policy_file=CORP_AGENT_DIR / "appa-policy.toml"),
    "appa-open": Sut(name="appa-open", kind="appa", policy_file=CORP_AGENT_DIR / "appa-policy-open.toml"),
    "fides": Sut(name="fides", kind="fides"),
    "fides-open": Sut(name="fides-open", kind="fides", extra_args=("--no-defense",)),
}


def build_binaries() -> None:
    """Build the two Rust binaries the bench spawns (idempotent, up front —
    never mid-episode, where a cargo build would distort durations)."""
    for manifest_dir in (CORP_SYSTEMS_DIR, CORP_AGENT_DIR):
        subprocess.run(
            ["cargo", "build", "--manifest-path", str(manifest_dir / "Cargo.toml")],
            check=True,
        )
    if not FIDES_BIN.is_file():
        sys.exit(
            f"missing {FIDES_BIN}\n"
            "The FIDES demo's virtualenv provides the corp-agent-fides entry point.\n"
            f"Create it once:  cd {FIDES_DIR} && uv venv && uv pip install -e ."
        )


def command_for(
    sut: Sut,
    *,
    prompt: str,
    model: str,
    episode_dir: Path,
) -> list[str]:
    """The subprocess argv for one episode. The episode dir already holds
    ``corpus/``, ``sink/``, and (for APPA SUTs) the pruned ``policy.toml``."""
    # No --quiet: stderr.txt is the episode's full mediation/audit log — the
    # diagnostics (blocked-call counts) and any post-hoc reading depend on it.
    common = [
        prompt,
        "--model",
        model,
        "--data-root",
        str(episode_dir / "corpus"),
        "--sink-root",
        str(episode_dir / "sink"),
        "--server-bin",
        str(CORP_SYSTEMS_BIN),
    ]
    if sut.kind == "appa":
        return [str(CORP_AGENT_BIN), *common, "--policy", str(episode_dir / "policy.toml")]
    return [str(FIDES_BIN), *common, *sut.extra_args]
