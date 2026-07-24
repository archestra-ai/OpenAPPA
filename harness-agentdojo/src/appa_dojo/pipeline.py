"""Build stock and APPA-mediated AgentDojo pipelines."""

from __future__ import annotations

import os
from dataclasses import dataclass
from hashlib import sha256
from typing import TYPE_CHECKING

import openai
from agentdojo.agent_pipeline import (
    AgentPipeline,
    InitQuery,
    OpenAILLM,
    SystemMessage,
    ToolsExecutionLoop,
    ToolsExecutor,
)
from agentdojo.agent_pipeline.agent_pipeline import load_system_message

from appa_dojo.policies import Policy

if TYPE_CHECKING:
    from appa_dojo.defense import AppaToolsExecutor

OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"


@dataclass
class BuiltPipeline:
    pipeline: AgentPipeline
    executor: AppaToolsExecutor | None

    def close(self) -> None:
        if self.executor is not None:
            self.executor.close()


def openrouter_api_key() -> str:
    key = os.environ.get("OPENROUTER_API_KEY")
    if key is None:
        raise RuntimeError("OPENROUTER_API_KEY is required to run the benchmark")
    return key


def build_pipeline(model: str, defense: str, policy: Policy) -> BuiltPipeline:
    llm = OpenAILLM(
        openai.OpenAI(
            base_url=OPENROUTER_BASE_URL,
            api_key=openrouter_api_key(),
        ),
        model,
    )
    executor: AppaToolsExecutor | None
    if defense == "none":
        executor = None
        elements = [
            SystemMessage(load_system_message(None)),
            InitQuery(),
            llm,
            ToolsExecutionLoop([ToolsExecutor(), llm]),
        ]
    elif defense in {"appa", "appa-open", "appa-practical", "appa-complete"}:
        from appa_dojo.defense import AppaRuntimeSetup, AppaToolsExecutionLoop, AppaToolsExecutor

        executor = AppaToolsExecutor(policy.toml)
        elements = [
            SystemMessage(load_system_message(None)),
            InitQuery(),
            AppaRuntimeSetup(),
            llm,
            AppaToolsExecutionLoop(executor, llm),
        ]
    else:
        raise ValueError(f"unsupported defense {defense!r}")

    pipeline = AgentPipeline(elements)
    policy_suffix = ""
    if executor is not None:
        from appa_dojo.native import BINDING_IDENTITY
        from appa_dojo.tool_bridge import PROTOCOL_IDENTITY

        policy_hash = sha256(policy.toml.encode()).hexdigest()[:10]
        policy_suffix = f"-{BINDING_IDENTITY}-{PROTOCOL_IDENTITY}-{policy_hash}"
    pipeline.name = f"{model.replace('/', '_')}-{defense}{policy_suffix}"
    return BuiltPipeline(pipeline=pipeline, executor=executor)
