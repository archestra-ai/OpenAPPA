"""The adapter wire: event construction and decision parsing.

One JSON object per callback crosses ``POST $APPA_RUNTIME_URL/hook``.
The ``appa-adapter-kagent`` codec in the runtime parses these events
and renders every answer as one decision envelope. This module owns
both shapes on the python side and imports no ADK code, so the wire
stays testable against the shared fixtures
(``integrations/kagent/fixtures/wire-events.jsonl``) without an agent
runtime.

Ids are the harness's own: ``root_id`` is the ADK session id of the
root trajectory (in a delegated child workload, the root id read from
the inbound call metadata), and ``child_id`` is the delegated child
scope's own id. The codec applies the ``kagent:`` prefix; this module
never does.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

_OUTCOME_STATUSES = ("success", "failure", "indeterminate")


def _event(kind: str, root_id: str, child_id: str | None, **fields: Any) -> dict[str, Any]:
    wire: dict[str, Any] = {"event": kind, "root_id": root_id}
    if child_id is not None:
        wire["child_id"] = child_id
    for name, value in fields.items():
        if value is not None:
            wire[name] = value
    return wire


def ping() -> dict[str, Any]:
    """The liveness probe: parses to no event, answers 200 ``{}``."""
    return {"event": "ping"}


def session_start(root_id: str) -> dict[str, Any]:
    return {"event": "session_start", "root_id": root_id}


def prompt(root_id: str, text: str, child_id: str | None = None) -> dict[str, Any]:
    return _event("prompt", root_id, child_id, text=text)


def turn_end(root_id: str, child_id: str | None = None) -> dict[str, Any]:
    return _event("turn_end", root_id, child_id)


RESERVED_TOOL = "execute_remedy_plan"
"""The engine's remedy-execution tool, as it crosses the wire."""


def tool_call(
    root_id: str,
    tool: str,
    arguments: dict[str, Any],
    spawn: bool,
    child_id: str | None = None,
    ruling: str | None = None,
) -> dict[str, Any]:
    """A proposed call. ``ruling`` (``approve`` or ``deny``) rides only the
    control call whose offer a person ruled on through kagent's own
    confirmation; the runtime spends it as the human authority's answer."""
    return _event("tool_call", root_id, child_id, tool=tool, arguments=arguments, spawn=spawn, ruling=ruling)


def tool_result(
    root_id: str, tool: str, arguments: dict[str, Any], outcome: dict[str, Any], child_id: str | None = None
) -> dict[str, Any]:
    return _event("tool_result", root_id, child_id, tool=tool, arguments=arguments, outcome=outcome)


def spawn_result(
    root_id: str,
    tool: str,
    arguments: dict[str, Any],
    outcome: dict[str, Any],
    spawned_id: str | None = None,
    value: str | None = None,
    child_id: str | None = None,
) -> dict[str, Any]:
    return _event(
        "spawn_result",
        root_id,
        child_id,
        tool=tool,
        arguments=arguments,
        outcome=outcome,
        spawned_id=spawned_id,
        value=value,
    )


def child_start(root_id: str, child_id: str, spawn_binding: str | None = None) -> dict[str, Any]:
    wire: dict[str, Any] = {"event": "child_start", "root_id": root_id, "child_id": child_id}
    if spawn_binding is not None:
        wire["spawn_binding"] = spawn_binding
    return wire


def child_end(root_id: str, child_id: str, value: str | None = None) -> dict[str, Any]:
    """The child's stop, carrying the value it returns to its parent.

    An absent ``value`` is a child that returns nothing, and the codec
    reads an empty string the same way."""
    wire: dict[str, Any] = {"event": "child_end", "root_id": root_id, "child_id": child_id}
    if value is not None:
        wire["value"] = value
    return wire


def success(body: Any) -> dict[str, Any]:
    """A success outcome carrying the tool response as spelled."""
    return {"status": "success", "body": body}


def failure(message: str) -> dict[str, Any]:
    return {"status": "failure", "message": message}


def indeterminate() -> dict[str, Any]:
    return {"status": "indeterminate"}


class WireError(Exception):
    """The runtime's answer left the decision contract. Fail closed."""


RETURN_AS_SPOKEN = "as_spoken"
"""The route of a return that crosses as the child spoke it."""

RETURN_SANITIZED = "sanitized"
"""The route of a return a registered sanitizer derives."""


@dataclass(frozen=True)
class Offer:
    """One remedy a ``deny_call`` offers, for the plugin that routes one
    itself rather than through the model's control call.

    ``offer_id`` is the id ``execute_remedy_plan`` takes. ``returns`` is
    ``None`` on an offer that declares no child return, ``as_spoken``
    where the return crosses as the child spoke it, and ``sanitized``
    where a sanitizer derives it. ``sanitizer`` names that sanitizer and
    rides the ``sanitized`` route only."""

    offer_id: str
    returns: str | None = None
    sanitizer: str | None = None


