"""``AppaPluginKagent`` — the google-adk plugin of the OpenAPPA images.

The plugin maps each gated ADK callback onto one wire event, posts it
to ``$APPA_RUNTIME_URL/hook``, and enforces the answered decision. It
holds no policy state: every answer comes from ``appa-runtime``, and
the plugin's only judgment is mechanical enforcement.

Fail-closed contract. A gated callback raises on a transport failure,
a non-2xx answer, or an answer outside the decision contract — ADK
wraps the raise and aborts the invocation. The model and emission
callbacks feed no event but still probe the ``/hook`` channel with a
``ping`` and raise when it is down. Turn ends are the one exception:
blocking a finished turn wedges the harness, so ``turn_end`` posts are
best-effort and never raise.

One plugin codebase serves both locked ADK majors. Every callback uses
the exact keyword names the plugin manager passes, and the two
error-turn callbacks of google-adk 2.8.0 are defined unconditionally —
the 1.31.1 manager simply never calls them.

A delegated child opens once per (root, child) pair, not once per
session. kagent's Go remote-agent tool sends every delegation of one
parent pod into one child context id, so one child session id can
serve every parent in turn. Each run classifies from the lineage
headers the executor landed before it, and the pair decides whether a
``child_start`` is due (``_opening``).

The plugin holds the stop of a child scope. It registers the
APPA-owned tool ``appa_return`` on every model request of that scope,
and it replaces the final message of the child with one call to that
tool. The body of the tool posts ``child_end``, where the value of the
child crosses. The runtime acknowledges the value, names other bytes,
or blocks the return with a reason the model reads as a tool result.

The plugin knows its own gate by the object it built, never by the name
that object answers to. A tool of the same name from anywhere else
crosses the tool gate like any other, and the config guard refuses a
rendered config that declares one.

The plugin also declares the return of a spawn itself. A ``deny_call``
that offers a return route never reaches the model: the plugin takes
the bare floor, runs that plan on the ``/mcp`` endpoint of the runtime,
and proposes the same call again.
"""

from __future__ import annotations

import asyncio
import json
import logging
from collections.abc import Awaitable, Callable
from datetime import timedelta
from typing import Any

import httpx
from google.adk.models.llm_response import LlmResponse
from google.adk.plugins.base_plugin import BasePlugin
from google.adk.tools.function_tool import FunctionTool
from google.genai import types

from . import wire
from .identity import SessionIdentity

logger = logging.getLogger("appa_kagent_adk.plugin")

_DENY_KEY = "appa"
_DENIED = "denied"
_WITHHELD = "withheld"
_REVIEW = "review"

_REVIEW_PENDING = (
    "[appa] this remedy needs a person's ruling. The reviewer has been asked through the "
    "confirmation; wait for the answer and do not call the tool again."
)

RETURN_TOOL = "appa_return"
"""The name of the tool a child scope stops through.

APPA owns the gate object, and only that object crosses no tool gate.
The name is what the model types, and anything else that answers to it
is somebody else's tool.
"""

# What the return gate hands the model back. A crossing names the bytes
# the child must repeat, so its outgoing reply carries what crossed.
_RETURN_CROSSED = (
    "[appa] the return crossed. End this errand now, with exactly this text as your final message:\n{value}"
)
_RETURN_VOID = "[appa] the void return crossed. End this errand now with an empty final message."
_RETURN_BLOCKED = "[appa] this return did not cross: {reason}"

# Agent-as-tool classes, by name: ADK's own AgentTool family plus
# kagent's remote A2A tool. Name-based so the plugin imports no kagent
# code and no version-specific ADK module.
_SPAWN_TOOL_TYPES = (
    "AgentTool",
    "GoogleSearchAgentTool",
    "KAgentRemoteA2ATool",
)

_GATED_TIMEOUT_SECONDS = 120.0
_TURN_END_TIMEOUT_SECONDS = 30.0
# The remedy call the plugin routes itself, over the MCP endpoint of the
# runtime. One plan can hold for the whole consult window of the
# runtime, so this budget must outlast that window.
_REMEDY_TIMEOUT_SECONDS = 300.0


class AppaFailClosed(RuntimeError):
    """The runtime blocked, refused, or could not answer. The action stops."""


def _withheld(reason: str) -> dict[str, Any]:
    return {"result": f"[appa] the tool result was withheld: {reason}", _DENY_KEY: _WITHHELD}


