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

import hashlib
import json
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from importlib.metadata import version

from agent_framework import Content, FunctionInvocationContext, FunctionTool, MiddlewareTermination
from agent_framework.security import ConfidentialityLabel, IntegrityLabel, SecureAgentConfig

FIDES_BINDING_IDENTITY = "agent-framework-fides-agentthreatbench-v1"
FIDES_VERSION = version("agent-framework-core")


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

    def digest(self) -> str:
        return hashlib.sha256(self.serialize().encode()).hexdigest()


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


@dataclass(frozen=True)
class FidesBlocked:
    detail: str


@dataclass(frozen=True)
class FidesAllowed:
    content: str
    execution_error: str | None


type FidesResult = FidesBlocked | FidesAllowed


class FidesSession:
    """One fresh official FIDES trajectory for one Inspect sample."""

    def __init__(self, policy: FidesPolicy) -> None:
        self.policy = policy
        self.config = SecureAgentConfig(
            auto_hide_untrusted=False,
            block_on_violation=True,
            approval_on_violation=False,
            enable_audit_log=True,
            enable_policy_enforcement=True,
        )
        self._turn = 0
        self._closed = False

    async def invoke(
        self,
        tool: str,
        arguments: dict[str, object],
        dispatch: Callable[[], Awaitable[object]],
    ) -> FidesResult:
        if self._closed:
            raise RuntimeError("the FIDES episode is closed")
        policy = self.policy.tools.get(tool)
        if policy is None:
            raise RuntimeError(f"FIDES policy does not declare AgentThreatBench tool {tool!r}")
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
                raw_result = await dispatch()
                content = str(raw_result)
                integrity = policy.result_integrity
            except Exception as error:
                content = ""
                integrity = IntegrityLabel.TRUSTED
                execution_error = f"Tool failed: {type(error).__name__}: {error}"
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

        try:
            if self.config.label_tracker is None:
                raise RuntimeError("FIDES label tracking is disabled")
            await self.config.label_tracker.process(context, enforce)
        except MiddlewareTermination:
            detail = context.result.get("error") if isinstance(context.result, dict) else str(context.result)
            result: FidesResult = FidesBlocked(str(detail))
        else:
            if not isinstance(context.result, list) or not all(isinstance(item, Content) for item in context.result):
                raise RuntimeError("FIDES returned an invalid AgentThreatBench tool result")
            result = FidesAllowed(
                "\n".join(item.text or "" for item in context.result),
                execution_error,
            )
        finally:
            self._turn += 1
        return result

    def close(self) -> list[dict[str, object]]:
        if self._closed:
            return []
        self._closed = True
        return list(self.config.get_audit_log())
