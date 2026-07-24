"""Tests for the FIDES labeling of the tool surface. Needs agent-framework
(installed as a dependency) but no network or API key: it inspects the tools'
declared policy and the security labels their results carry, which is where the
APPA->FIDES mapping actually lives."""

from __future__ import annotations

from pathlib import Path

import pytest

from corp_fides.tools import build_tools

_CORPUS = (Path(__file__).resolve().parent / ".." / ".." / "corporate-agent" / "data").resolve()

pytestmark = pytest.mark.skipif(not _CORPUS.is_dir(), reason="sibling corporate-agent/data corpus not present")


def _tools_by_name(tmp_path: Path):
    tools, _ = build_tools(_CORPUS, tmp_path)
    return {t.name: t for t in tools}


def test_thirteen_tools_with_expected_names(tmp_path: Path) -> None:
    tools = _tools_by_name(tmp_path)
    assert len(tools) == 13
    for system in ("hr", "finance", "task_tracker", "public_forum"):
        for verb in ("search", "read", "create"):
            assert f"{verb}_{system}" in tools
    assert "send_email" in tools


def test_send_email_is_the_gated_egress_sink(tmp_path: Path) -> None:
    props = _tools_by_name(tmp_path)["send_email"].additional_properties
    # Refuses a tainted context AND refuses writing private data outward — the
    # FIDES analogue of APPA's requires={trust=internal, audience includes $to}.
    assert props["accepts_untrusted"] is False
    assert props["max_allowed_confidentiality"] == "public"


def test_forum_reads_are_untrusted_sources(tmp_path: Path) -> None:
    props = _tools_by_name(tmp_path)["read_public_forum"].additional_properties
    assert props["source_integrity"] == "untrusted"
    # A pure source is safe to call even in a tainted context (cannot exfiltrate).
    assert props["accepts_untrusted"] is True


def test_hr_reads_are_trusted_but_private(tmp_path: Path) -> None:
    props = _tools_by_name(tmp_path)["read_hr"].additional_properties
    assert props["source_integrity"] == "trusted"


def test_hr_result_carries_private_label(tmp_path: Path) -> None:
    read_hr = _tools_by_name(tmp_path)["read_hr"]
    # Invoke the wrapped source directly and inspect the label it stamps.
    contents = read_hr.func("alice-chen.md")  # type: ignore[attr-defined]
    label = contents[0].additional_properties["security_label"]
    assert label == {"integrity": "trusted", "confidentiality": "private"}


def test_forum_result_carries_untrusted_label(tmp_path: Path) -> None:
    read_forum = _tools_by_name(tmp_path)["read_public_forum"]
    contents = read_forum.func("acme-forum-thread.md")  # type: ignore[attr-defined]
    label = contents[0].additional_properties["security_label"]
    assert label == {"integrity": "untrusted", "confidentiality": "public"}
