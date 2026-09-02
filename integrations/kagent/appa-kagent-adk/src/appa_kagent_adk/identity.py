"""Trajectory identity: one classification per ADK session.

The plugin and the entrypoint gates must place every event of one
session in the same trajectory, so the classification lives here and
each session's ids pin at first sight. A delegated entry carries the
caller's lineage headers in session state — kagent's remote-agent tool
sends them on every covered lane — and the session's own id becomes
the child id under the caller's root.
"""

from __future__ import annotations

from typing import Any

_HEADERS_STATE_KEY = "headers"
_ROOT_HEADER = "x-kagent-root-context-id"
_PARENT_HEADER = "x-kagent-parent-context-id"


class SessionIdentity:
    def __init__(self) -> None:
        # Bookkeeping, not policy state: a lane that lands the lineage
        # headers after the first event must not flip one session
        # between two trajectories.
        self._pinned: dict[str, tuple[str, str | None]] = {}

    def ids(self, session: Any) -> tuple[str, str | None]:
        """The (root_id, child_id) pair of the emitting scope."""
        cached = self._pinned.get(session.id)
        if cached is not None:
            return cached
        state = getattr(session, "state", None) or {}
        headers = state.get(_HEADERS_STATE_KEY)
        root = None
        if isinstance(headers, dict):
            root = headers.get(_ROOT_HEADER) or headers.get(_PARENT_HEADER)
        delegated = isinstance(root, str) and root != "" and root != session.id
        ids = (root, session.id) if delegated else (session.id, None)
        self._pinned[session.id] = ids
        return ids

    def is_fresh(self, session: Any) -> bool:
        """Whether no content has crossed this session yet.

        The kagent executor may append a state-only header event before
        the first user message, so an empty event list is too strict —
        fresh means no content-bearing event.
        """
        return not any(getattr(event, "content", None) is not None for event in session.events)
