"""Load an APPA policy and validate it against an AgentDojo tool surface."""

import tomllib
from dataclasses import dataclass
from pathlib import Path

CONTRACTS_DIR = Path(__file__).resolve().parents[2] / "contracts"


@dataclass(frozen=True)
class Policy:
    name: str
    toml: str
    tools: frozenset[str]

    def check_covers(self, suite_tools: set[str]) -> None:
        missing = sorted(suite_tools - self.tools)
        stale = sorted(self.tools - suite_tools)
        if missing or stale:
            raise ValueError(
                f"policy {self.name!r} does not match the AgentDojo tool surface: missing={missing} stale={stale}"
            )


def load_policy(name: str) -> Policy:
    path = CONTRACTS_DIR / f"{name}.toml"
    source = path.read_text()
    raw = tomllib.loads(source)
    entries = raw.get("tool")
    if not isinstance(entries, list):
        raise ValueError(f"policy {path} has no [[tool]] declarations")
    names = []
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
            raise ValueError(f"policy {path} has an invalid [[tool]] declaration")
        names.append(entry["name"])
    if len(names) != len(set(names)):
        raise ValueError(f"policy {path} declares a tool more than once")
    return Policy(name=name, toml=source, tools=frozenset(names))
