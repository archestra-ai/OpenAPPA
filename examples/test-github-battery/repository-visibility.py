#!/usr/bin/env python3
"""Annotate a GitHub MCP call from the repository's GitHub visibility."""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Callable
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

MAX_INPUT_BYTES = 64 * 1024
GITHUB_API = "https://api.github.com"
TOKEN_ENV = "APPA_PROVIDER_GITHUB_TOKEN"


class ConsultError(Exception):
    """The Annotator cannot safely answer the consult."""


def repository_from_consult(consult: object) -> tuple[str, str]:
    if not isinstance(consult, dict):
        raise ConsultError("consult must be a JSON object")
    if consult.get("version") != 1 or consult.get("kind") != "annotation":
        raise ConsultError("unsupported consult")
    if consult.get("name") != "github.repository-visibility":
        raise ConsultError("unexpected annotator name")

    artifact = consult.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    if not isinstance(arguments, dict):
        raise ConsultError("tool arguments are missing")

    owner = arguments.get("owner")
    repo = arguments.get("repo")
    if not isinstance(owner, str) or not owner.strip():
        raise ConsultError("owner must be a non-empty string")
    if not isinstance(repo, str) or not repo.strip():
        raise ConsultError("repo must be a non-empty string")
    return owner.strip(), repo.strip()


def repository_is_private(
    owner: str,
    repo: str,
    token: str,
    opener: Callable[..., Any] = urlopen,
) -> bool:
    url = f"{GITHUB_API}/repos/{quote(owner, safe='')}/{quote(repo, safe='')}"
    request = Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "OpenAPPA-repository-visibility-annotator",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    try:
        with opener(request, timeout=10) as response:
            body = response.read(MAX_INPUT_BYTES + 1)
    except (HTTPError, URLError, TimeoutError, OSError) as error:
        raise ConsultError("GitHub repository lookup failed") from error

    if len(body) > MAX_INPUT_BYTES:
        raise ConsultError("GitHub response is too large")
    try:
        payload = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ConsultError("GitHub returned invalid JSON") from error
    private = payload.get("private") if isinstance(payload, dict) else None
    if not isinstance(private, bool):
        raise ConsultError("GitHub response has no repository visibility")
    return private


def annotation(private: bool) -> dict[str, object]:
    audience: str | list[str] = ["github:internal"] if private else "public"
    return {
        "version": 1,
        "answer": {
            "delta": {"trust": "suspicious", "audience": audience},
            "requires": {"history": [], "attention": []},
            "emits": [],
        },
    }


def main() -> int:
    try:
        raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
        if len(raw) > MAX_INPUT_BYTES:
            raise ConsultError("consult is too large")
        consult = json.loads(raw)
        owner, repo = repository_from_consult(consult)
        token = os.environ.get(TOKEN_ENV, "")
        if not token:
            raise ConsultError(f"{TOKEN_ENV} is not set")
        answer = annotation(repository_is_private(owner, repo, token))
    except (ConsultError, UnicodeDecodeError, json.JSONDecodeError) as error:
        print(f"repository-visibility: {error}", file=sys.stderr)
        return 1

    json.dump(answer, sys.stdout, separators=(",", ":"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
