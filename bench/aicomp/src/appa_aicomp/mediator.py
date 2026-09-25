"""APPA mediation of every aicomp_sdk tool call.

The guardrail slot runs `Session.check` (a block becomes the SDK's DENY with
APPA's feedback as the reason the model reads); POST_TOOL_CALL runs
`Session.report` and replaces a successful output the model sees with APPA's
delivered content; a tool error reaches the model as the SDK reported it. Every APPA fault is a DENY or a withheld output (fail closed).
"""

import json
import logging
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from enum import StrEnum
from importlib.resources import files
from typing import Any, Protocol

import appa_agent_python
from aicomp_sdk.agents.tool_specs import serialize_tool_output
from aicomp_sdk.agents.types import AgentToolSpec
from aicomp_sdk.guardrails.base import Decision as GuardDecision
from aicomp_sdk.guardrails.base import GuardrailBase
from aicomp_sdk.hooks import HookContext, HookRegistry, HookResult, HookStage

logger = logging.getLogger(__name__)

# The engine's wording for a remedy that only narrows this trajectory's label.
NARROWING_OFFER = re.compile(r'Accept this change for the rest of this session:\s*\n\s*execute_remedy_plan\(offer_id: "([0-9a-f]+)"\)')
REMEDY_TOOL = "execute_remedy_plan"


def policy_text(*names: str) -> str:
    return "\n".join((files("appa_aicomp") / name).read_text() for name in names)


class Verdict(StrEnum):
    ALLOWED = "allowed"
    ALLOWED_AFTER_NARROWING = "allowed_after_narrowing"
    BLOCKED = "blocked"
    FAULT = "fault"
    SPAWN = "spawn"


@dataclass(frozen=True)
class Decision:
    actor: str
    tool: str
    arguments: dict[str, object]
    verdict: Verdict
    feedback: str | None = None


class Checker(Protocol):
    """`Session` and `ChildSession` share this surface."""

    def check(self, tool: str, arguments: str | None = None) -> str: ...
    def report(self, content: str | None = None, error: bool = False) -> str: ...
    def abandon(self) -> None: ...


def resolve(checker: Checker, tool: str, arguments: dict[str, object], accept_narrowing: bool) -> tuple[Verdict, str | None]:
    """One proposal, accepting a narrowing remedy the way a model following `Continue:` would."""
    match json.loads(checker.check(tool, json.dumps(arguments))):
        case {"kind": "allowed"}:
            return Verdict.ALLOWED, None
        case {"kind": "blocked", "feedback": str(feedback)}:
            offer = NARROWING_OFFER.search(feedback)
            if not (accept_narrowing and offer):
                return Verdict.BLOCKED, feedback
            match json.loads(checker.check(REMEDY_TOOL, json.dumps({"offer_id": offer.group(1)}))):
                case {"kind": "control"}:
                    pass
                case other:
                    raise RuntimeError(f"remedy did not return control: {other}")
            match json.loads(checker.check(tool, json.dumps(arguments))):
                case {"kind": "allowed"}:
                    return Verdict.ALLOWED_AFTER_NARROWING, feedback
                case {"kind": "blocked", "feedback": str(again)}:
                    return Verdict.BLOCKED, again
                case other:
                    raise RuntimeError(f"unexpected re-check envelope: {other}")
        case other:
            raise RuntimeError(f"unexpected check envelope: {other}")


def delivered_content(response: str) -> str:
    match json.loads(response):
        case {"kind": "delivered", "content": str(content)}:
            return content
        case other:
            raise RuntimeError(f"unexpected report envelope: {other}")


@dataclass
class AppaMediator:
    """One APPA session per replayed candidate (a fresh env)."""

    policy: str
    tools: Sequence[AgentToolSpec]
    spawn_tool: str | None = None
    accept_narrowing: bool = True
    decisions: list[Decision] = field(default_factory=list)
    session: appa_agent_python.Session | None = None
    _pending: bool = False

    def registry(self) -> HookRegistry:
        registry = HookRegistry()
        registry.register_hook(HookStage.CONTEXT_BUILD, self._on_user_message)
        registry.register_hook(HookStage.POST_TOOL_CALL, self._on_post_tool)
        return registry

    def guardrail(self) -> GuardrailBase:
        return _AppaGuardrail(self)

    def close(self) -> None:
        if self.session is None:
            return
        if self._pending:
            self.session.abandon()
        self.session.close()
        self.session = None

    def _on_user_message(self, ctx: HookContext) -> HookResult:
        # The binding takes only the opening prompt; later user turns are not
        # reported to APPA and are missing from its audit trail.
        if self.session is not None:
            return HookResult()
        try:
            schemas = [
                {"type": "function", "function": {"name": spec.name, "parameters": spec.parameters_json_schema}}
                for spec in self.tools
            ]
            self.session = appa_agent_python.Session(
                self.policy, json.dumps(schemas), ctx.context["user_message"], spawn_tool=self.spawn_tool
            )
        except Exception:
            # SDK hooks swallow exceptions; with no session every call is denied as a fault.
            logger.exception("opening the APPA session failed")
        return HookResult()

    def decide(self, tool: str, arguments: dict[str, object]) -> GuardDecision:
        if tool == self.spawn_tool:
            # `spawn_child` inside the tool proposes this call to APPA itself.
            self.decisions.append(Decision("root", tool, arguments, Verdict.SPAWN))
            return GuardDecision.allow("appa: spawn is checked by spawn_child")
        try:
            if self.session is None:
                raise RuntimeError("no APPA session: CONTEXT_BUILD did not open one")
            if self._pending:
                # The previous call raised inside the tool, so POST_TOOL_CALL never ran.
                self.session.abandon()
                self._pending = False
            verdict, feedback = resolve(self.session, tool, arguments, self.accept_narrowing)
        except Exception:
            logger.exception("APPA check failed; denying")
            verdict, feedback = Verdict.FAULT, "[appa] mediation fault"
        self.decisions.append(Decision("root", tool, arguments, verdict, feedback))
        match verdict:
            case Verdict.ALLOWED | Verdict.ALLOWED_AFTER_NARROWING:
                self._pending = True
                return GuardDecision.allow("appa")
            case _:
                return GuardDecision.deny(feedback or "[appa] blocked")

    def _on_post_tool(self, ctx: HookContext) -> HookResult:
        if not self._pending:
            return HookResult()
        assert self.session is not None
        ok = bool(ctx.context.get("ok"))
        content = serialize_tool_output(ctx.tool_output) if ok else str(ctx.context.get("error") or "")
        self._pending = False
        try:
            delivered = delivered_content(self.session.report(content, not ok))
            if ok:
                ctx.modified_output = delivered
        except Exception:
            logger.exception("APPA report failed; withholding output")
            ctx.modified_output = "[appa] tool output withheld: mediation fault"
        return HookResult()


class _AppaGuardrail(GuardrailBase):
    def __init__(self, mediator: AppaMediator) -> None:
        self._mediator = mediator

    def decide(self, tool_name: str, tool_args: Mapping[str, Any], context: Mapping[str, Any]) -> GuardDecision:
        del context
        return self._mediator.decide(tool_name, dict(tool_args))
