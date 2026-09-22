#!/usr/bin/env python3
"""Check that every production battery script is reachable from appa.toml.

The linter deliberately understands a small command language.  This keeps the
answer useful for package validation: a command is either an exact two-element
argv for ``python3`` and one local Python script, or it is diagnosed as an
unsupported command. It does not try to execute TOML or Python.
"""

from __future__ import annotations

import argparse
import ast
from dataclasses import dataclass
from pathlib import Path
import sys
import tomllib
from typing import Any, Iterator, Sequence


@dataclass(frozen=True)
class Location:
    path: Path
    line: int | None = None

    def display(self) -> str:
        return f"{self.path}:{self.line}" if self.line is not None else str(self.path)


@dataclass(frozen=True)
class Diagnostic:
    """One actionable linter finding.

    ``kind`` is intentionally a short, stable phrase because CI and editor
    integrations can use it without parsing the prose message.
    """

    battery: str
    kind: str
    message: str
    location: Location | None = None
    file: Path | None = None
    severity: str = "error"

    @property
    def path(self) -> Path | None:
        """Alias useful to callers that think of a diagnostic as a path."""

        return self.file or (self.location.path if self.location else None)

    def __str__(self) -> str:
        where = f" [{self.location.display()}]" if self.location else ""
        affected = f" ({self.file})" if self.file and (not self.location or self.file != self.location.path) else ""
        return f"{self.severity}: {self.battery}: {self.kind}{where}{affected}: {self.message}"


@dataclass(frozen=True)
class Command:
    argv: Any
    location: Location


@dataclass(frozen=True)
class LocalPath:
    path: Path | None
    relative: str | None
    reason: str | None = None


@dataclass(frozen=True)
class Dependency:
    raw: str
    line: int


def _line_for_key(text: str, key: str, start: int = 0) -> tuple[int, int] | None:
    """Find the next ordinary ``key =`` line for a diagnostic location.

    TOML itself is parsed only by ``tomllib``.  This small source locator is
    not a TOML parser; it is best-effort metadata for diagnostics, and safely
    returns no line when a key is written in an unusual form.
    """

    offset = 0
    for number, line in enumerate(text.splitlines(keepends=True), 1):
        if offset < start:
            offset += len(line)
            continue
        stripped = line.lstrip()
        if not stripped.startswith("#"):
            position = line.find(key)
            while position >= 0:
                after = line[position + len(key) :].lstrip()
                before = line[:position].rstrip()
                if (not before or before.endswith((" ", "\t"))) and after.startswith("="):
                    return number, offset + position
                position = line.find(key, position + 1)
        offset += len(line)
    return None


def _command_values(
    value: Any, source: Path, text: str, cursor: list[int] | None = None
) -> Iterator[Command]:
    """Yield every value under a TOML key named ``command`` recursively."""

    if cursor is None:
        cursor = [0]
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "command":
                found = _line_for_key(text, "command", cursor[0])
                if found is None:
                    location = Location(source)
                else:
                    line, position = found
                    location = Location(source, line)
                    newline = text.find("\n", position)
                    cursor[0] = len(text) if newline < 0 else newline + 1
                yield Command(child, location)
            yield from _command_values(child, source, text, cursor)
    elif isinstance(value, list):
        for child in value:
            yield from _command_values(child, source, text, cursor)


def _toml_line(error: tomllib.TOMLDecodeError) -> int | None:
    return getattr(error, "lineno", None)


def _relative_local_path(root: Path, raw: str, *, canonical: bool = True) -> LocalPath:
    """Resolve a path and prove that it stays in ``root``.

    The canonical spelling check intentionally agrees with the Rust manifest
    parser: ``foo.py`` is accepted, while ``./foo.py`` and parent traversal
    are not silently normalized into a different command.
    """

    if not raw or "\x00" in raw:
        return LocalPath(None, None, "the target is empty or contains a NUL")
    candidate = Path(raw)
    if candidate.is_absolute():
        return LocalPath(None, None, "the target is absolute and escapes the battery")
    try:
        resolved = (root / candidate).resolve(strict=False)
    except OSError as error:
        return LocalPath(None, None, f"the target cannot be resolved: {error}")
    try:
        relative = resolved.relative_to(root).as_posix()
    except ValueError:
        return LocalPath(None, None, "the target escapes the battery")
    if canonical and raw != relative:
        return LocalPath(None, relative, "the target is not a canonical relative path")
    return LocalPath(resolved, relative)