class AppaPluginKagent(BasePlugin):
    def __init__(
        self,
        runtime_url: str,
        *,
        client_factory: Callable[[], httpx.AsyncClient] | None = None,
        spawn_tool_types: tuple[str, ...] = _SPAWN_TOOL_TYPES,
        identity: SessionIdentity | None = None,
        remedy_call: Callable[[dict[str, Any]], Awaitable[str]] | None = None,
    ):
        super().__init__(name="appa_plugin_kagent")
        if not runtime_url:
            raise ValueError("AppaPluginKagent needs the appa-runtime URL (APPA_RUNTIME_URL)")
        self._hook_url = runtime_url.rstrip("/") + "/hook"
        self._mcp_url = runtime_url.rstrip("/") + "/mcp"
        # kagent builds a fresh ADK Runner per A2A request around ONE
        # plugin instance, and every Runner close closes the plugin. So
        # the transport is a factory: a closed client is replaced on the
        # next gated event instead of failing the whole next request.
        self._client_factory = client_factory or (lambda: httpx.AsyncClient(timeout=_GATED_TIMEOUT_SECONDS))
        self._client = self._client_factory()
        self._spawn_tool_types = spawn_tool_types
        # Shared with the entrypoint gates. The gates classify from the
        # session state, which the kagent executor lands before each
        # run, so a synthetic call lands in the trajectory the run
        # pinned.
        self._identity = identity or SessionIdentity()
        # The (root, child) pairs whose child_start the runtime acked
        # through this plugin instance. A pair the runtime refused, or
        # that never reached it, stays out and opens again on the next
        # entry. Nothing prunes the set: one entry per parent that
        # delegates into this pod over its life.
        self._opened: set[tuple[str, str]] = set()
        # The reviews a deny handed over: offers whose plans consult a
        # human authority, with the text the person reads. The reserved
        # call that quotes one asks the person through kagent's own
        # confirmation before it crosses, and the answer rides the call.
        self._reviews: dict[str, str] = {}
        # The return the gate crossed for a run, by invocation id, and
        # the exact bytes that crossed. The stop of that run then carries
        # those bytes, so the reply the child sends replays them. The
        # run's end drops the entry.
        self._crossed: dict[str, str] = {}
        # The calls this plugin closed itself, by ADK's function-call
        # id, under the invocation that dispatched each one. A denied
        # call opened no dispatch, and a failed call closed the dispatch
        # at its error point, so the result gate of either reports
        # nothing. The result gate drops the entry it reads, and the
        # run's end drops what no result gate reached.
        self._settled: dict[str, set[str]] = {}
        # ADK executes parallel function calls concurrently, while one
        # OpenAPPA branch admits one dispatch lifecycle at a time. A
        # function-call id holds its branch's lock from the call gate
        # through the result or error gate. Calls on other branches stay
        # independent.
        self._dispatch_locks: dict[tuple[str, str | None], asyncio.Lock] = {}
        self._dispatch_users: dict[tuple[str, str | None], int] = {}
        self._dispatch_leases: dict[tuple[str, str], tuple[tuple[str, str | None], asyncio.Lock]] = {}
        self._dispatch_waiters: dict[tuple[str, str], tuple[tuple[str, str | None], asyncio.Lock]] = {}
        self._abandoned_invocations: set[str] = set()
        self._runner_invocations: dict[asyncio.Task, set[str]] = {}
        # The agent scope each invocation opened first. Every later
        # scope of that invocation is an in-process child.
        self._scopes: dict[str, str] = {}
        # The declaration path the plugin routes without the model. The
        # seam takes the arguments of `execute_remedy_plan` and answers
        # with the text the runtime rendered.
        self._remedy_call = remedy_call or self._remedy_over_mcp
        # The tool a child scope stops through. ADK resolves the call
        # from the request the plugin registered it on.
        self._return_tool = _return_gate_tool(self)

    async def close(self) -> None:
        task = asyncio.current_task()
        for invocation_id in self._runner_invocations.pop(task, set()) if task is not None else ():
            self._abandon_invocation(invocation_id)
        await self._client.aclose()

    # -- transport ----------------------------------------------------

    def _live_client(self) -> httpx.AsyncClient:
        if self._client.is_closed:
            self._client = self._client_factory()
        return self._client

    async def _post(self, event: dict[str, Any]) -> wire.Decision:
        """Post one gated event; anything but a 200 decision fails closed."""
        try:
            answer = await self._live_client().post(self._hook_url, json=event)
        except httpx.HTTPError as error:
            raise AppaFailClosed(f"the appa /hook channel is down: {error}") from error
        if answer.status_code != 200:
            raise AppaFailClosed(f"appa answered {answer.status_code}: {answer.text[:500]}")
        try:
            return wire.parse_decision(answer.content)
        except wire.WireError as error:
            raise AppaFailClosed(str(error)) from error

    async def _post_quiet(self, event: dict[str, Any]) -> None:
        """Post a turn end; a finished turn never blocks the harness."""
        try:
            await self._live_client().post(self._hook_url, json=event, timeout=_TURN_END_TIMEOUT_SECONDS)
        except httpx.HTTPError as error:
            logger.warning("a turn end did not reach appa-runtime: %s", error)

    async def _ping(self) -> None:
        """The liveness gate: pass only while the /hook channel answers.

        A ping feeds no event, so the runtime answers a bare 200 ``{}``
        — reachability is the whole check.
        """
        try:
            answer = await self._live_client().post(self._hook_url, json=wire.ping())
        except httpx.HTTPError as error:
            raise AppaFailClosed(f"the appa /hook channel is down: {error}") from error
        if answer.status_code != 200:
            raise AppaFailClosed(f"the liveness probe answered {answer.status_code}")

    async def _remedy_over_mcp(self, arguments: dict[str, Any]) -> str:
        """Run one remedy plan on the MCP endpoint of the runtime.

        The vouch of the preceding ``tool_call`` names the trajectory,
        so the call itself carries only the quoted offer and its
        arguments. The runtime answers with the text it rendered, and a
        failure to reach it fails closed like every other post.
        """
        # The import stays here: the locked lane installs no MCP client,
        # and only a held spawn reaches this path.
        from mcp import ClientSession
        from mcp.client.streamable_http import streamablehttp_client

        budget = timedelta(seconds=_REMEDY_TIMEOUT_SECONDS)
        try:
            async with streamablehttp_client(self._mcp_url, timeout=budget) as (read, write, _):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    answer = await session.call_tool(wire.RESERVED_TOOL, arguments, read_timeout_seconds=budget)
        except Exception as error:
            raise AppaFailClosed(f"the appa /mcp endpoint did not run the remedy plan: {_cause(error)}") from error
        return _mcp_text(answer)

    # -- ids ----------------------------------------------------------

    def _ids(self, context: Any) -> tuple[str, str | None]:
        """The pair of the run this callback context belongs to."""
        return self._identity.ids_for(context)

    def _is_fresh(self, session: Any) -> bool:
        return self._identity.is_fresh(session)

    def _is_spawn(self, tool: Any) -> bool:
        return type(tool).__name__ in self._spawn_tool_types

    def _claim_scope(self, invocation_id: str, agent_name: str) -> bool:
        """Whether this agent scope is the invocation's own: the first scope it opened, or a re-entry of it.

        ADK builds every callback context from the agent that is
        running, so the name a callback carries and the name its context
        reports are always the same one. The first scope of a run is
        therefore the only thing that tells the agent the run entered on
        from an in-process child of it, and the plugin remembers it.
        """
        return self._scopes.setdefault(invocation_id, agent_name) == agent_name

    def _close_run(self, invocation_id: str) -> None:
        """Drop everything this run pinned. The next run reads afresh."""
        self._abandon_invocation(invocation_id)
        for task, invocations in list(self._runner_invocations.items()):
            invocations.discard(invocation_id)
            if not invocations:
                self._runner_invocations.pop(task)
        self._identity.close_invocation(invocation_id)
        self._crossed.pop(invocation_id, None)
        self._scopes.pop(invocation_id, None)
        self._settled.pop(invocation_id, None)

    # -- synthetic gates for the entrypoint wrappers ------------------

    async def gate_synthetic_call(self, session: Any, tool: str, arguments: dict[str, Any]) -> wire.Decision:
        """Gate an out-of-band flow as a tool call in the session's trajectory.

        The entrypoint wraps ADK features that move a value without a
        FunctionTool call — code execution, the memory write-back — and
        brings each under the tool gate through this method. The caller
        enforces the decision.
        """
        root_id, child_id = self._identity.ids(session)
        return await self._post(wire.tool_call(root_id, tool, arguments, False, child_id))

    async def report_synthetic_result(
        self, session: Any, tool: str, arguments: dict[str, Any], body: Any
    ) -> wire.Decision:
        """Report an out-of-band flow's output as its tool result."""
        root_id, child_id = self._identity.ids(session)
        return await self._post(wire.tool_result(root_id, tool, arguments, wire.success(body), child_id))

    # -- session and prompt -------------------------------------------

    def _opening(self, session: Any, root_id: str, child_id: str | None) -> dict[str, Any] | None:
        """The opening event the emitting scope needs before its prompt crosses, or None.

        A root session opens with ``session_start`` while no content has
        crossed it. A delegated entry opens with ``child_start`` while this
        plugin instance has not opened its (root, child) pair: the child
        session id can be shared by every parent that delegates into this
        pod, so the pair, not the session, decides.

        A re-entry of an opened pair sends no ``child_start``. That
        re-entry is a second delegation from the same parent into the
        same child context. The runtime ended the child trajectory when
        its first return crossed the parent's gate, and the child context
        id can bind no second fork. The child then runs in the ended
        trajectory, the runtime refuses its tool calls, and the parent's
        return comes back withheld with ``the spawn did not take``. The
        log line tells that case from a child opened under another
        parent's root, which opens with its own ``child_start``.
        """
        if child_id is not None:
            if (root_id, child_id) in self._opened:
                logger.info(
                    "child %s re-enters under root %s with its pair already open; no child_start is sent. "
                    "A re-entry after the child's return runs in the ended child trajectory: the runtime "
                    "refuses its tool calls, and the parent's return comes back withheld",
                    child_id,
                    root_id,
                )
                return None
            logger.info("child %s opens under root %s", child_id, root_id)
            return wire.child_start(root_id, child_id)
        if self._is_fresh(session):
            logger.info("trajectory %s opens as a root", root_id)
            return wire.session_start(root_id)
        return None

    async def on_user_message_callback(self, *, invocation_context, user_message):
        self._own_invocation(invocation_context.invocation_id)
        root_id, child_id = self._identity.open_invocation(invocation_context)
        contract = None
        opening = self._opening(invocation_context.session, root_id, child_id)
        if opening is not None:
            decision = await self._post(opening)
            if decision.kind == "context" and child_id is not None:
                # The return policy of the fork needs words. The child
                # reads them in front of the request its parent sent, and
                # that request stands unchanged.
                contract = decision.text
            elif decision.kind != "ack":
                raise AppaFailClosed(f"appa refused the session: {decision.detail or decision.kind}")
            if child_id is not None:
                # A child_start for a pair the runtime already holds open
                # answers ack too, so a repeat after a plugin restart
                # changes nothing.
                self._opened.add((root_id, child_id))
        decision = await self._post(wire.prompt(root_id, _content_text(user_message), child_id))
        if decision.kind == "block":
            raise AppaFailClosed(f"appa blocked the prompt: {decision.reason}")
        if decision.kind != "ack":
            raise AppaFailClosed(f"appa answered the prompt with {decision.detail or decision.kind}")
        if contract is None:
            return None
        return _with_contract(contract, user_message)

    # -- liveness gates -----------------------------------------------

    async def before_run_callback(self, *, invocation_context):
        self._own_invocation(invocation_context.invocation_id)
        self._identity.open_invocation(invocation_context)
        await self._ping()
        return None

    async def on_event_callback(self, *, invocation_context, event):
        await self._ping()
        return None

    async def before_model_callback(self, *, callback_context, llm_request):
        await self._ping()
        _, child_id = self._ids(callback_context)
        if child_id is not None:
            # ADK rebuilds the request for every step, so the gate tool
            # is registered again for every step.
            llm_request.append_tools([self._return_tool])
        return None

    async def after_model_callback(self, *, callback_context, llm_response):
        await self._ping()
        _, child_id = self._ids(callback_context)
        if child_id is None:
            return None
        return self._hold_the_stop(callback_context.invocation_id, llm_response)

    async def on_model_error_callback(self, *, callback_context, llm_request, error):
        await self._ping()
        return None

    # -- agent scopes -------------------------------------------------

    async def before_agent_callback(self, *, agent, callback_context):
        if self._claim_scope(callback_context.invocation_id, agent.name):
            # The invocation's own agent: the prompt hook already marked
            # this turn (root), or the delegated entry did (child pod).
            await self._ping()
            return None
        root_id, _ = self._ids(callback_context)
        child_id = _local_child_id(callback_context.invocation_id, agent.name)
        decision = await self._post(wire.child_start(root_id, child_id))
        if decision.kind != "ack":
            raise AppaFailClosed(f"appa refused the child scope: {decision.detail or decision.kind}")
        return None

    async def after_agent_callback(self, *, agent, callback_context):
        if self._claim_scope(callback_context.invocation_id, agent.name):
            await self._ping()
            return None
        root_id, _ = self._ids(callback_context)
        await self._post_quiet(wire.turn_end(root_id, _local_child_id(callback_context.invocation_id, agent.name)))
        return None

    # -- the tool gate ------------------------------------------------

    async def _acquire_dispatch(self, root_id: str, child_id: str | None, tool_context: Any) -> None:
        """Queue one ADK call behind the open dispatch on its branch."""
        call_id = _call_id(tool_context)
        if call_id is None:
            return
        lease = (tool_context.invocation_id, call_id)
        if lease in self._dispatch_leases or lease in self._dispatch_waiters:
            raise AppaFailClosed(f"ADK reused function-call id {call_id!r} before its result")
        key = (root_id, child_id)
        lock = self._dispatch_locks.setdefault(key, asyncio.Lock())
        self._dispatch_users[key] = self._dispatch_users.get(key, 0) + 1
        self._dispatch_waiters[lease] = (key, lock)
        try:
            await lock.acquire()
        except BaseException:
            self._dispatch_waiters.pop(lease, None)
            self._drop_dispatch_user(key, lock)
            self._forget_abandoned(tool_context.invocation_id)
            raise
        self._dispatch_waiters.pop(lease, None)
        if tool_context.invocation_id in self._abandoned_invocations:
            lock.release()
            self._drop_dispatch_user(key, lock)
            self._forget_abandoned(tool_context.invocation_id)
            raise AppaFailClosed("the runner ended before this queued tool call could execute")
        self._dispatch_leases[lease] = (key, lock)

    def _release_dispatch(self, lease: tuple[str, str]) -> None:
        """Release the branch held by one completed ADK call."""
        held = self._dispatch_leases.pop(lease, None)
        if held is None:
            return
        key, lock = held
        lock.release()
        self._drop_dispatch_user(key, lock)
        self._forget_abandoned(lease[0])

    def _release_tool_dispatch(self, tool_context: Any) -> None:
        call_id = _call_id(tool_context)
        if call_id is not None:
            self._release_dispatch((tool_context.invocation_id, call_id))

    def _own_invocation(self, invocation_id: str) -> None:
        task = asyncio.current_task()
        if task is not None:
            self._runner_invocations.setdefault(task, set()).add(invocation_id)

    def _abandon_invocation(self, invocation_id: str) -> None:
        self._abandoned_invocations.add(invocation_id)
        for lease in [lease for lease in self._dispatch_leases if lease[0] == invocation_id]:
            self._release_dispatch(lease)
        self._forget_abandoned(invocation_id)

    def _forget_abandoned(self, invocation_id: str) -> None:
        if any(lease[0] == invocation_id for lease in self._dispatch_leases):
            return
        if any(waiter[0] == invocation_id for waiter in self._dispatch_waiters):
            return
        self._abandoned_invocations.discard(invocation_id)

    def _drop_dispatch_user(self, key: tuple[str, str | None], lock: asyncio.Lock) -> None:
        users = self._dispatch_users[key] - 1
        if users:
            self._dispatch_users[key] = users
            return
        self._dispatch_users.pop(key)
        if self._dispatch_locks.get(key) is lock:
            self._dispatch_locks.pop(key)

    def _settle(self, tool_context: Any) -> None:
        """Remember that this plugin closed the call the context names.

        A call the plugin answered at the call gate, or one whose
        failure already crossed. A call ADK gave no id cannot be
        remembered: its result gate then reports a dispatch the runtime
        never opened, the runtime blocks that report, and the model
        reads the withheld notice in place of the output of its tool.
        ADK fills in an id the model left out, so a dispatched call
        carries one.
        """
        call_id = _call_id(tool_context)
        if call_id is not None:
            self._settled.setdefault(tool_context.invocation_id, set()).add(call_id)

    def _is_settled(self, tool_context: Any) -> bool:
        """Whether this plugin already closed the call the context names.

        The read consumes the entry: one dispatch reaches one result
        gate, and ADK mints a fresh id for every later call.
        """
        call_id = _call_id(tool_context)
        calls = self._settled.get(tool_context.invocation_id)
        if call_id is None or calls is None or call_id not in calls:
            return False
        calls.discard(call_id)
        return True

    async def before_tool_callback(self, *, tool, tool_args, tool_context):
        root_id, child_id = self._ids(tool_context)
        if tool is self._return_tool:
            # APPA owns the return gate — the object this plugin built,
            # not the name it answers to. Its body posts the stop of the
            # child, so the call itself crosses no tool gate. A tool of
            # the same name from anywhere else is somebody else's, and
            # it crosses the gate like any other.
            return None
        ruling = None
        if tool.name == wire.RESERVED_TOOL:
            offer = str(tool_args.get("offer_id", ""))
            confirmation = getattr(tool_context, "tool_confirmation", None)
            if confirmation is None:
                review = self._reviews.get(offer)
                if review is not None:
                    # The person rules before the act, through kagent's
                    # stock confirmation. The hint is the consult artifact
                    # as the runtime renders it — nothing the model said —
                    # and the answer comes back on the resumed call, never
                    # through the model.
                    tool_context.request_confirmation(hint=review, payload={"appa": _REVIEW, "offer_id": offer})
                    self._settle(tool_context)
                    return {"result": _REVIEW_PENDING, _DENY_KEY: _REVIEW}
            else:
                ruling = "approve" if confirmation.confirmed else "deny"
                self._reviews.pop(offer, None)
        await self._acquire_dispatch(root_id, child_id, tool_context)
        call = wire.tool_call(root_id, tool.name, _plain_json(tool_args), self._is_spawn(tool), child_id, ruling=ruling)
        try:
            decision = await self._post(call)
            route = _return_offer(decision)
            if route is not None:
                decision = await self._declare_return(root_id, child_id, call, route, decision)
        except BaseException:
            self._release_tool_dispatch(tool_context)
            raise
        if decision.kind in ("allow_call", "pass_control"):
            return None
        if decision.kind == "deny_call":
            for offer_id, text in decision.review:
                self._reviews[offer_id] = text
            self._settle(tool_context)
            self._release_tool_dispatch(tool_context)
            # The feedback rides under "result": kagent's model converters
            # serialize a function response only from a str or a dict with
            # a "content" or "result" key, and any other shape reaches the
            # model as an EMPTY tool message — the model then cannot see
            # why the call was blocked, or the remedy offer it quotes.
            return {"result": decision.feedback, _DENY_KEY: _DENIED}
        self._release_tool_dispatch(tool_context)
        raise AppaFailClosed(f"appa answered the tool call with {decision.detail or decision.kind}")

    async def after_tool_callback(self, *, tool, tool_args, tool_context, result):
        if tool is self._return_tool:
            # The return gate reported the stop at the child_end it
            # posted, and the runtime opened no dispatch for it.
            return None
        if self._is_settled(tool_context):
            # This plugin closed the call itself: it answered the call
            # gate with a deny or a review, which opened no dispatch, or
            # the failure crossed at the error point, which closed the
            # dispatch it opened. A report here would double-count one
            # dispatch. The id of the call decides that, never the
            # payload — a tool result carries whatever its tool spells,
            # the `appa` key included.
            self._release_tool_dispatch(tool_context)
            return None
        root_id, child_id = self._ids(tool_context)
        arguments = _plain_json(tool_args)
        # A result of None with no failure is a deferred or long-running
        # call. Nothing entered attention, and the dispatch is genuinely
        # unresolved here.
        outcome = wire.indeterminate() if result is None else wire.success(_plain_json(result))
        try:
            if self._is_spawn(tool):
                spawned_id, value = _spawn_return(result)
                decision = await self._post(
                    wire.spawn_result(root_id, tool.name, arguments, outcome, spawned_id, value, child_id)
                )
                if decision.kind == "ack":
                    return None
                if decision.kind == "child_return":
                    return {"result": decision.value}
                if decision.kind == "replace_output":
                    return {"result": decision.output}
                if decision.kind == "block":
                    return _withheld(decision.reason or "")
                raise AppaFailClosed(f"appa answered the spawn result with {decision.detail or decision.kind}")
            decision = await self._post(wire.tool_result(root_id, tool.name, arguments, outcome, child_id))
            if decision.kind == "ack":
                return None
            if decision.kind == "replace_output":
                return {"result": decision.output}
            if decision.kind == "block":
                return _withheld(decision.reason or "")
            raise AppaFailClosed(f"appa answered the tool result with {decision.detail or decision.kind}")
        finally:
            self._release_tool_dispatch(tool_context)

    async def on_tool_error_callback(self, *, tool, tool_args, tool_context, error):
        # The failure closes the dispatch here. ADK runs the result gate
        # of a handled failure too, and that second report would
        # double-count one dispatch.
        self._settle(tool_context)
        root_id, child_id = self._ids(tool_context)
        try:
            decision = await self._post(
                wire.tool_result(root_id, tool.name, _plain_json(tool_args), wire.failure(str(error)), child_id)
            )
            if decision.kind == "ack":
                return None  # the original error propagates
            if decision.kind == "replace_output":
                return {"result": decision.output}
            if decision.kind == "block":
                return _withheld(decision.reason or "")
            raise AppaFailClosed(f"appa answered the tool failure with {decision.detail or decision.kind}")
        finally:
            self._release_tool_dispatch(tool_context)

    # -- the return gate ----------------------------------------------

    async def _declare_return(
        self, root_id: str, child_id: str | None, call: dict[str, Any], offer: wire.Offer, denial: wire.Decision
    ) -> wire.Decision:
        """Declare the return of a held spawn, then propose the call again.

        The runtime holds a marked spawn until this session declares what
        a return may carry. The plugin declares the bare floor itself, so
        the model reads one ordinary tool call and its result. An empty
        label is the label the parent holds now.

        The plugin declares once per call. A runtime that does not vouch
        for the plan hands the block back with its menu, and a second
        deny goes to the model as it stands.
        """
        arguments = {"offer_id": offer.offer_id, "label": {}}
        vouch = await self._post(wire.tool_call(root_id, wire.RESERVED_TOOL, arguments, False, child_id))
        if vouch.kind != "pass_control":
            logger.info(
                "appa answered the return declaration %s with %s, so the block goes to the model",
                offer.offer_id,
                vouch.kind,
            )
            return denial
        answer = await self._remedy_call(arguments)
        logger.debug("the return declaration %s answered: %s", offer.offer_id, answer)
        return await self._post(call)

    async def hold_the_return(self, tool_context: Any, text: str) -> dict[str, Any]:
        """Post the stop of a child scope and enforce the ruling of the runtime.

        The body of the return gate. An ``ack`` crossed the value as the
        child spoke it. A ``child_return`` names other bytes, which the
        plugin returns as a second ``child_end`` before the child stops
        with them. A ``block`` carries the reason to the model, which
        writes another final message this gate holds the same way.
        """
        root_id, child_id = self._ids(tool_context)
        if child_id is None:
            raise AppaFailClosed("the return gate ran outside a child scope, where no return crosses")
        decision = await self._post(wire.child_end(root_id, child_id, text or None))
        if decision.kind == "child_return":
            value = decision.value or ""
            echo = await self._post(wire.child_end(root_id, child_id, value or None))
            if echo.kind != "ack":
                raise AppaFailClosed(f"appa answered the returned bytes with {echo.detail or echo.reason or echo.kind}")
            return self._crossing(tool_context.invocation_id, value)
        if decision.kind == "ack":
            return self._crossing(tool_context.invocation_id, text)
        if decision.kind == "block":
            return {"result": _RETURN_BLOCKED.format(reason=decision.reason or "")}
        raise AppaFailClosed(f"appa answered the child end with {decision.detail or decision.kind}")

    def _crossing(self, invocation_id: str, value: str) -> dict[str, Any]:
        """Hold what crossed for this run and tell the model to stop with it."""
        self._crossed[invocation_id] = value
        if not value:
            return {"result": _RETURN_VOID}
        return {"result": _RETURN_CROSSED.format(value=value)}

    def _hold_the_stop(self, invocation_id: str, llm_response: Any) -> Any:
        """The stop of a child scope: the gate call, or the value that crossed.

        A response that proposes a tool call is no stop, and neither is a
        partial one or one that carries reasoning alone. A stop before
        the return crossed becomes one call to the return gate, carrying
        the answer of the child. A stop after it carries the bytes that
        crossed, so the reply the child sends replays them.
        """
        if getattr(llm_response, "partial", None):
            return None
        content = getattr(llm_response, "content", None)
        parts = getattr(content, "parts", None) or []
        if any(getattr(part, "function_call", None) is not None for part in parts):
            return None
        # The reasoning of a model is no part of its answer, and a
        # response that carries reasoning alone answers nothing yet.
        answer = [part for part in parts if not getattr(part, "thought", None)]
        if not answer:
            return None
        crossed = self._crossed.get(invocation_id)
        if crossed is not None:
            return _spoken(crossed)
        return _return_call("\n".join(part.text for part in answer if getattr(part, "text", None)))

    # -- turn ends ----------------------------------------------------

    async def after_run_callback(self, *, invocation_context):
        # The turn ends under the pair the run open pinned, so the
        # turn_end lands where the prompt and the tool calls of the run
        # landed. The pin then goes, and the next run classifies afresh.
        root_id, child_id = self._ids(invocation_context)
        await self._post_quiet(wire.turn_end(root_id, child_id))
        self._close_run(invocation_context.invocation_id)

    # google-adk 2.8.0 only — the 1.31.1 manager never calls these two.

    async def on_agent_error_callback(self, *, agent, callback_context, error):
        root_id, _ = self._ids(callback_context)
        child_id = None
        if not self._claim_scope(callback_context.invocation_id, agent.name):
            child_id = _local_child_id(callback_context.invocation_id, agent.name)
        await self._post_quiet(wire.turn_end(root_id, child_id))
        return None  # the error still propagates

    async def on_run_error_callback(self, *, invocation_context, error):
        # google-adk 2.8.0 runs no after_run_callback for a failed run,
        # so the failed run's pin goes here.
        root_id, child_id = self._ids(invocation_context)
        await self._post_quiet(wire.turn_end(root_id, child_id))
        self._close_run(invocation_context.invocation_id)
        return None


