"""The corporate tool surface, labeled for FIDES.

Thirteen tools — ``search_``/``read_``/``create_`` for each of ``hr``,
``finance``, ``task_tracker``, ``public_forum``, plus the outbound
``send_email`` sink — the exact surface of the shared ``corp-systems-mcp``
server. Each is a native Agent Framework tool that *forwards* the call over
MCP (:class:`~.systems.CorpSystemsClient`), so the semantics are literally the
sibling Rust server's. What lives here is the *labeling*: every tool result
carries a FIDES ``security_label`` (the integrity/confidentiality analogue of
OpenAPPA's trust/audience), and the egress sink declares the policy FIDES
enforces before it runs.

The mapping from the sibling APPA policy (``bench-corp/policies/appa.toml``)
to FIDES labels:

    APPA trust  (suspicious | internal)  <->  FIDES integrity     (untrusted | trusted)
    APPA audience(hr-only | unrestricted) <->  FIDES confidentiality(private | public)

so:

    public_forum -> integrity=untrusted (the taint), confidentiality=public
    hr           -> integrity=trusted,   confidentiality=private   (the secret)
    finance      -> integrity=trusted,   confidentiality=public    (unconstrained, like the APPA policy's `delta = {}`)
    task_tracker -> integrity=trusted,   confidentiality=public
    send_email   -> accepts_untrusted=False, max_allowed_confidentiality=public

Reads/searches are pure sources (``accepts_untrusted=True``): safe to call even
in a tainted context because they cannot exfiltrate. ``send_email`` is the only
egress sink, so it is the only tool that refuses an untrusted or over-private
context — exactly the single gated flow the APPA demo guards.

Where the mapping stops
-----------------------

The trust/integrity row above is a true isomorphism: two ranks either side, and
``accepts_untrusted=False`` is ``requires.trust = "internal"``. The audience row
is not. It holds only because of a property of *today's* APPA policy, and it
will stop holding the moment that property changes.

FIDES confidentiality is an ordinal chain — ``PUBLIC < PRIVATE <
USER_IDENTITY`` — enforced as a numeric ceiling against a sink's
``max_allowed_confidentiality`` (``agent_framework/security.py``). Despite its
name and its ``metadata={"user_id": ...}``, ``USER_IDENTITY`` means "more
secret than private", not "for this reader": no policy path in FIDES reads the
call's arguments, so no label can distinguish one recipient from another.

APPA's audience is a reader *set*, folded by intersection, and the sink's
requirement names the recipient: ``audience = { includes = ["$to"] }``
resolves ``$to`` to the literal address at dispatch. That is a strictly richer
question than a ceiling.

The two agree today only because ``bench-corp/policies/appa.toml`` mints
exactly one reader token, ``hr``, and never an address. No audience set can
contain ``finance-lead@northwind.example``, so ``includes($to)`` degenerates
into "is this trajectory still ``Public``?" — which is precisely
``max_allowed_confidentiality=public``. The value-granular machinery is
declared but not exercised, so the transcription lands.

Give any tool's ``delta`` a real recipient set and the transcription breaks
irreparably. A trajectory carrying data readable by ``finance-lead@`` but not
``all@`` has no image in an ordinal chain: ``public`` releases to both,
``private`` and ``user_identity`` release to neither. **If that day comes, the
divergence is the measurement, not a defect to repair.** #82 aligned the finance
label because the two policies *could* match there and had drifted apart by
accident; do not extend that reflex to a constraint FIDES cannot express, or
re-aligning the labels will silently delete the result.
"""

from __future__ import annotations

from collections.abc import Collection
from typing import Any

from agent_framework import Content, tool

from .systems import CorpSystemsClient, System

# Per-system output label (integrity, confidentiality). See module docstring.
_LABELS: dict[System, tuple[str, str]] = {
    System.HR: ("trusted", "private"),
    System.FINANCE: ("trusted", "public"),
    System.TASK_TRACKER: ("trusted", "public"),
    System.PUBLIC_FORUM: ("untrusted", "public"),
}

