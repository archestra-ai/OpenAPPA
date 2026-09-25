import tomllib
from pathlib import Path


BATTERIES = Path(__file__).resolve().parents[1] / "marketplace" / "batteries"
REQUIRED = {"appa-package.toml", "appa.toml"}


def unreachable_rules(policy: Path) -> list[str]:
    """Rules for one tool are tried in order and the first match wins, so a
    rule without an argument selector hides every later rule for its tool."""
    try:
        rules = tomllib.loads(policy.read_text(encoding="utf-8")).get("policy", {}).get("tool", [])
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return []
    errors = []
    unconditional = set()
    for rule in rules:
        name = rule.get("name") if isinstance(rule, dict) else None
        if not isinstance(name, str):
            continue
        tool, selector, _ = name.partition("(")
        if tool in unconditional:
            errors.append(f"{policy}: rule {name!r} is unreachable: an earlier rule for {tool} matches every call")
        if not selector:
            unconditional.add(tool)
    return errors


def lint(root: Path) -> list[str]:
    if not root.is_dir():
        return [f"{root}: battery directory is missing"]

    batteries = sorted(path for path in root.iterdir() if path.is_dir() and path.name != "__pycache__")
    if not batteries:
        return [f"{root}: no battery directories found"]

    errors = []
    for battery in batteries:
        for name in sorted(REQUIRED):
            if not (battery / name).is_file():
                errors.append(f"{battery / name}: required TOML file is missing")
        manifest = battery / "appa-package.toml"
        if manifest.is_file():
            try:
                package = tomllib.loads(manifest.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
                errors.append(f"{manifest}: cannot read package manifest: {error}")
            else:
                battery_config = package.get("battery")
                if not isinstance(battery_config, dict) or battery_config.get("policy") != "appa.toml":
                    errors.append(f"{manifest}: battery.policy must name appa.toml")
        errors.extend(unreachable_rules(battery / "appa.toml"))

    allowed = {battery / name for battery in batteries for name in REQUIRED}
    for path in sorted(root.rglob("*")):
        if path.suffix.lower() == ".toml" and path.is_file() and path not in allowed:
            errors.append(f"{path}: unexpected battery TOML file")
    return errors


if __name__ == "__main__":
    import sys

    problems = lint(BATTERIES)
    if problems:
        print("\n".join(problems), file=sys.stderr)
        sys.exit(1)
    count = sum(path.is_dir() and path.name != "__pycache__" for path in BATTERIES.iterdir())
    print(f"Battery TOML files: {count} directories checked")
