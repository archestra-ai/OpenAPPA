"""Assemble AgentDojo pipelines, with or without the OpenAPPA defense."""

import os
from pathlib import Path

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

from appa_dojo.contracts import ContractTable
from appa_dojo.defense import AppaToolsExecutor

OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"
REPO_ENV = Path(__file__).resolve().parents[3] / ".env"


def openrouter_api_key() -> str:
    """$OPENROUTER_API_KEY, falling back to the repository-root .env file."""
    key = os.environ.get("OPENROUTER_API_KEY")
    if key:
        return key
    if REPO_ENV.exists():
        for line in REPO_ENV.read_text().splitlines():
            line = line.strip()
            if line.startswith("OPENROUTER_API_KEY="):
                value = line.split("=", 1)[1].strip()
                if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                    value = value[1:-1]
                if value:
                    return value
    raise RuntimeError(
        f"OPENROUTER_API_KEY not set and not found in {REPO_ENV}; "
        "bench mode needs an OpenRouter key"
    )


def build_pipeline(
    model: str,
    table: ContractTable,
    defense: str,
    unknown_policy: str,
    taint_policy: str = "allow",
) -> AgentPipeline:
    """The stock pipeline shape with the executor swapped per `defense`."""
    if defense == "appa":
        executor = AppaToolsExecutor(table, unknown_policy, taint_policy)
        name = f"{model.replace('/', '_')}-appa-{unknown_policy}"
    elif defense == "none":
        executor = ToolsExecutor()
        name = f"{model.replace('/', '_')}-none"
    else:
        raise ValueError(f"defense must be 'appa' or 'none', got {defense!r}")

    llm = OpenAILLM(
        openai.OpenAI(base_url=OPENROUTER_BASE_URL, api_key=openrouter_api_key()),
        model,
    )
    pipeline = AgentPipeline(
        [
            SystemMessage(load_system_message(None)),
            InitQuery(),
            llm,
            ToolsExecutionLoop([executor, llm]),
        ]
    )
    # Logdir path segment, result-cache key, and some attacks read this.
    pipeline.name = name
    return pipeline