# A neutral receipt (creation acks, send confirmations, error text from the
# framework rather than fetched content) carries nothing that should taint or
# restrict the trajectory — the FIDES analogue of APPA's `delta = {}`.
_NEUTRAL = ("trusted", "public")

# The full 13-tool surface of the shared server: the default when no live
# listing narrows `available` (offline tests, docs).
ALL_TOOL_NAMES: frozenset[str] = frozenset(
    f"{verb}_{system.dir_name}" for system in _LABELS for verb in ("search", "read", "create")
) | {"send_email"}


def _labeled(text: str, label: tuple[str, str]) -> Content:
    integrity, confidentiality = label
    return Content.from_text(
        text,
        additional_properties={
            "security_label": {"integrity": integrity, "confidentiality": confidentiality}
        },
    )


def build_tools(client: CorpSystemsClient, available: Collection[str] | None = None) -> list[Any]:
    """Construct the FIDES-labeled tools over a systems client, one per name in
    ``available``.

    ``available`` is the live server's tool listing (``list_tool_names``): when
    the server runs with a narrowed ``--systems`` / ``CORP_ENABLED_SYSTEMS``
    surface, only those tools are built, so the model is never shown a tool the
    server would refuse. ``None`` (offline callers) means the full surface.

    ``client`` must be entered (its async context open) by the time a tool is
    invoked; building the tools — and inspecting their declarations — needs no
    live server."""
    if available is None:
        available = ALL_TOOL_NAMES

    async def forward(name: str, arguments: dict[str, Any], label: tuple[str, str]) -> list[Content]:
        text, is_error = await client.call(name, arguments)
        # Errors are trusted framework text, not fetched content.
        return [_labeled(text, _NEUTRAL if is_error else label)]

    def make_search(system: System):
        label = _LABELS[system]

        async def _search(query: str) -> list[Content]:
            return await forward(f"search_{system.dir_name}", {"query": query}, label)

        return _search

    def make_read(system: System):
        label = _LABELS[system]

        async def _read(file: str) -> list[Content]:
            return await forward(f"read_{system.dir_name}", {"file": file}, label)

        return _read

    def make_create(system: System):
        async def _create(file: str, content: str) -> list[Content]:
            return await forward(f"create_{system.dir_name}", {"file": file, "content": content}, _NEUTRAL)

        return _create

    tools: list[Any] = []

    def add(name: str, fn: Any, description: str, props: dict[str, Any]) -> None:
        if name in available:
            tools.append(tool(fn, name=name, description=description, additional_properties=props))

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
        add(
            f"search_{system.dir_name}",
            make_search(system),
            f"Search {blurb} by keyword.",
            dict(source_props),
        )
        add(
            f"read_{system.dir_name}",
            make_read(system),
            f"Read a file from {blurb} by exact file name.",
            dict(source_props),
        )
        # create_* are internal writes, not egress sinks in this demo; allow
        # them in any context (neutral), mirroring APPA's `delta = {}`.
        add(
            f"create_{system.dir_name}",
            make_create(system),
            f"Create a new file in {blurb}.",
            {"accepts_untrusted": True},
        )

    async def send_email(to: str, subject: str, body: str) -> list[Content]:
        return await forward("send_email", {"to": to, "subject": subject, "body": body}, _NEUTRAL)

    # The one egress sink. FIDES enforces BOTH gates before the body runs:
    #   accepts_untrusted=False          -> refuse a tainted (untrusted) context
    #   max_allowed_confidentiality=public -> refuse writing PRIVATE data outward
    # Together they are the FIDES analogue of APPA's
    #   requires = { trust = "internal", audience = { includes = ["$to"] } }.
    add(
        "send_email",
        send_email,
        "Send an outbound email. Delivers the message to the given recipient.",
        {
            "accepts_untrusted": False,
            "max_allowed_confidentiality": "public",
        },
    )

    return tools
