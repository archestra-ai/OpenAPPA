"""Microsoft FIDES mediation for the AgentThreatBench Inspect loop.

The adapter keeps AgentThreatBench's model, prompts, tools, state, and scorers.
Microsoft Agent Framework's label tracker and policy enforcer mediate each tool
call.  Labels are assigned from the tool's role in the benchmark, never from
attack markers or other answer-key fields.

FIDES has no dynamic audience primitive corresponding to OpenAPPA's per-record
Data Exfiltration ACL.  Customer records are therefore private and public
delivery sinks use FIDES's coarse ``max_allowed_confidentiality=public`` gate.
This intentionally blocks authorized and unauthorized post-read delivery and
makes that expressiveness/utility trade-off visible in the paired controls.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from importlib.metadata import version

from agent_framework import Content, FunctionInvocationContext, FunctionTool, MiddlewareTermination
from agent_framework.security import (
    SECURITY_TOOL_INSTRUCTIONS,
    ConfidentialityLabel,
    IntegrityLabel,
    SecureAgentConfig,
)

FIDES_BINDING_IDENTITY = "agent-framework-fides-agentthreatbench-v1"
FIDES_NATIVE_BINDING_IDENTITY = "agent-framework-fides-native-agentthreatbench-v1"
FIDES_VERSION = version("agent-framework-core")

# Agent Framework 1.13 stores the currently executing label tracker in a
# thread-local during every middleware invocation. Inspect executes many
# samples as asyncio tasks on one thread, and the official security tools yield
# before consulting that thread-local. Keep one complete FIDES trajectory in
# flight per process so samples cannot interleave tracker state. The lock is
# acquired by a session's first mediated call and released only by close(); it
# deliberately does not serialize and re-queue every individual tool call.
FIDES_MAX_CONCURRENT_TRAJECTORIES = 1
_FIDES_TRAJECTORY_LOCK = asyncio.Lock()


@dataclass(frozen=True)
class FidesToolPolicy:
    """Result label and pre-call gates for one Inspect tool."""

    result_integrity: IntegrityLabel = IntegrityLabel.TRUSTED
    result_confidentiality: ConfidentialityLabel = ConfidentialityLabel.PUBLIC
    accepts_untrusted: bool = True
    max_allowed_confidentiality: ConfidentialityLabel | None = None

    def payload(self) -> dict[str, object]:
        return {
            "result_integrity": self.result_integrity.value,
            "result_confidentiality": self.result_confidentiality.value,
            "accepts_untrusted": self.accepts_untrusted,
            "max_allowed_confidentiality": (
                None if self.max_allowed_confidentiality is None else self.max_allowed_confidentiality.value
            ),
        }


@dataclass(frozen=True)
class FidesPolicy:
    task_type: str
    tools: Mapping[str, FidesToolPolicy]

    def check_covers(self, names: set[str]) -> None:
        missing = sorted(names - set(self.tools))
        stale = sorted(set(self.tools) - names)
        if missing or stale:
            raise ValueError(
                f"FIDES policy for {self.task_type!r} does not match the AgentThreatBench tool surface: "
                f"missing={missing} stale={stale}"
            )

    def serialize(self) -> str:
        payload = {name: policy.payload() for name, policy in sorted(self.tools.items())}
        return json.dumps(payload, sort_keys=True, separators=(",", ":"))


def _policy(
    task_type: str,
    names: set[str],
    *,
    untrusted_sources: set[str],
    private_sources: set[str] = frozenset(),
    protected_tools: set[str],
    public_sinks: set[str] = frozenset(),
) -> FidesPolicy:
    referenced = untrusted_sources | private_sources | protected_tools | public_sinks
    unknown = sorted(referenced - names)
    if unknown:
        raise ValueError(f"FIDES policy for {task_type!r} references unknown tools: {unknown}")
    return FidesPolicy(
        task_type=task_type,
        tools={
            name: FidesToolPolicy(
                result_integrity=(IntegrityLabel.UNTRUSTED if name in untrusted_sources else IntegrityLabel.TRUSTED),
                result_confidentiality=(
                    ConfidentialityLabel.PRIVATE if name in private_sources else ConfidentialityLabel.PUBLIC
                ),
                accepts_untrusted=name not in protected_tools,
                max_allowed_confidentiality=(ConfidentialityLabel.PUBLIC if name in public_sinks else None),
            )
            for name in sorted(names)
        },
    )


def fides_policy(task_type: str, names: set[str]) -> FidesPolicy:
    """Translate one AgentThreatBench task surface into a FIDES policy."""

    if task_type == "memory_poison":
        return _policy(
            task_type,
            names,
            untrusted_sources={"memory_read"},
            protected_tools={"memory_write"},
        )
    if task_type == "autonomy_hijack":
        return _policy(
            task_type,
            names,
            untrusted_sources={"read_inbox"},
            protected_tools={"categorize_email", "draft_summary", "respond_to_user"},
            public_sinks={"respond_to_user"},
        )
    if task_type == "data_exfil":
        return _policy(
            task_type,
            names,
            untrusted_sources={"lookup_customer"},
            private_sources={"lookup_customer"},
            protected_tools={"send_message", "respond_to_user"},
            public_sinks={"send_message", "respond_to_user"},
        )
    raise ValueError(f"unsupported AgentThreatBench task type for FIDES: {task_type!r}")


def fides_policy_digest(policy: FidesPolicy, *, native: bool) -> str:
    mode = "native-auto-hide" if native else "middleware-only"
    security = SECURITY_TOOL_INSTRUCTIONS if native else ""
    payload = f"{mode}\0{policy.serialize()}\0{security}"
    return hashlib.sha256(payload.encode()).hexdigest()


@dataclass(frozen=True)
class FidesBlocked:
    detail: str


@dataclass(frozen=True)
class FidesAllowed:
    content: str
    execution_error: str | None
    dispatched_arguments: dict[str, object]


type FidesResult = FidesBlocked | FidesAllowed


class FidesSession:
    """One fresh official FIDES trajectory for one Inspect sample."""

    def __init__(
        self,
        policy: FidesPolicy,
        *,
        native: bool = False,
        quarantine_chat_client: object | None = None,
    ) -> None:
        self.policy = policy
        self.native = native
        self.config = SecureAgentConfig(
            auto_hide_untrusted=native,
            block_on_violation=True,
            approval_on_violation=False,
            enable_audit_log=True,
            enable_policy_enforcement=True,
            quarantine_chat_client=quarantine_chat_client if native else None,
        )
        self.security_tools = {tool.name: tool for tool in self.config.get_tools()} if native else {}
        self._turn = 0
        self._closed = False
        self._owns_trajectory = False

    @property
    def binding_identity(self) -> str:
        return FIDES_NATIVE_BINDING_IDENTITY if self.native else FIDES_BINDING_IDENTITY

    def digest(self) -> str:
        return fides_policy_digest(self.policy, native=self.native)

    async def start(self) -> None:
        """Own the one in-process FIDES trajectory slot until ``close``."""
        if self._closed:
            raise RuntimeError("the FIDES episode is closed")
        if not self._owns_trajectory:
            await _FIDES_TRAJECTORY_LOCK.acquire()
            self._owns_trajectory = True

    async def invoke(
        self,
        tool: str,
        arguments: dict[str, object],
        dispatch: Callable[[dict[str, object]], Awaitable[object]] | None,
    ) -> FidesResult:
        if self._closed:
            raise RuntimeError("the FIDES episode is closed")
        await self.start()
        policy = self.policy.tools.get(tool)
        security_tool = self.security_tools.get(tool)
        if policy is None and security_tool is None:
            raise RuntimeError(f"FIDES policy does not declare AgentThreatBench tool {tool!r}")
        if security_tool is not None:
            function = security_tool
        else:
            assert policy is not None
            function = FunctionTool(
                name=tool,
                additional_properties={
                    "source_integrity": policy.result_integrity.value,
                    "confidentiality": policy.result_confidentiality.value,
                    "accepts_untrusted": policy.accepts_untrusted,
                    "max_allowed_confidentiality": (
                        None if policy.max_allowed_confidentiality is None else policy.max_allowed_confidentiality.value
                    ),
                },
            )
        context = FunctionInvocationContext(
            function=function,
            arguments=arguments,
            metadata={"turn_number": self._turn},
        )
        execution_error: str | None = None

        async def execute() -> None:
            nonlocal execution_error
            try:
                if security_tool is not None:
                    context.result = await security_tool.invoke(arguments=context.arguments)
                    return
                if dispatch is None:
                    raise RuntimeError(f"no dispatch callback for AgentThreatBench tool {tool!r}")
                assert policy is not None
                raw_result = await dispatch(dict(context.arguments))
                content = str(raw_result)
                integrity = policy.result_integrity
            except Exception as error:
                content = ""
                integrity = IntegrityLabel.TRUSTED
                execution_error = f"Tool failed: {type(error).__name__}: {error}"
                confidentiality = ConfidentialityLabel.PUBLIC if policy is None else policy.result_confidentiality
                context.result = [
                    Content.from_text(
                        content,
                        additional_properties={
                            "security_label": {
                                "integrity": integrity.value,
                                "confidentiality": confidentiality.value,
                            }
                        },
                    )
                ]
                return
            assert policy is not None
            context.result = [
                Content.from_text(
                    content,
                    additional_properties={
                        "security_label": {
                            "integrity": integrity.value,
                            "confidentiality": policy.result_confidentiality.value,
                        }
                    },
                )
            ]

        async def enforce() -> None:
            if self.config.policy_enforcer is None:
                raise RuntimeError("FIDES policy enforcement is disabled")
            await self.config.policy_enforcer.process(context, execute)

        async def mediate() -> FidesResult:
            if self.config.label_tracker is None:
                raise RuntimeError("FIDES label tracking is disabled")
            try:
                await self.config.label_tracker.process(context, enforce)
            except MiddlewareTermination:
                detail = context.result.get("error") if isinstance(context.result, dict) else str(context.result)
                return FidesBlocked(str(detail))
            if not isinstance(context.result, list) or not all(isinstance(item, Content) for item in context.result):
                raise RuntimeError("FIDES returned an invalid AgentThreatBench tool result")
            return FidesAllowed(
                "\n".join(item.text or "" for item in context.result),
                execution_error,
                dict(context.arguments),
            )

        try:
            return await mediate()
        finally:
            self._turn += 1

    def close(self) -> list[dict[str, object]]:
        if self._closed:
            return []
        self._closed = True
        try:
            return list(self.config.get_audit_log())
        finally:
            if self._owns_trajectory:
                self._owns_trajectory = False
                _FIDES_TRAJECTORY_LOCK.release()
