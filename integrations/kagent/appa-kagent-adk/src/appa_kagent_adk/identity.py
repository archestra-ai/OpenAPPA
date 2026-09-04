"""Trajectory identity: the (root, child) pair of every event.

The plugin and the entrypoint gates must place every event of one run
in the same trajectory, so the classification lives here. A delegated
entry carries the caller's lineage headers in session state. kagent's
executor lands them before each run on every covered lane, and the
session's own id becomes the child id under the caller's root.

The classification reads the headers as they stand at each call. One
child session id can serve many parents in turn: kagent's Go
remote-agent tool sends every delegation of a parent pod into one
shared child context. So the same session id classifies under a
different root when a different parent delegates into it. A run pins
the pair it read when it opened, and every callback of that run reads
the pin. Headers that land mid-run cannot flip one run between two
trajectories.
"""

from __future__ import annotations

from typing import Any

_HEADERS_STATE_KEY = "headers"
_ROOT_HEADER = "x-kagent-root-context-id"
_PARENT_HEADER = "x-kagent-parent-context-id"

TrajectoryIds = tuple[str, str | None]


class SessionIdentity:
    """The classification, and the per-run pin the plugin's callbacks read."""

    def __init__(self) -> None:
        # Bookkeeping, not policy state: the pair each open run read
        # from its session state, by invocation id. The run's end
        # removes it. On google-adk 1.31.1 no callback fires after a
        # run aborts, so an aborted run leaves its entry behind.
        self._invocations: dict[str, TrajectoryIds] = {}

    def ids(self, session: Any) -> TrajectoryIds:
        """The (root_id, child_id) pair of the emitting scope, as the session state reads now.

        A delegated entry carries the caller's lineage headers: the root
        context id names the root trajectory, and the session's own id
        becomes the child id. A plain session is the root itself. The
        entrypoint gates classify here, from the state kagent's executor
        landed before the run.
        """
        state = getattr(session, "state", None) or {}
        headers = state.get(_HEADERS_STATE_KEY)
        root = None
        if isinstance(headers, dict):
            root = headers.get(_ROOT_HEADER) or headers.get(_PARENT_HEADER)
        delegated = isinstance(root, str) and root != "" and root != session.id
        return (root, session.id) if delegated else (session.id, None)

    def open_invocation(self, invocation_context: Any) -> TrajectoryIds:
        """Pin the run's pair from its session state. The first open of a run wins.

        The user-message callback and the before-run callback both open
        the run, and the pin they share is the one read first.
        """
        ids = self.ids(invocation_context.session)
        return self._invocations.setdefault(invocation_context.invocation_id, ids)

    def ids_for(self, context: Any) -> TrajectoryIds:
        """The pair of the run a callback context belongs to.

        The pin when a run open reached this identity, else the
        classification as the session state reads now.
        """
        pinned = self._invocations.get(context.invocation_id)
        if pinned is not None:
            return pinned
        return self.ids(context.session)

    def close_invocation(self, invocation_id: str) -> None:
        """Forget the run's pin; the next run classifies afresh."""
        self._invocations.pop(invocation_id, None)

    def is_fresh(self, session: Any) -> bool:
        """Whether no content has crossed this session yet.

        The kagent executor may append a state-only header event before
        the first user message, so an empty event list is too strict —
        fresh means no content-bearing event.
        """
        return not any(getattr(event, "content", None) is not None for event in session.events)
