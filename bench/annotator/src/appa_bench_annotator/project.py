"""One `appa runtime annotate` row as the four labels the gold set scores.

An annotation is a full contract; the gold set labels its two label dimensions only. A
row that refuses the call in a session — no answer, or one outside the mandate — has no
labels: it is scored as a stall on every label.
"""

from dataclasses import dataclass
from enum import StrEnum

LABELS = ("delta_audience", "delta_trust", "requires_audience", "requires_trusted")


class Direction(StrEnum):
    """Which way a wrong label errs: a leak lets a flow through, a stall stops one."""

    LEAK = "leak"
    STALL = "stall"


# Each label's values from the one that stops the most flows to the one that stops the fewest.
STRICTEST_FIRST = {
    "delta_audience": ("self", "internal", "public"),
    "delta_trust": ("suspicious", "trusted"),
    "requires_audience": ("public", "internal", "none"),
    "requires_trusted": ("true", "false"),
}


@dataclass(frozen=True)
class Refused:
    reason: str


def narrowest(audience: object) -> str:
    """The chain level an audience clause reads as; any reader or group is inside `internal`."""
    match audience:
        case None | "public":
            return "public"
        case list() if "self" in audience:
            return "self"
        case _:
            return "internal"


def labels_of(row: dict) -> dict[str, str] | Refused | None:
    """The labels of one answered call, `Refused`, or None for a call no Annotator judges."""
    match row["outcome"]:
        case "answer":
            answer = row["answer"]
            delta, requires = answer["delta"], answer["requires"]
            contains = requires.get("audience", {}).get("contains")
            return {
                "delta_audience": narrowest(delta.get("audience")),
                "delta_trust": "suspicious" if "trust" in delta else "trusted",
                "requires_audience": "none" if contains is None else narrowest(contains),
                "requires_trusted": "true" if "trust" in requires else "false",
            }
        case "outside_mandate" | "no_answer":
            return Refused(row["outcome"])
        case "static" | "unresolved":
            return None
        case other:
            raise ValueError(f"unexpected outcome {other!r}")


def direction(label: str, gold: str, answered: str) -> Direction:
    order = STRICTEST_FIRST[label]
    return Direction.STALL if order.index(answered) < order.index(gold) else Direction.LEAK