def _diagnostic(
    battery: str,
    kind: str,
    message: str,
    location: Location | None = None,
    file: Path | None = None,
) -> Diagnostic:
    return Diagnostic(battery=battery, kind=kind, message=message, location=location, file=file)


def _parse_command(
    command: Command,
    battery: str,
    root: Path,
) -> tuple[Path | None, list[Diagnostic]]:
    """Validate one command and return its existing script target, if any."""

    value = command.argv
    if not isinstance(value, list) or not value or not all(isinstance(item, str) for item in value):
        return None, [
            _diagnostic(
                battery,
                "unsupported/dynamic command",
                "command must be a string argv array with exactly an interpreter and one script target",
                command.location,
            )
        ]
    if len(value) != 2:
        return None, [
            _diagnostic(
                battery,
                "unsupported/dynamic command",
                f"unsupported command shape {value!r}; only two-element local-script argv is accepted",
                command.location,
            )
        ]
    interpreter, raw_target = value
    if interpreter != "python3":
        return None, [
            _diagnostic(
                battery,
                "unsupported/dynamic command",
                f"unsupported interpreter {interpreter!r}; battery commands must use 'python3'",
                command.location,
            )
        ]
    suffix = ".py"
    if not raw_target.endswith(suffix):
        return None, [
            _diagnostic(
                battery,
                "unsupported/dynamic command",
                f"{interpreter!r} must be followed by a local {suffix} file, got {raw_target!r}",
                command.location,
            )
        ]
    local = _relative_local_path(root, raw_target)
    if local.reason:
        return None, [
            _diagnostic(
                battery,
                "unsupported/dynamic command",
                f"command target {raw_target!r} is invalid: {local.reason}",
                command.location,
            )
        ]
    assert local.path is not None
    if not local.path.is_file():
        return None, [
            _diagnostic(
                battery,
                "missing dependency",
                f"command target {raw_target!r} does not name a file inside the battery",
                command.location,
                local.path,
            )
        ]
    return local.path, []


def _is_test_file(path: Path, root: Path) -> bool:
    try:
        relative = path.relative_to(root)
    except ValueError:
        return False
    if any(part in {"test", "tests"} for part in relative.parts[:-1]):
        return True
    name = path.name
    return name.startswith("test_") or name.endswith("_test.py")


def _production_scripts(root: Path) -> Iterator[Path]:
    for path in sorted(root.rglob("*")):
        if path.is_file() and path.suffix == ".py" and not _is_test_file(path, root):
            yield path


def _module_file(base: Path, parts: Sequence[str]) -> Path | None:
    if not parts:
        return None
    stem = base.joinpath(*parts)
    module = stem.with_suffix(".py")
    if module.is_file():
        return module
    package = stem / "__init__.py"
    if package.is_file():
        return package
    return None


def _python_imports(
    root: Path, source: Path, tree: ast.AST
) -> tuple[list[Dependency], list[tuple[int, str]], list[tuple[int, str]]]:
    dependencies: list[Dependency] = []
    missing: list[tuple[int, str]] = []
    unsupported: list[tuple[int, str]] = []

    def bases_for(node: ast.ImportFrom) -> list[Path]:
        if node.level:
            base = source.parent
            for _ in range(max(0, node.level - 1)):
                base = base.parent
            return [base]
        # A helper imported by a script is normally beside that script.  The
        # battery root fallback also covers a nested entry point run with the
        # battery root on PYTHONPATH.
        return [source.parent] if source.parent == root else [source.parent, root]

    def add(path: Path, line: int) -> None:
        if path not in dependencies_by_path:
            dependencies_by_path.add(path)
            dependencies.append(Dependency(path.relative_to(root).as_posix(), line))

    dependencies_by_path: set[Path] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                parts = alias.name.split(".")
                for base in ([source.parent] if source.parent == root else [source.parent, root]):
                    found = _module_file(base, parts)
                    if found:
                        add(found, node.lineno)
                        break
        elif isinstance(node, ast.ImportFrom):
            module_parts = node.module.split(".") if node.module else []
            found_any = False
            for base in bases_for(node):
                module_file = _module_file(base, module_parts) if module_parts else None
                if module_file:
                    add(module_file, node.lineno)
                    found_any = True
                    # ``from package import helper`` can import another local
                    # module, while ``from helper import function`` usually
                    # resolves to helper.py itself.
                    if module_file.name == "__init__.py":
                        package_base = module_file.parent
                        for alias in node.names:
                            child = _module_file(package_base, alias.name.split("."))
                            if child:
                                add(child, node.lineno)
                    break
                for alias in node.names:
                    names = module_parts + alias.name.split(".")
                    child = _module_file(base, names)
                    if child:
                        add(child, node.lineno)
                        found_any = True
            if node.level and not found_any:
                missing.append((node.lineno, node.module or ", ".join(alias.name for alias in node.names)))
        elif isinstance(node, ast.Call):
            dynamic = False
            if isinstance(node.func, ast.Name) and node.func.id == "__import__":
                dynamic = not node.args or not isinstance(node.args[0], ast.Constant) or not isinstance(node.args[0].value, str)
            elif isinstance(node.func, ast.Attribute) and node.func.attr == "import_module":
                dynamic = not node.args or not isinstance(node.args[0], ast.Constant) or not isinstance(node.args[0].value, str)
            if dynamic:
                unsupported.append((node.lineno, "dynamic import is not followed because its module name is not static"))
    return dependencies, missing, unsupported


