"""The native Annotator deployment used by the benchmark."""

from __future__ import annotations

import json
from dataclasses import dataclass

RISK_MARK = "shell-risk-review"
ANNOTATOR_NAME = "shellrisk-command-annotator"
TOOL_PARAMETERS = (
    '{ type = "object", additionalProperties = false, '
    'properties = { command = { type = "string" } }, required = ["command"] }'
)


@dataclass(frozen=True)
class ModelProfile:
    provider: str
    model: str
    url: str | None
    token_env: str | None
    timeout_ms: int
    max_concurrent: int


def _quoted(value: str) -> str:
    return json.dumps(value)


def _externals(profile: ModelProfile) -> str:
    lines = [
        "[externals]",
        f"timeout_ms = {profile.timeout_ms}",
        "review_timeout_ms = 600000",
        "max_body_bytes = 65536",
        "",
    ]
    lines.extend(
        [
            "[externals.llm]",
            f"provider = {_quoted(profile.provider)}",
            f"model = {_quoted(profile.model)}",
        ]
    )
    if profile.url:
        lines.append(f"url = {_quoted(profile.url)}")
    if profile.provider != "ollama":
        lines.append('token_env = "APPA_SHELLRISK_TOKEN"')
    lines.extend(
        [
            f"timeout_ms = {profile.timeout_ms}",
            f"max_concurrent = {profile.max_concurrent}",
            "",
        ]
    )
    return "\n".join(lines)


def annotation_policy(profile: ModelProfile, *, mark: str = RISK_MARK) -> str:
    return f"""[policy]
version = 1
trust_chain = ["trusted"]

[[policy.annotator]]
name = "{ANNOTATOR_NAME}"
builtin = "llm"
ranks = ["trusted"]
audiences = []
marks = ["{mark}"]
effects = []

[[policy.tool]]
name = "Bash"
description = "Runs one shell command and returns its output."
parameters = {TOOL_PARAMETERS}
annotator = "{ANNOTATOR_NAME}"

[[policy.authority]]
name = "shellrisk-attention-reviewer"
hint = "Reviews a command that the Annotator marked for ShellRisk review."

[policy.authority.permits]
attention = ["{mark}"]

{_externals(profile)}"""
