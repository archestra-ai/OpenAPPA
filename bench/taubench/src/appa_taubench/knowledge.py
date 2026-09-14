"""Tau Knowledge tool-surface helpers shared by policy and mediation."""

from tau2.domains.banking_knowledge.retrieval_toolkits import (
    KnowledgeToolsAllTools,
    KnowledgeToolsPlain,
)
from tau2.environment.tool import Tool, as_tool
from tau2.environment.toolkit import DISCOVERABLE_ATTR

from appa_taubench import SUPPORTED_RETRIEVAL_CONFIGS

DOMAIN = "banking_knowledge"
DISCOVERABLE_WRAPPER = "call_discoverable_agent_tool"
ALLTOOLS_RETRIEVAL_TOOLS = frozenset({"KB_search_bm25", "KB_search_dense", "shell"})


def _plain_toolkit() -> KnowledgeToolsPlain:
    # Tool schema construction does not read the database or execute a tool.
    return KnowledgeToolsPlain(db=None)


def discoverable_tools() -> list[Tool]:
    """Return schemas for the logical tools hidden behind Tau's dispatcher."""
    methods = _plain_toolkit().get_discoverable_tools()
    return [as_tool(methods[name]) for name in sorted(methods)]


def model_tools(retrieval_config: str) -> list[Tool]:
    """Return the actual top-level schemas for a supported AllTools variant."""
    if retrieval_config not in SUPPORTED_RETRIEVAL_CONFIGS:
        raise ValueError(f"unsupported retrieval config: {retrieval_config}")
    toolkit = KnowledgeToolsAllTools(
        db=None,
        kb_bm25_pipeline=None,
        kb_dense_pipeline=None,
        sandbox=None,
    )
    methods = {name: method for name, method in toolkit.tools.items() if not getattr(method, DISCOVERABLE_ATTR, False)}
    return [as_tool(methods[name]) for name in sorted(methods)]


def model_tool_names(retrieval_config: str) -> frozenset[str]:
    """Return the top-level tool names exposed for a supported retrieval config."""
    names = frozenset(tool.name for tool in model_tools(retrieval_config))
    if not ALLTOOLS_RETRIEVAL_TOOLS <= names:
        raise RuntimeError("Tau's AllTools surface is missing a required retrieval tool")
    return names


def policy_tool_names(retrieval_config: str) -> frozenset[str]:
    """Return the exact APPA surface: model tools plus logical hidden tools."""
    return model_tool_names(retrieval_config) | frozenset(tool.name for tool in discoverable_tools())
