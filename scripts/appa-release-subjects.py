#!/usr/bin/env python3
"""Name a release's staged artifacts for archestra-ai/release-publisher.

Reads the staged digest of every OpenAPPA image and the release's chart
archives, and writes:

  release-images.json   {"<public name>": "sha256:<digest>"}, the request
  release-subjects.txt  "<sha256>  <name>" lines for attest-build-provenance:
                        each image under its public name, each chart by
                        its file name

The publisher copies a digest only when this run attested it under exactly
the name it publishes, so these two files must agree; one script writes both.

Usage: appa-release-subjects.py VERSION DIGESTS_JSON CHARTS_DIR
  DIGESTS_JSON maps every image in IMAGES to its staged digest.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

PUBLIC = "ghcr.io/archestra-ai"
IMAGES = ("appa-runtime", "appa-kagent-adk", "appa-kagent-adk-go", "golang-adk",
          "appa-demo-tools", "appa-demo-mocks")
CHARTS = ("appa-runtime", "appa-kagent-demo")
VERSION = re.compile(r"\d+\.\d+\.\d+(?:-[a-z]+\.\d+)?")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def subjects(version: str, digests: dict, charts: Path) -> tuple[dict, list[str]]:
    if not VERSION.fullmatch(version):
        raise ValueError(f"not a release version: {version}")
    if not isinstance(digests, dict) or set(digests) != set(IMAGES):
        raise ValueError(f"digests must name exactly {', '.join(IMAGES)}")
    for image, digest in digests.items():
        if not isinstance(digest, str) or not DIGEST.fullmatch(digest):
            raise ValueError(f"no staged digest for {image}")
    # kagent derives golang-adk from the agent image name; it is an alias.
    if digests["golang-adk"] != digests["appa-kagent-adk-go"]:
        raise ValueError("golang-adk must name the appa-kagent-adk-go digest")
    expected = {f"{chart}-{version}.tgz" for chart in CHARTS}
    found = {path.name for path in charts.iterdir()}
    if found != expected:
        raise ValueError(f"{charts} must hold exactly {', '.join(sorted(expected))}")
    images = {f"{PUBLIC}/{image}": digests[image] for image in sorted(digests)}
    lines = [f"{digest.removeprefix('sha256:')}  {name}" for name, digest in images.items()]
    lines += [f"{hashlib.sha256((charts / name).read_bytes()).hexdigest()}  {name}" for name in sorted(expected)]
    return images, lines


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        raise ValueError("usage: appa-release-subjects.py VERSION DIGESTS_JSON CHARTS_DIR")
    images, lines = subjects(argv[1], json.loads(argv[2]), Path(argv[3]))
    Path("release-images.json").write_text(json.dumps(images, indent=2) + "\n")
    Path("release-subjects.txt").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except (ValueError, json.JSONDecodeError, OSError) as error:
        sys.exit(f"appa-release-subjects: {error}")
