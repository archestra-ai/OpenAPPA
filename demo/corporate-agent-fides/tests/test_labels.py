"""Tests for the FIDES labeling of the tool surface. Needs agent-framework
(installed as a dependency) but no network or API key: it inspects the tools'
declared policy — where the APPA->FIDES mapping actually lives — and, against
the live shared server, the security labels their results carry."""

from __future__ import annotations

import asyncio
from pathlib import Path

from corp_fides.systems import CorpSystemsClient
from corp_fides.tools import ALL_TOOL_NAMES, build_tools

# Declarations need no live server — no tool is invoked.
_OFFLINE_CLIENT: CorpSystemsClient = None  # type: ignore[assignment]


def _tools_by_name():
    return {t.name: t for t in build_tools(_OFFLINE_CLIENT, set(ALL_TOOL_NAMES))}


def test_thirteen_tools_with_expected_names() -> None:
    tools = _tools_by_name()
    assert len(tools) == 13
    for system in ("hr", "finance", "task_tracker", "public_forum"):
        for verb in ("search", "read", "create"):
            assert f"{verb}_{system}" in tools
    assert "send_email" in tools


def test_send_email_is_the_gated_egress_sink() -> None:
    props = _tools_by_name()["send_email"].additional_properties
    # Refuses a tainted context AND refuses writing private data outward — the
    # FIDES analogue of APPA's requires={trust=internal, audience includes $to}.
    assert props["accepts_untrusted"] is False
    assert props["max_allowed_confidentiality"] == "public"


def test_forum_reads_are_untrusted_sources() -> None:
    props = _tools_by_name()["read_public_forum"].additional_properties
    assert props["source_integrity"] == "untrusted"
    # A pure source is safe to call even in a tainted context (cannot exfiltrate).
    assert props["accepts_untrusted"] is True


def test_hr_reads_are_trusted_but_private() -> None:
    props = _tools_by_name()["read_hr"].additional_properties
    assert props["source_integrity"] == "trusted"


def _label_of(server_bin: Path, corpus: Path, sink: Path, name: str, *args: str) -> dict:
    async def run():
        async with CorpSystemsClient(corpus, sink, server_bin) as client:
            tools = {t.name: t for t in build_tools(client, set(await client.list_tool_names()))}
            contents = await tools[name].func(*args)  # type: ignore[attr-defined]
            return contents[0].additional_properties["security_label"]

    return asyncio.run(run())


def test_hr_result_carries_private_label(server_bin: Path, corpus: Path, sink: Path) -> None:
    label = _label_of(server_bin, corpus, sink, "read_hr", "alice-chen.md")
    assert label == {"integrity": "trusted", "confidentiality": "private"}


def test_forum_result_carries_untrusted_label(server_bin: Path, corpus: Path, sink: Path) -> None:
    label = _label_of(server_bin, corpus, sink, "read_public_forum", "acme-forum-thread.md")
    assert label == {"integrity": "untrusted", "confidentiality": "public"}


def test_error_text_carries_the_neutral_label(server_bin: Path, corpus: Path, sink: Path) -> None:
    # A not-found error is trusted framework text, not fetched content.
    label = _label_of(server_bin, corpus, sink, "read_public_forum", "missing.md")
    assert label == {"integrity": "trusted", "confidentiality": "public"}


def test_reduced_surface_builds_only_listed_tools() -> None:
    # A narrowed server (--systems hr,email) lists 4 tools; the wrappers must
    # match exactly — no dead tools advertised to the model.
    available = {"search_hr", "read_hr", "create_hr", "send_email"}
    tools = build_tools(_OFFLINE_CLIENT, available)
    assert {t.name for t in tools} == available