def _return_gate_tool(plugin: AppaPluginKagent):
    """The return gate of one plugin instance, as an ADK tool.

    ADK builds the declaration from the signature, and it passes the
    tool context to the body. The body is the gate of the plugin.
    """

    async def appa_return(text: str, tool_context) -> dict[str, Any]:
        """End this errand and return this text to the agent that sent it.

        Call this tool with your whole final answer. The answer of this
        tool tells you what reached the caller.
        """
        return await plugin.hold_the_return(tool_context, text)

    return FunctionTool(func=appa_return)


def _return_call(text: str) -> LlmResponse:
    """The stop of a child, as one call to the return gate."""
    call = types.FunctionCall(name=RETURN_TOOL, args={"text": text})
    return _response(types.Part(function_call=call))


def _spoken(value: str) -> LlmResponse:
    """The stop of a child, carrying the bytes that crossed."""
    return _response(types.Part(text=value))


def _response(part: Any) -> LlmResponse:
    return LlmResponse(content=types.Content(role="model", parts=[part]))


def _return_offer(decision: wire.Decision) -> wire.Offer | None:
    """The offer of a deny that takes the return of a child as spoken.

    That offer is the bare floor of the return menu, which the runtime
    lists first. A deny with no such offer carries no return route, and
    the model reads it.
    """
    if decision.kind != "deny_call":
        return None
    for offer in decision.offers:
        if offer.returns == wire.RETURN_AS_SPOKEN:
            return offer
    return None