def _read_manifest(root: Path, battery: str) -> tuple[str, list[tuple[str, Location]], list[Diagnostic]]:
    manifest = root / "appa-package.toml"
    diagnostics: list[Diagnostic] = []
    try:
        text = manifest.read_text(encoding="utf-8")
        data = tomllib.loads(text)
    except FileNotFoundError:
        return battery, [], [_diagnostic(battery, "invalid manifest", "appa-package.toml is missing", Location(manifest))]
    except tomllib.TOMLDecodeError as error:
        return battery, [], [
            _diagnostic(battery, "invalid manifest", f"appa-package.toml is not valid TOML: {error}", Location(manifest, _toml_line(error)))
        ]
    name = data.get("name") if isinstance(data.get("name"), str) else battery
    table = data.get("battery")
    if not isinstance(table, dict):
        return name, [], [_diagnostic(name, "invalid manifest", "appa-package.toml has no [battery] table", Location(manifest))]
    helpers = table.get("helpers", [])
    if not isinstance(helpers, list) or not all(isinstance(helper, str) for helper in helpers):
        return name, [], [_diagnostic(name, "invalid manifest", "[battery].helpers must be an array of strings", Location(manifest))]
    found = _line_for_key(text, "helpers")
    location = Location(manifest, found[0] if found else None)
    return name, [(helper, location) for helper in helpers], diagnostics