@dataclass(frozen=True)
class Decision:
    """One parsed decision envelope.

    ``kind`` is the wire spelling (``ack``, ``allow_call``,
    ``pass_control``, ``deny_call``, ``block``, ``replace_output``,
    ``child_return``, ``context``, ``refuse``); the payload field, where
    the kind carries one, lands in the matching attribute.
    """

    kind: str
    feedback: str | None = None
    reason: str | None = None
    output: str | None = None
    value: str | None = None
    text: str | None = None
    """On a ``context``: what the harness hands the actor the event
    names — at a child's start, the return contract it works under."""
    detail: str | None = None
    spawn_binding: str | None = None
    review: tuple[tuple[str, str], ...] = ()
    """On a ``deny_call``: the offers whose plans consult a human
    authority, as ``(offer_id, text)`` — the review as the person reads
    it, which the plugin shows through kagent's confirmation."""
    offers: tuple[Offer, ...] = ()
    """On a ``deny_call``: every remedy the block offers, in the order
    the feedback lists them."""


_DECISION_PAYLOADS: dict[str, tuple[str, ...]] = {
    "ack": (),
    "allow_call": (),
    "pass_control": (),
    "deny_call": ("feedback",),
    "block": ("reason",),
    "replace_output": ("output",),
    "child_return": ("value",),
    "context": ("text",),
    "refuse": ("detail",),
}


def parse_decision(body: bytes | str) -> Decision:
    """Parse one decision envelope; anything else is a `WireError`.

    The plugin enforces decisions mechanically, so an answer outside
    the contract must block the gated action rather than pass it — the
    caller treats `WireError` exactly like a transport failure.
    """
    try:
        parsed = json.loads(body)
    except ValueError as error:
        raise WireError(f"unreadable decision envelope: {error}") from error
    if not isinstance(parsed, dict):
        raise WireError("the decision envelope is not an object")
    kind = parsed.get("decision")
    if not isinstance(kind, str) or kind not in _DECISION_PAYLOADS:
        raise WireError(f"a decision kind outside the wire: {kind!r}")
    fields: dict[str, str] = {}
    for name in _DECISION_PAYLOADS[kind]:
        payload = parsed.get(name)
        if not isinstance(payload, str):
            raise WireError(f"a {kind} decision without its {name}")
        fields[name] = payload
    if kind == "allow_call":
        binding = parsed.get("spawn_binding")
        if binding is not None:
            if not isinstance(binding, str):
                raise WireError("a spawn binding that is not a string")
            fields["spawn_binding"] = binding
    review: tuple[tuple[str, str], ...] = ()
    offers: tuple[Offer, ...] = ()
    if kind == "deny_call":
        review = _parse_review(parsed.get("review"))
        offers = _parse_offers(parsed.get("offers"))
    return Decision(kind=kind, review=review, offers=offers, **fields)


def _parse_review(raw: Any) -> tuple[tuple[str, str], ...]:
    """The deny's review entries; absent is none, and any other shape is a
    `WireError` — a review the plugin cannot read would leave a person
    unasked, which is fail-open."""
    if raw is None:
        return ()
    if not isinstance(raw, list):
        raise WireError("a deny_call review that is not a list")
    entries = []
    for entry in raw:
        offer = entry.get("offer_id") if isinstance(entry, dict) else None
        text = entry.get("text") if isinstance(entry, dict) else None
        if not isinstance(offer, str) or not isinstance(text, str):
            raise WireError("a deny_call review entry without its offer_id and text")
        entries.append((offer, text))
    return tuple(entries)


def _parse_offers(raw: Any) -> tuple[Offer, ...]:
    """The deny's offers; absent is none, and any other shape is a
    `WireError` — an offer the plugin cannot read would leave a route
    unrouted, which is fail-open."""
    if raw is None:
        return ()
    if not isinstance(raw, list):
        raise WireError("a deny_call offers list that is not a list")
    offers = []
    for entry in raw:
        offer = entry.get("offer_id") if isinstance(entry, dict) else None
        if not isinstance(offer, str):
            raise WireError("a deny_call offer without its offer_id")
        offers.append(Offer(offer_id=offer, **_parse_offer_return(entry.get("returns"))))
    return tuple(offers)


def _parse_offer_return(raw: Any) -> dict[str, str]:
    """One offer's return route, as the `Offer` fields spell it. Absent
    is an offer that declares no child return."""
    if raw is None:
        return {}
    if raw == RETURN_AS_SPOKEN:
        return {"returns": RETURN_AS_SPOKEN}
    sanitizer = raw.get("sanitizer") if isinstance(raw, dict) else None
    if not isinstance(sanitizer, str):
        raise WireError(f"a deny_call offer with a return route outside the wire: {raw!r}")
    return {"returns": RETURN_SANITIZED, "sanitizer": sanitizer}
