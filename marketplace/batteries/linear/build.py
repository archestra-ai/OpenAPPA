#!/usr/bin/env python3
"""Render reviewed Linear contracts; --check verifies without changing files."""

import argparse
import json
from pathlib import Path
import sys
from contract import PROFILES

ROOT = Path(__file__).resolve().parent


def toml(value):
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, (int, float)):
        return json.dumps(value, allow_nan=False)
    if isinstance(value, list):
        return "[" + ", ".join(toml(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{ " + ", ".join(toml(k) + " = " + toml(v) for k, v in value.items()) + " }"
    raise ValueError("unsupported TOML value")


def render():
    lock = json.loads((ROOT / "schema-lock.json").read_text())
    tools = lock["surfaces"]["read-write"]["tools"]
    readonly = set(lock["surfaces"]["read-only"]["tools"])
    operations = json.loads((ROOT / "operations.json").read_text())["operations"]
    if set(tools) != set(operations):
        raise ValueError("locked tools and reviewed operations differ; review drift first")
    if readonly != {name for name, op in operations.items() if op["kind"] == "read"}:
        raise ValueError("read-only surface differs from reviewed read contracts")
    for op in operations.values():
        if op["kind"] not in ("read", "write", "sensitive"):
            raise ValueError("unreviewed operation kind")
        scope, variable = set(op["scope_arguments"]), set(op["variable_arguments"])
        if scope & variable or not set(op["required_arguments"]) <= scope | variable:
            raise ValueError("invalid argument classification")
    files = {}
    for profile in PROFILES:
        lines = ['# Generated from schema-lock.json and operations.json by build.py.',
                 '# Override this annotator in the root config with explicit resource rules.',
                 '[policy]', 'version = 2', '', '[[policy.annotator]]',
                 f'name = "linear.{profile}"', 'hint = \'{"rules":{}}\'',
                 'ranks = ["suspicious", "trusted"]', 'marks = ["linear-review"]',
                 'effects = ["linear.changed", "linear.sensitive"]', '',
                 f'[externals.annotators."linear.{profile}"]', 'command = ["python3", "annotate.py"]', '']
        for name in sorted(tools):
            if profile == "read-only" and name not in readonly:
                continue
            lines += ['[[policy.tool]]', f'name = {toml(name)}', 'server = "linear"',
                      f'tags = ["linear", "linear-{operations[name]["kind"]}"]',
                      'parameters = { type = "object", additionalProperties = true }',
                      f'annotator = "linear.{profile}"', '']
        files['appa.toml' if profile == 'approved-writes' else profile + '.toml'] = '\n'.join(lines)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    parser.add_argument('--check', action='store_true', help='report stale generated files without writing')
    args = parser.parse_args()
    try:
        outputs = render()
        stale = [name for name, body in outputs.items() if not (ROOT / name).exists() or (ROOT / name).read_text() != body]
        if args.check:
            if stale:
                print('Run python3 marketplace/batteries/linear/build.py: ' + ', '.join(stale), file=sys.stderr)
                return 1
        else:
            for name, body in outputs.items():
                (ROOT / name).write_text(body)
        return 0
    except (ValueError, OSError, KeyError) as error:
        print('Linear contract generation failed: ' + str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