def lint_battery(root: Path) -> list[Diagnostic]:
    """Lint one battery directory and return all findings in stable order."""

    root = root.resolve()
    default_name = root.name
    battery, helper_declarations, diagnostics = _read_manifest(root, default_name)
    appa = root / "appa.toml"
    try:
        appa_text = appa.read_text(encoding="utf-8")
        appa_data = tomllib.loads(appa_text)
    except FileNotFoundError:
        return diagnostics + [_diagnostic(battery, "invalid manifest", "appa.toml is missing", Location(appa))]
    except tomllib.TOMLDecodeError as error:
        return diagnostics + [
            _diagnostic(battery, "invalid manifest", f"appa.toml is not valid TOML: {error}", Location(appa, _toml_line(error)))
        ]

    commands = list(_command_values(appa_data, appa, appa_text))
    direct_targets: set[Path] = set()
    for command in commands:
        target, found = _parse_command(command, battery, root)
        diagnostics.extend(found)
        if target:
            direct_targets.add(target)

    helper_paths: dict[Path, tuple[str, Location]] = {}
    for raw, location in helper_declarations:
        local = _relative_local_path(root, raw)
        if local.reason or local.path is None:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "missing dependency",
                    f"declared helper {raw!r} is invalid: {local.reason or 'not a local path'}",
                    location,
                    root / raw,
                )
            )
            continue
        if not local.path.is_file():
            diagnostics.append(
                _diagnostic(
                    battery,
                    "missing dependency",
                    f"declared helper {raw!r} does not name a file inside the battery",
                    location,
                    local.path,
                )
            )
            continue
        helper_paths[local.path] = (raw, location)

    for target in sorted(direct_targets):
        if target not in helper_paths:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "command target missing from helpers",
                    f"command target {target.relative_to(root).as_posix()!r} is not declared in [battery].helpers",
                    next((command.location for command in commands if _command_target_path(command, root) == target), None),
                    target,
                )
            )

    reachable: set[Path] = set()
    pending = list(sorted(direct_targets))
    seen_dependency_diagnostics: set[tuple[Path, int, str]] = set()
    while pending:
        source = pending.pop()
        if source in reachable:
            continue
        reachable.add(source)
        try:
            if source.suffix == ".py":
                tree = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
                dependencies, missing, unsupported = _python_imports(root, source, tree)
            else:
                dependencies, unsupported = [], []
                missing = []
        except (OSError, UnicodeDecodeError, SyntaxError) as error:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "unsupported/dynamic command",
                    f"cannot statically inspect {source.relative_to(root).as_posix()!r}: {error}",
                    Location(source),
                    source,
                )
            )
            continue
        for line, message in unsupported:
            key = (source, line, message)
            if key not in seen_dependency_diagnostics:
                seen_dependency_diagnostics.add(key)
                diagnostics.append(
                    _diagnostic(
                        battery,
                        "unsupported/dynamic command",
                        message,
                        Location(source, line),
                        source,
                    )
                )
        for line, message in missing:
            key = (source, line, message)
            if key not in seen_dependency_diagnostics:
                seen_dependency_diagnostics.add(key)
                diagnostics.append(
                    _diagnostic(
                        battery,
                        "missing dependency",
                        message,
                        Location(source, line),
                        source,
                    )
                )
        for dependency in dependencies:
            local = _relative_local_path(root, dependency.raw, canonical=False)
            if local.reason or local.path is None:
                diagnostics.append(
                    _diagnostic(
                        battery,
                        "missing dependency",
                        f"local dependency {dependency.raw!r} is invalid: {local.reason or 'not a local path'}",
                        Location(source, dependency.line),
                        source,
                    )
                )
                continue
            expected = ".py"
            if not local.relative or not local.relative.endswith(expected):
                diagnostics.append(
                    _diagnostic(
                        battery,
                        "missing dependency",
                        f"local dependency {dependency.raw!r} is not a {expected} file",
                        Location(source, dependency.line),
                        source,
                    )
                )
                continue
            if not local.path.is_file():
                diagnostics.append(
                    _diagnostic(
                        battery,
                        "missing dependency",
                        f"local dependency {dependency.raw!r} does not name a file inside the battery",
                        Location(source, dependency.line),
                        source,
                    )
                )
                continue
            if local.path not in reachable:
                pending.append(local.path)

    for path, (raw, location) in sorted(helper_paths.items()):
        if path not in reachable:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "unused manifest helper",
                    f"declared helper {raw!r} is not reachable from any appa.toml command",
                    location,
                    path,
                )
            )
    for path in _production_scripts(root):
        if path not in reachable:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "unreferenced production script",
                    f"production script {path.relative_to(root).as_posix()!r} is not reachable from an appa.toml command",
                    file=path,
                )
            )
    return diagnostics


def _command_target_path(command: Command, root: Path) -> Path | None:
    value = command.argv
    if not isinstance(value, list) or len(value) != 2 or not all(isinstance(item, str) for item in value):
        return None
    local = _relative_local_path(root, value[1])
    return local.path if local.reason is None and local.path and local.path.is_file() else None


def lint_batteries(batteries_root: Path) -> list[Diagnostic]:
    """Lint every immediate battery directory below ``batteries_root``."""

    root = batteries_root.resolve()
    if (root / "appa.toml").is_file():
        directories = [root]
    else:
        directories = [
            path
            for path in sorted(root.iterdir())
            if path.is_dir() and ((path / "appa.toml").is_file() or (path / "appa-package.toml").is_file())
        ]
    diagnostics: list[Diagnostic] = []
    for directory in directories:
        diagnostics.extend(lint_battery(directory))
    return diagnostics


def _default_batteries_root() -> Path:
    return Path(__file__).resolve().parent.parent / "marketplace" / "batteries"


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "batteries_root",
        nargs="?",
        type=Path,
        default=_default_batteries_root(),
        help="battery directory or directory containing batteries (default: marketplace/batteries)",
    )
    args = parser.parse_args(argv)
    diagnostics = lint_batteries(args.batteries_root)
    for diagnostic in diagnostics:
        print(diagnostic, file=sys.stderr)
    return 1 if diagnostics else 0


if __name__ == "__main__":
    raise SystemExit(main())
