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
"""

from __future__ import annotations

import json
import logging
from collections.abc import Callable
from typing import Any

import httpx
from google.adk.plugins.base_plugin import BasePlugin

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
    ):
        super().__init__(name="appa_plugin_kagent")
        if not runtime_url:
            raise ValueError("AppaPluginKagent needs the appa-runtime URL (APPA_RUNTIME_URL)")
        self._hook_url = runtime_url.rstrip("/") + "/hook"
        # kagent builds a fresh ADK Runner per A2A request around ONE
        # plugin instance, and every Runner close closes the plugin. So
        # the transport is a factory: a closed client is replaced on the
        # next gated event instead of failing the whole next request.
        self._client_factory = client_factory or (lambda: httpx.AsyncClient(timeout=_GATED_TIMEOUT_SECONDS))
        self._client = self._client_factory()
        self._spawn_tool_types = spawn_tool_types
        # Shared with the entrypoint gates, so a synthetic tool call
        # lands in the same trajectory as the callbacks around it.
        self._identity = identity or SessionIdentity()
        # The reviews a deny handed over: offers whose plans consult a
        # human authority, with the text the person reads. The reserved
        # call that quotes one asks the person through kagent's own
        # confirmation before it crosses, and the answer rides the call.
        self._reviews: dict[str, str] = {}

    async def close(self) -> None:
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

    # -- ids ----------------------------------------------------------

    def _ids(self, session: Any) -> tuple[str, str | None]:
        return self._identity.ids(session)

    def _is_fresh(self, session: Any) -> bool:
        return self._identity.is_fresh(session)

    def _is_spawn(self, tool: Any) -> bool:
        return type(tool).__name__ in self._spawn_tool_types

    # -- synthetic gates for the entrypoint wrappers ------------------

    async def gate_synthetic_call(self, session: Any, tool: str, arguments: dict[str, Any]) -> wire.Decision:
        """Gate an out-of-band flow as a tool call in the session's trajectory.

        The entrypoint wraps ADK features that move a value without a
        FunctionTool call — code execution, the memory write-back — and
        brings each under the tool gate through this method. The caller
        enforces the decision.
        """
        root_id, child_id = self._ids(session)
        return await self._post(wire.tool_call(root_id, tool, arguments, False, child_id))

    async def report_synthetic_result(
        self, session: Any, tool: str, arguments: dict[str, Any], body: Any
    ) -> wire.Decision:
        """Report an out-of-band flow's output as its tool result."""
        root_id, child_id = self._ids(session)
        return await self._post(wire.tool_result(root_id, tool, arguments, wire.success(body), child_id))

    # -- session and prompt -------------------------------------------

    async def on_user_message_callback(self, *, invocation_context, user_message):
        session = invocation_context.session
        root_id, child_id = self._ids(session)
        if self._is_fresh(session):
            opening = wire.child_start(root_id, child_id) if child_id is not None else wire.session_start(root_id)
            if child_id is None:
                logger.info("trajectory %s opens as a root", root_id)
            else:
                logger.info("child %s opens under root %s", child_id, root_id)
            decision = await self._post(opening)
            if decision.kind != "ack":
                raise AppaFailClosed(f"appa refused the session: {decision.detail or decision.kind}")
        decision = await self._post(wire.prompt(root_id, _content_text(user_message), child_id))
        if decision.kind == "ack":
            return None
        if decision.kind == "block":
            raise AppaFailClosed(f"appa blocked the prompt: {decision.reason}")
        raise AppaFailClosed(f"appa answered the prompt with {decision.detail or decision.kind}")

    # -- liveness gates -----------------------------------------------

    async def before_run_callback(self, *, invocation_context):
        await self._ping()
        return None

    async def on_event_callback(self, *, invocation_context, event):
        await self._ping()
        return None

    async def before_model_callback(self, *, callback_context, llm_request):
        await self._ping()
        return None

    async def after_model_callback(self, *, callback_context, llm_response):
        await self._ping()
        return None

    async def on_model_error_callback(self, *, callback_context, llm_request, error):
        await self._ping()
        return None

    # -- agent scopes -------------------------------------------------

    async def before_agent_callback(self, *, agent, callback_context):
        if agent.name == callback_context.agent_name:
            # The invocation's own agent: the prompt already gated these
            # bytes (root), or the delegated entry did (child pod).
            await self._ping()
            return None
        root_id, _ = self._ids(callback_context.session)
        child_id = _local_child_id(callback_context.invocation_id, agent.name)
        decision = await self._post(wire.child_start(root_id, child_id))
        if decision.kind != "ack":
            raise AppaFailClosed(f"appa refused the child scope: {decision.detail or decision.kind}")
        return None

    async def after_agent_callback(self, *, agent, callback_context):
        if agent.name == callback_context.agent_name:
            await self._ping()
            return None
        root_id, _ = self._ids(callback_context.session)
        await self._post_quiet(wire.turn_end(root_id, _local_child_id(callback_context.invocation_id, agent.name)))
        return None

    # -- the tool gate ------------------------------------------------

    async def before_tool_callback(self, *, tool, tool_args, tool_context):
        root_id, child_id = self._ids(tool_context.session)
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
                    return {"result": _REVIEW_PENDING, _DENY_KEY: _REVIEW}
            else:
                ruling = "approve" if confirmation.confirmed else "deny"
                self._reviews.pop(offer, None)
        decision = await self._post(
            wire.tool_call(root_id, tool.name, _plain_json(tool_args), self._is_spawn(tool), child_id, ruling=ruling)
        )
        if decision.kind in ("allow_call", "pass_control"):
            return None
        if decision.kind == "deny_call":
            for offer_id, text in decision.review:
                self._reviews[offer_id] = text
            # The feedback rides under "result": kagent's model converters
            # serialize a function response only from a str or a dict with
            # a "content" or "result" key, and any other shape reaches the
            # model as an EMPTY tool message — the model then cannot see
            # why the call was blocked, or the remedy offer it quotes.
            return {"result": decision.feedback, _DENY_KEY: _DENIED}
        raise AppaFailClosed(f"appa answered the tool call with {decision.detail or decision.kind}")

    async def after_tool_callback(self, *, tool, tool_args, tool_context, result):
        if isinstance(result, dict) and result.get(_DENY_KEY) in (_DENIED, _REVIEW):
            # The plugin's own deny or review dict flowing back: already
            # reported at the call, and the runtime never opened a dispatch.
            return None
        root_id, child_id = self._ids(tool_context.session)
        arguments = _plain_json(tool_args)
        outcome = wire.success(_plain_json(result))
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

    async def on_tool_error_callback(self, *, tool, tool_args, tool_context, error):
        root_id, child_id = self._ids(tool_context.session)
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

    # -- turn ends ----------------------------------------------------

    async def after_run_callback(self, *, invocation_context):
        root_id, child_id = self._ids(invocation_context.session)
        await self._post_quiet(wire.turn_end(root_id, child_id))

    # google-adk 2.8.0 only — the 1.31.1 manager never calls these two.

    async def on_agent_error_callback(self, *, agent, callback_context, error):
        root_id, _ = self._ids(callback_context.session)
        child_id = None
        if agent.name != callback_context.agent_name:
            child_id = _local_child_id(callback_context.invocation_id, agent.name)
        await self._post_quiet(wire.turn_end(root_id, child_id))
        return None  # the error still propagates

    async def on_run_error_callback(self, *, invocation_context, error):
        root_id, child_id = self._ids(invocation_context.session)
        await self._post_quiet(wire.turn_end(root_id, child_id))
        return None


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
