"""The corporate tool surface, labeled for FIDES.

Thirteen tools — ``search_``/``read_``/``create_`` for each of ``hr``,
``finance``, ``task_tracker``, ``public_forum``, plus the outbound
``send_email`` sink — mirroring the sibling Rust demo's MCP server. What is new
here is the *labeling*: every tool result carries a FIDES ``security_label``
(the integrity/confidentiality analogue of OpenAPPA's trust/audience), and the
egress sink declares the policy that FIDES enforces before it runs.

The mapping from the sibling APPA policy (``appa-policy.toml``) to FIDES labels:

    APPA trust  (suspicious | internal)  <->  FIDES integrity     (untrusted | trusted)
    APPA audience(hr-only | unrestricted) <->  FIDES confidentiality(private | public)

so:

    public_forum -> integrity=untrusted (the taint), confidentiality=public
    hr           -> integrity=trusted,   confidentiality=private   (the secret)
    finance      -> integrity=trusted,   confidentiality=public    (internal, egress-able)
    task_tracker -> integrity=trusted,   confidentiality=public
    send_email   -> accepts_untrusted=False, max_allowed_confidentiality=public

Reads/searches are pure sources (``accepts_untrusted=True``): safe to call even
in a tainted context because they cannot exfiltrate. ``send_email`` is the only
egress sink, so it is the only tool that refuses an untrusted or over-private
context — exactly the single gated flow the APPA demo guards.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Callable

from agent_framework import Content, tool

from . import systems
from .systems import System

# Per-system output label (integrity, confidentiality). See module docstring.
_LABELS: dict[System, tuple[str, str]] = {
    System.HR: ("trusted", "private"),
    System.FINANCE: ("trusted", "public"),
    System.TASK_TRACKER: ("trusted", "public"),
    System.PUBLIC_FORUM: ("untrusted", "public"),
}

# A neutral receipt (creation acks, send confirmations) carries nothing that
# should taint or restrict the trajectory — the FIDES analogue of APPA's
# `delta = {}`.
_NEUTRAL = ("trusted", "public")


def _labeled(text: str, label: tuple[str, str]) -> Content:
    integrity, confidentiality = label
    return Content.from_text(
        text,
        additional_properties={
            "security_label": {"integrity": integrity, "confidentiality": confidentiality}
        },
    )


def _render_search(system: System, query: str, hits: list[systems.Hit]) -> str:
    if not hits:
        return f"no matches for {query!r} in the {system.dir_name} system"
    lines = [f"{len(hits)} match(es) in the {system.dir_name} system:"]
    lines += [f"- {h.file} — {h.snippet}" for h in hits]
    return "\n".join(lines)


def build_tools(corpus_root: Path, sink_root: Path) -> tuple[list[Any], Callable[[], None]]:
    """Construct the thirteen FIDES-labeled tools bound to a corpus + sink.

    Returns ``(tools, sink_is_empty_checker)`` — the second value is unused by
    the agent but handy for tests. Reads come from ``corpus_root`` (the shared,
    read-only corpus); ``send_email`` writes to ``sink_root`` (this demo's own
    observable folder)."""

    def make_search(system: System):
        label = _LABELS[system]

        def _search(query: str) -> list[Content]:
            hits = systems.search(corpus_root, system, query)
            return [_labeled(_render_search(system, query, hits), label)]

        return _search

    def make_read(system: System):
        label = _LABELS[system]

        def _read(file: str) -> list[Content]:
            try:
                body = systems.read(corpus_root, system, file)
            except (FileNotFoundError, systems.NameError_) as exc:
                # Errors are trusted framework text, not fetched content.
                return [_labeled(str(exc), _NEUTRAL)]
            return [_labeled(body, label)]

        return _read

    def make_create(system: System):
        def _create(file: str, content: str) -> list[Content]:
            try:
                systems.create(corpus_root, system, file, content)
            except (FileExistsError, systems.NameError_) as exc:
                return [_labeled(str(exc), _NEUTRAL)]
            return [_labeled(f"created {file} in the {system.dir_name} system", _NEUTRAL)]

        return _create

    tools: list[Any] = []
    descriptions = {
        System.HR: "the HR system (employee records, org roster, policies)",
        System.FINANCE: "the finance system (invoices, budgets, expense policy)",
        System.TASK_TRACKER: "the task tracker (tickets, tasks, assignments)",
        System.PUBLIC_FORUM: "the public forum (external, untrusted user-posted content)",
    }
    # A pure data source is safe to invoke in a tainted context (it cannot
    # exfiltrate), so reads/searches opt in via accepts_untrusted=True and
    # declare the integrity of the data they fetch via source_integrity.
    for system, blurb in descriptions.items():
        integrity = _LABELS[system][0]
        source_props = {"source_integrity": integrity, "accepts_untrusted": True}
        tools.append(
            tool(
                make_search(system),
                name=f"search_{system.dir_name}",
                description=f"Search {blurb} by keyword.",
                additional_properties=dict(source_props),
            )
        )
        tools.append(
            tool(
                make_read(system),
                name=f"read_{system.dir_name}",
                description=f"Read a file from {blurb} by exact file name.",
                additional_properties=dict(source_props),
            )
        )
        # create_* are internal writes, not egress sinks in this demo; allow
        # them in any context (neutral), mirroring APPA's `delta = {}`.
        tools.append(
            tool(
                make_create(system),
                name=f"create_{system.dir_name}",
                description=f"Create a new file in {blurb}.",
                additional_properties={"accepts_untrusted": True},
            )
        )

    def send_email(to: str, subject: str, body: str) -> list[Content]:
        file = systems.send_email(sink_root, to, subject, body)
        return [_labeled(f"email sent to {to} (subject: {subject!r}); archived as {file}", _NEUTRAL)]

    # The one egress sink. FIDES enforces BOTH gates before the body runs:
    #   accepts_untrusted=False          -> refuse a tainted (untrusted) context
    #   max_allowed_confidentiality=public -> refuse writing PRIVATE data outward
    # Together they are the FIDES analogue of APPA's
    #   requires = { trust = "internal", audience = { includes = ["$to"] } }.
    tools.append(
        tool(
            send_email,
            name="send_email",
            description="Send an outbound email. Delivers the message to the given recipient.",
            additional_properties={
                "accepts_untrusted": False,
                "max_allowed_confidentiality": "public",
            },
        )
    )

    def sink_is_empty() -> None:  # pragma: no cover - test convenience only
        return None

    return tools, sink_is_empty