def _with_contract(text: str, message: Any) -> Any:
    """The first user message of a child, with the return contract in front.

    kagent carries no side channel for the contract, so it rides as the
    first part of the message the child reads.
    """
    parts: list[Any] = [types.Part(text=text)]
    parts.extend(getattr(message, "parts", None) or [])
    return types.Content(role=getattr(message, "role", None) or "user", parts=parts)


def _cause(error: BaseException) -> str:
    """The plainest text of one failure.

    The MCP client raises the failure of its task group, which hides the
    reason one level down. A group renders its first member instead.
    """
    inner = getattr(error, "exceptions", None)
    while inner:
        error = inner[0]
        inner = getattr(error, "exceptions", None)
    return f"{type(error).__name__}: {error}"


def _mcp_text(answer: Any) -> str:
    """The text of one MCP tool result, joined. An error result reads the same way."""
    blocks = getattr(answer, "content", None) or []
    return "\n".join(block.text for block in blocks if getattr(block, "text", None))


def _call_id(tool_context: Any) -> str | None:
    """ADK's id for the call a tool context belongs to.

    One context serves the whole dispatch of one function call — the
    call gate, the tool body, the error point and the result gate — and
    it carries the id of that call.
    """
    call_id = getattr(tool_context, "function_call_id", None)
    return call_id if isinstance(call_id, str) and call_id else None


