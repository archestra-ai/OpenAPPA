#!/usr/bin/env python3
"""Pin a release chart's images to the digests released with it.

release.yml runs this on a chart's values.yaml before `helm package`. For
each IMAGE=DIGEST argument it finds the one `image:` block whose
`repository:` ends in `/IMAGE` and sets its `digest: ""` line to DIGEST, so
the packaged chart pulls exactly the images archestra-ai/release-publisher
published from this release. A chart installed with --set image.tag keeps
working: the templates use the tag only when the digest is empty.

Usage: appa-chart-digests.py values.yaml appa-runtime=sha256:<64 hex> ...
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

NAME = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
REPOSITORY = re.compile(r"(?P<indent> *)repository: *(?P<value>\S+) *(?:#.*)?")
EMPTY_DIGEST = re.compile(r'(?P<indent> *)digest: *"" *(?:#.*)?')


def pin(values: str, digests: dict[str, str]) -> str:
    lines = values.splitlines(keepends=True)
    for image, digest in digests.items():
        if not NAME.fullmatch(image) or not DIGEST.fullmatch(digest):
            raise ValueError(f"invalid pin {image}={digest}")
        blocks = [
            index for index, line in enumerate(lines)
            if (match := REPOSITORY.fullmatch(line.rstrip("\n")))
            and match["value"].endswith(f"/{image}")
        ]
        if len(blocks) != 1:
            raise ValueError(f"expected one repository ending in /{image}, found {len(blocks)}")
        start = blocks[0]
        indent = REPOSITORY.fullmatch(lines[start].rstrip("\n"))["indent"]
        # The digest line is a sibling key of the same mapping.
        index = start + 1
        while index < len(lines):
            line = lines[index].rstrip("\n")
            stripped = line.lstrip(" ")
            if stripped and not stripped.startswith("#") and len(line) - len(stripped) < len(indent):
                break
            match = EMPTY_DIGEST.fullmatch(line)
            if match and match["indent"] == indent:
                lines[index] = f'{indent}digest: "{digest}"\n'
                break
            index += 1
        else:
            index = len(lines)
        if index == len(lines) or not lines[index].strip().startswith("digest:"):
            raise ValueError(f'the {image} image has no empty digest: "" line')
    return "".join(lines)


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        raise ValueError("usage: appa-chart-digests.py values.yaml IMAGE=DIGEST ...")
    digests = dict(argument.split("=", 1) for argument in argv[2:])
    path = Path(argv[1])
    path.write_text(pin(path.read_text(), digests))
    for image, digest in digests.items():
        print(f"Pinned {image} to {digest} in {path}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except ValueError as error:
        sys.exit(f"appa-chart-digests: {error}")
