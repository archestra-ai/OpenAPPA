"""Load an OpenAPPA contract and validate it against a TauBench tool surface."""

import tomllib
from dataclasses import dataclass
from importlib.resources import files

CONTRACTS = files("appa_taubench").joinpath("contracts")
POLICY_MODES = ("guarded", "permissive", "stock")


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
                f"policy {self.name!r} does not match the TauBench tool surface: missing={missing} stale={stale}"
            )


def load_policy(mode: str = "guarded", tool_names: set[str] | frozenset[str] | None = None) -> Policy:
    if mode not in POLICY_MODES:
        raise ValueError(f"unsupported policy mode: {mode}")
    if mode in {"permissive", "stock"}:
        if not tool_names:
            raise ValueError(f"{mode} policy construction requires the Tau tool surface")
        declarations = "\n".join(f"[[tool]]\nname = {name!r}\ndelta = {{}}\n" for name in sorted(tool_names))
        return Policy(
            name=f"banking_knowledge_{mode}_control",
            toml=f'version = 1\ntrust_chain = ["neutral"]\n\n{declarations}',
            tools=frozenset(tool_names),
        )
    name = "banking_knowledge"
    resource = CONTRACTS.joinpath("banking_knowledge.toml")
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
