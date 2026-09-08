#!/usr/bin/env python3
"""Record a published OCI manifest and its runnable platform digests.

The caller reads the manifest by the digest passed here, not by a mutable tag.
This is release tooling, not a registry client or an installation script.
"""

import argparse
import json
import re
import sys

MAX_BYTES = 1024 * 1024
PLATFORMS = frozenset({"linux/amd64", "linux/arm64"})


def checked_digest(value):
    if not isinstance(value, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        raise ValueError("expected sha256:<64 lowercase hexadecimal characters>")
    return value


def describe(digest, manifest, single_platform=None):
    checked_digest(digest)
    platforms = {}
    if "manifests" in manifest:
        for child in manifest["manifests"]:
            platform = child.get("platform", {})
            # BuildKit attestations are descriptors, not runnable images.
            if platform.get("os") == "unknown" and platform.get("architecture") == "unknown":
                continue
            name = f"{platform.get('os')}/{platform.get('architecture')}"
            if name not in PLATFORMS or name in platforms:
                raise ValueError(f"unsupported or duplicate platform: {name}")
            platforms[name] = checked_digest(child.get("digest"))
    else:
        if manifest.get("mediaType") not in {
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        } or single_platform not in PLATFORMS:
            raise ValueError("a single manifest requires its explicit build platform")
        platforms[single_platform] = digest
    if not platforms:
        raise ValueError("manifest has no runnable platforms")
    return {"digest": digest, "platforms": platforms}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("digest", help="Published manifest digest; stdin must be that manifest")
    parser.add_argument("--single-platform", choices=sorted(PLATFORMS), help="Build platform for a non-index manifest")
    args = parser.parse_args()
    try:
        raw = sys.stdin.buffer.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise ValueError("manifest exceeds the byte limit")
        result = describe(args.digest, json.loads(raw), args.single_platform)
    except (ValueError, TypeError, KeyError, AttributeError) as error:
        parser.exit(1, f"image descriptor: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
