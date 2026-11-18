"""Print the appa-guide skill text one host loads, as appa-runtime/src/init/skill.rs
composes it: SKILL.md, references/core.md, and the host reference, joined by a blank line.

    python3 bench/appa-guide/build_skill.py claude-code            # working tree
    python3 bench/appa-guide/build_skill.py kagent --ref origin/main
"""

import argparse
import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
PARTS = ("SKILL.md", "references/core.md", "references/{host}.md")


def read(path, ref):
    if ref is None:
        return (ROOT / path).read_text(encoding="utf-8")
    return subprocess.run(
        ["git", "-C", str(ROOT), "show", f"{ref}:{path}"],
        check=True, capture_output=True, text=True,
    ).stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("host", choices=("claude-code", "kagent", "opencode"))
    parser.add_argument("--ref", help="git ref to read instead of the working tree")
    args = parser.parse_args()
    paths = [f"integrations/appa-guide/{p.format(host=args.host)}" for p in PARTS]
    print("\n\n".join(read(p, args.ref) for p in paths), end="")


main()