def _local_child_id(invocation_id: str, agent_name: str) -> str:
    """An in-process child scope's id, unique per invocation."""
    return f"{invocation_id}:{agent_name}"


def _content_text(content: Any) -> str:
    """The prompt text of a user message: its text parts, joined.

    A message with no text part still crosses the gate — as its JSON
    rendering, so non-text ingress is never invisible to the policy.
    """
    parts = getattr(content, "parts", None) or []
    texts = [part.text for part in parts if getattr(part, "text", None)]
    if texts:
        return "\n".join(texts)
    dump = getattr(content, "model_dump", None)
    if callable(dump):
        return json.dumps(dump(exclude_none=True), default=str)
    return str(content)


def _plain_json(value: Any) -> Any:
    """A JSON-representable copy of an ADK value, losslessly at best."""
    try:
        json.dumps(value)
        return value
    except (TypeError, ValueError):
        return json.loads(json.dumps(value, default=str))


def _spawn_return(result: Any) -> tuple[str | None, str | None]:
    """The (spawned child id, returned value) a spawn result carries.

    A child reply arrives as ``{"result": ...}`` for Task replies —
    with the child's context id under ``subagent_session_id`` — and as
    a bare string for direct Message replies and the error paths; both
    shapes cross.
    """
    if isinstance(result, str):
        return None, result or None
    if isinstance(result, dict):
        value = result.get("result")
        if value is not None and not isinstance(value, str):
            value = json.dumps(value, default=str)
        spawned = result.get("subagent_session_id")
        return (spawned if isinstance(spawned, str) else None), (value or None)
    return None, None
