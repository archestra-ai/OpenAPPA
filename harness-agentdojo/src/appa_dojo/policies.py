"""Load an APPA policy and validate it against an AgentDojo tool surface."""

import tomllib
from dataclasses import dataclass
from importlib.resources import files

CONTRACTS = files("appa_dojo").joinpath("contracts")


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
    resource = CONTRACTS.joinpath(f"{name}.toml")
    source = resource.read_text(encoding="utf-8")
    raw = tomllib.loads(source)
    entries = raw.get("tool")
    if not isinstance(entries, list):
        raise ValueError(f"policy {name!r} has no [[tool]] declarations")
    names = []
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
            raise ValueError(f"policy {name!r} has an invalid [[tool]] declaration")
        names.append(entry["name"])
    if len(names) != len(set(names)):
        raise ValueError(f"policy {name!r} declares a tool more than once")
    return Policy(name=name, toml=source, tools=frozenset(names))
