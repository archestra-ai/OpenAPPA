#!/usr/bin/env python3
"""Tag contract for OpenAPPA images and charts in appa-public.

Release images use an immutable v* tag. Rolling CI tags are sha-, pr-,
main, and latest. Chart versions stay SemVer without a v prefix and live
under charts/, so GAR prefix matching can garbage-collect CI images
without deleting releases or charts.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

GAR_HOST = "europe-west1-docker.pkg.dev"
GAR_PROJECT = "friendly-path-465518-r6"
GAR_REPOSITORY = "appa-public"
REGISTRY = f"{GAR_HOST}/{GAR_PROJECT}/{GAR_REPOSITORY}"
CHARTS_OCI = f"oci://{REGISTRY}/charts"

IMAGE_PACKAGES = (
    "appa-runtime",
    "appa-kagent-adk",
    "appa-kagent-adk-go",
    "appa-demo-tools",
    "appa-demo-mocks",
    "golang-adk",
)

RELEASE_VERSION = re.compile(
    r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-.][0-9A-Za-z.-]+)?$"
)
RELEASE_IMAGE_TAG = re.compile(
    r"^v[0-9]+\.[0-9]+\.[0-9]+(?:[-.][0-9A-Za-z.-]+)?$"
)
CI_SHA_TAG = re.compile(r"^sha-[0-9a-f]{7,40}$")
CI_PR_TAG = re.compile(r"^pr-[1-9][0-9]*$")
CI_MOVING_TAGS = frozenset({"main", "latest"})
FULL_SHA = re.compile(r"^[0-9a-f]{40}$")

DELETE_TAG_PREFIXES = ("sha-", "pr-", "main", "latest")
KEEP_TAG_PREFIXES = ("v",)
KEEP_RECENT = 10
MAX_AGE_SECONDS = 2592000
POLICY_PATH = Path(__file__).with_name("appa-gar-cleanup-policies.json")


def registry() -> str:
    return REGISTRY


def charts_oci() -> str:
    return CHARTS_OCI


def release_image_tag(version: str) -> str:
    if not RELEASE_VERSION.fullmatch(version):
        raise ValueError(f"not a release version: {version}")
    return f"v{version}"


def rolling_tags(
    *,
    event: str,
    sha: str,
    ref: str = "",
    pr_number: int | None = None,
) -> list[str]:
    if not FULL_SHA.fullmatch(sha):
        raise ValueError("sha must be a full commit")
    tags = [f"sha-{sha[:12]}"]
    if event == "pull_request":
        if pr_number is None or pr_number < 1:
            raise ValueError("pull_request requires a PR number")
        tags.append(f"pr-{pr_number}")
        return tags
    if ref == "refs/heads/main":
        tags.extend(["main", "latest"])
    return tags


def classify_tag(tag: str) -> str:
    if RELEASE_IMAGE_TAG.fullmatch(tag):
        return "release"
    if (
        CI_SHA_TAG.fullmatch(tag)
        or CI_PR_TAG.fullmatch(tag)
        or tag in CI_MOVING_TAGS
    ):
        return "ci"
    if RELEASE_VERSION.fullmatch(tag):
        return "chart"
    return "unknown"


def is_image_package(package: str) -> bool:
    return package in IMAGE_PACKAGES


def has_prefix(tag: str, prefixes: tuple[str, ...]) -> bool:
    return any(tag.startswith(prefix) for prefix in prefixes)


def deletion_candidates(
    versions: list[dict],
    *,
    now: int,
    max_age_seconds: int = MAX_AGE_SECONDS,
    keep_recent: int = KEEP_RECENT,
) -> list[str]:
    """Return digests GAR cleanup may delete.

    Mirrors the appa-public policies: keep any digest tagged v*, exclude
    charts/, keep the newest ``keep_recent`` versions of each image
    package, and delete remaining image versions whose tags are only
    rolling CI prefixes and that are older than ``max_age_seconds``.
    """
    newest: dict[str, list[tuple[int, str]]] = {}
    for version in versions:
        package = version["package"]
        if not is_image_package(package):
            continue
        newest.setdefault(package, []).append(
            (int(version["created"]), version["digest"])
        )
    protected_recent = set()
    for package, entries in newest.items():
        entries.sort(reverse=True)
        for _, digest in entries[:keep_recent]:
            protected_recent.add((package, digest))

    candidates = []
    for version in versions:
        package = version["package"]
        digest = version["digest"]
        tags = list(version["tags"])
        if not is_image_package(package):
            continue
        if any(has_prefix(tag, KEEP_TAG_PREFIXES) for tag in tags):
            continue
        if (package, digest) in protected_recent:
            continue
        if now - int(version["created"]) < max_age_seconds:
            continue
        if not tags or not all(
            has_prefix(tag, DELETE_TAG_PREFIXES) for tag in tags
        ):
            continue
        candidates.append(digest)
    return candidates


def load_policies() -> list[dict]:
    return json.loads(POLICY_PATH.read_text())


def _print_lines(values: list[str]) -> int:
    sys.stdout.write("\n".join(values) + "\n")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("registry")
    sub.add_parser("charts-oci")

    release = sub.add_parser("release-image")
    release.add_argument("--version", required=True)

    rolling = sub.add_parser("rolling")
    rolling.add_argument("--event", required=True)
    rolling.add_argument("--sha", required=True)
    rolling.add_argument("--ref", default="")
    rolling.add_argument("--pr", type=int, default=None)

    classify = sub.add_parser("classify")
    classify.add_argument("--tag", required=True)

    args = parser.parse_args(argv)
    if args.command == "registry":
        return _print_lines([registry()])
    if args.command == "charts-oci":
        return _print_lines([charts_oci()])
    if args.command == "release-image":
        return _print_lines([release_image_tag(args.version)])
    if args.command == "rolling":
        return _print_lines(
            rolling_tags(
                event=args.event,
                sha=args.sha,
                ref=args.ref,
                pr_number=args.pr,
            )
        )
    return _print_lines([classify_tag(args.tag)])


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ValueError as error:
        print(error, file=sys.stderr)
        sys.exit(1)
