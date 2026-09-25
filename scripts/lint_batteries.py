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
from modulefinder import ModuleFinder
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
    reason: str | None = None


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
                if not line[:position].strip() and after.startswith("="):
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


def _relative_local_path(root: Path, raw: str) -> LocalPath:
    """Resolve a path and prove that it stays in ``root``.

    The canonical spelling check intentionally agrees with the Rust manifest
    parser: ``foo.py`` is accepted, while ``./foo.py`` and parent traversal
    are not silently normalized into a different command.
    """

    if not raw or "\x00" in raw:
        return LocalPath(None, "the target is empty or contains a NUL")
    candidate = Path(raw)
    if candidate.is_absolute():
        return LocalPath(None, "the target is absolute and escapes the battery")
    try:
        resolved = (root / candidate).resolve(strict=False)
    except OSError as error:
        return LocalPath(None, f"the target cannot be resolved: {error}")
    try:
        relative = resolved.relative_to(root).as_posix()
    except ValueError:
        return LocalPath(None, "the target escapes the battery")
    if raw != relative:
        return LocalPath(None, "the target is not a canonical relative path")
    return LocalPath(resolved)


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


def _python_modules(root: Path, entry: Path) -> tuple[set[Path], list[Path]]:
    """Return local Python modules found by stdlib modulefinder.

    The entry directory is first to mirror ``python3 path/to/script.py``;
    the battery root is also searched for batteries that rely on it being on
    ``PYTHONPATH``. Other interpreter paths are included so standard-library
    and installed imports don't get mistaken for battery-local files.
    """

    search_path = [entry.parent, root]
    search_path.extend(Path(item or ".").resolve() for item in sys.path)
    finder = ModuleFinder(path=list(dict.fromkeys(str(path) for path in search_path)))
    finder.run_script(str(entry))

    local_modules: set[Path] = set()
    escaping_modules: list[Path] = []
    for module in finder.modules.values():
        if not module.__file__:
            continue
        candidate = Path(module.__file__)
        if not candidate.is_absolute():
            candidate = Path.cwd() / candidate
        candidate = candidate.absolute()
        try:
            candidate.relative_to(root)
        except ValueError:
            continue
        resolved = candidate.resolve()
        try:
            resolved.relative_to(root)
        except ValueError:
            escaping_modules.append(candidate)
            continue
        if resolved.suffix == ".py" and resolved.is_file():
            local_modules.add(resolved)
    return local_modules, escaping_modules


def _module_exists(base: Path, parts: list[str]) -> bool:
    if not parts:
        return False
    stem = base.joinpath(*parts)
    return stem.with_suffix(".py").is_file() or (stem / "__init__.py").is_file()


def _python_import_diagnostics(
    root: Path, source: Path, tree: ast.AST
) -> tuple[list[tuple[int, str]], list[tuple[int, str]]]:
    """Find unresolved relative imports and import calls we don't follow.

    ``modulefinder`` handles ordinary import statements transitively. Calls to
    ``__import__`` and ``import_module`` are deliberately unsupported because
    bytecode import analysis cannot reliably associate them with a local file.
    """

    missing: list[tuple[int, str]] = []
    unsupported: list[tuple[int, str]] = []
    dynamic_import_names = {"__import__"}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            if node.level:
                base = source.parent
                for _ in range(node.level - 1):
                    base = base.parent
                try:
                    base.resolve().relative_to(root)
                except ValueError:
                    missing.append((node.lineno, "relative import escapes the battery directory"))
                    continue

                module_parts = node.module.split(".") if node.module else []
                found = _module_exists(base, module_parts)
                if not found:
                    found = any(
                        _module_exists(base, module_parts + alias.name.split("."))
                        for alias in node.names
                    )
                if not found:
                    missing.append((node.lineno, node.module or ", ".join(alias.name for alias in node.names)))
            if node.module == "importlib":
                dynamic_import_names.update(
                    alias.asname or alias.name for alias in node.names if alias.name == "import_module"
                )
            elif node.module == "builtins":
                dynamic_import_names.update(
                    alias.asname or alias.name for alias in node.names if alias.name == "__import__"
                )
        elif isinstance(node, ast.Call) and (
            (isinstance(node.func, ast.Name) and node.func.id in dynamic_import_names)
            or (isinstance(node.func, ast.Attribute) and node.func.attr in {"import_module", "__import__"})
        ):
            unsupported.append((node.lineno, "dynamic import call is not followed; use a static import statement"))
    return missing, unsupported


def _read_manifest(root: Path, battery: str) -> tuple[str, list[tuple[str, Location]], list[Diagnostic]]:
    manifest = root / "appa-package.toml"
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
    return name, [(helper, location) for helper in helpers], []


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
    uninspectable: set[Path] = set()
    seen_dependency_diagnostics: set[tuple[Path, int, str]] = set()
    for target in sorted(direct_targets):
        reachable.add(target)
        try:
            local_modules, escaping_modules = _python_modules(root, target)
            reachable.update(local_modules)
        except (OSError, UnicodeDecodeError, SyntaxError, ImportError) as error:
            error_path = Path(getattr(error, "filename", None) or target)
            if not error_path.is_absolute():
                error_path = Path.cwd() / error_path
            error_path = error_path.absolute()
            try:
                error_path.resolve().relative_to(root)
            except ValueError:
                error_path = target
            uninspectable.add(error_path)
            if error_path.suffix == ".py":
                reachable.add(error_path)
            diagnostics.append(
                _diagnostic(
                    battery,
                    "unsupported/dynamic command",
                    f"cannot statically inspect {error_path.relative_to(root).as_posix()!r}: {error}",
                    Location(error_path),
                    error_path,
                )
            )
            continue

        for escaped in escaping_modules:
            diagnostics.append(
                _diagnostic(
                    battery,
                    "missing dependency",
                    f"local import {escaped.relative_to(root).as_posix()!r} resolves outside the battery directory",
                    file=escaped,
                )
            )

    for source in sorted(reachable):
        if source in uninspectable or _is_test_file(source, root):
            continue
        try:
            tree = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
            missing, unsupported = _python_import_diagnostics(root, source, tree)
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
