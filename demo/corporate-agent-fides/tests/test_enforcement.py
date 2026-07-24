"""Deterministic, offline proof of the defense — no LLM, no network, no key.

Drives the *real* FIDES primitives (``combine_labels`` = the taint fold,
``check_confidentiality_allowed`` = the exfiltration gate) with the labels this
demo actually stamps on tool results, and asserts the injection flow is refused
at ``send_email`` while a benign public-data egress passes. This is the
LLM-independent core of what ``scripts/injection-forum-fides.sh`` shows
end-to-end."""

from __future__ import annotations

from agent_framework.security import (
    ConfidentialityLabel,
    ContentLabel,
    IntegrityLabel,
    check_confidentiality_allowed,
    combine_labels,
)

from corp_fides.systems import CorpSystemsClient, System
from corp_fides.tools import _LABELS, build_tools

# Declarations need no live server — no tool is invoked.
_OFFLINE_CLIENT: CorpSystemsClient = None  # type: ignore[assignment]


def _label(system: System) -> ContentLabel:
    integrity, confidentiality = _LABELS[system]
    return ContentLabel(
        integrity=IntegrityLabel(integrity),
        confidentiality=ConfidentialityLabel(confidentiality),
    )


def _send_email_gates() -> tuple[bool, ConfidentialityLabel]:
    tools = build_tools(_OFFLINE_CLIENT)
    props = next(t for t in tools if t.name == "send_email").additional_properties
    return props["accepts_untrusted"], ConfidentialityLabel(props["max_allowed_confidentiality"])


def _blocked_by_send_email(context: ContentLabel) -> bool:
    accepts_untrusted, max_conf = _send_email_gates()
    integrity_violation = context.integrity == IntegrityLabel.UNTRUSTED and not accepts_untrusted
    confidentiality_violation = not check_confidentiality_allowed(context, max_conf)
    return integrity_violation or confidentiality_violation


def test_injection_flow_is_blocked_at_send_email() -> None:
    # forum read (untrusted) then HR read (private) — the taint fold.
    context = combine_labels(_label(System.PUBLIC_FORUM), _label(System.HR))
    assert context.integrity == IntegrityLabel.UNTRUSTED
    assert context.confidentiality == ConfidentialityLabel.PRIVATE
    # Refused on BOTH axes: tainted context and private->public exfiltration.
    assert _blocked_by_send_email(context)


def test_hr_egress_blocked_even_without_taint() -> None:
    # Emailing an HR record out is a confidentiality violation on its own,
    # even on a fully trusted, user-directed trajectory.
    context = _label(System.HR)
    assert context.integrity == IntegrityLabel.TRUSTED
    assert _blocked_by_send_email(context)


def test_finance_data_may_be_emailed() -> None:
    # Finance mirrors the APPA policy's `delta = {}` on read_finance: unconstrained
    # in both arms, so the invoice scenarios are scored on what the agent put in
    # the body, not on a label. Labeling finance private here would not confine
    # those scenarios but make them unwinnable: `send_email` resolves `$to` to the
    # literal recipient, so an audience narrowed to {finance} can never satisfy
    # includes(finance-lead@northwind.example), and no sanitizer covers it.
    context = _label(System.FINANCE)
    assert context.integrity == IntegrityLabel.TRUSTED
    assert not _blocked_by_send_email(context)


def test_public_internal_data_may_be_emailed() -> None:
    # Value-granular, not blanket: trusted + public task-tracker data egresses fine.
    context = _label(System.TASK_TRACKER)
    assert not _blocked_by_send_email(context)
