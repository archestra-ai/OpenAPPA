"""Deterministic, offline proof of the defense — no LLM, no network, no key.

Drives the *real* FIDES primitives (``combine_labels`` = the taint fold,
``check_confidentiality_allowed`` = the exfiltration gate) with the labels this
demo actually stamps on tool results, and asserts the injection flow is refused
at ``send_email`` while a benign public-data egress passes. This is the
LLM-independent core of what ``scripts/injection-forum-fides.sh`` shows
end-to-end."""

from __future__ import annotations

from pathlib import Path

import pytest

from agent_framework.security import (
    ConfidentialityLabel,
    ContentLabel,
    IntegrityLabel,
    check_confidentiality_allowed,
    combine_labels,
)

from corp_fides.tools import _LABELS, build_tools
from corp_fides.systems import System


def _label(system: System) -> ContentLabel:
    integrity, confidentiality = _LABELS[system]
    return ContentLabel(
        integrity=IntegrityLabel(integrity),
        confidentiality=ConfidentialityLabel(confidentiality),
    )


def _send_email_gates(tmp_path: Path) -> tuple[bool, ConfidentialityLabel]:
    tools, _ = build_tools(tmp_path, tmp_path)
    props = next(t for t in tools if t.name == "send_email").additional_properties
    return props["accepts_untrusted"], ConfidentialityLabel(props["max_allowed_confidentiality"])


def _blocked_by_send_email(context: ContentLabel, tmp_path: Path) -> bool:
    accepts_untrusted, max_conf = _send_email_gates(tmp_path)
    integrity_violation = context.integrity == IntegrityLabel.UNTRUSTED and not accepts_untrusted
    confidentiality_violation = not check_confidentiality_allowed(context, max_conf)
    return integrity_violation or confidentiality_violation


def test_injection_flow_is_blocked_at_send_email(tmp_path: Path) -> None:
    # forum read (untrusted) then HR read (private) — the taint fold.
    context = combine_labels(_label(System.PUBLIC_FORUM), _label(System.HR))
    assert context.integrity == IntegrityLabel.UNTRUSTED
    assert context.confidentiality == ConfidentialityLabel.PRIVATE
    # Refused on BOTH axes: tainted context and private->public exfiltration.
    assert _blocked_by_send_email(context, tmp_path)


def test_hr_egress_blocked_even_without_taint(tmp_path: Path) -> None:
    # Emailing an HR record out is a confidentiality violation on its own,
    # even on a fully trusted, user-directed trajectory.
    context = _label(System.HR)
    assert context.integrity == IntegrityLabel.TRUSTED
    assert _blocked_by_send_email(context, tmp_path)


def test_public_internal_data_may_be_emailed(tmp_path: Path) -> None:
    # Value-granular, not blanket: trusted + public finance data egresses fine.
    context = _label(System.FINANCE)
    assert not _blocked_by_send_email(context, tmp_path)
