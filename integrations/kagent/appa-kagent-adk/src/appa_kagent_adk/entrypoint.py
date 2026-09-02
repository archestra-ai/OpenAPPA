"""The replacement entrypoint of the appa-kagent-adk image.

It replays the stock kagent startup — the same public calls
``kagent.adk.cli.static`` makes, with the same controller args — and
adds the OpenAPPA construction deltas:

1. Refuse what the runtime cannot gate: unknown config fields, compiled
   ``sub_agents``, a divergent compaction summarizer (``config_guard``).
2. Bring the out-of-band flows under the tool gate: wrap the code
   executor and the memory persist callback (``gates``).
3. Rebuild the stock plugin list with the stock conditions, then append
   ``AppaPluginKagent`` last.
4. Append the reserved-tool toolset over ``$APPA_RUNTIME_URL/mcp``.
5. Fill the OpenAI model's ``reasoning_effort`` from
   ``$APPA_KAGENT_OPENAI_REASONING_EFFORT`` when the rendered config
   leaves it unset (``fill_reasoning_effort``).

Everything else — policy, decisions, remedy plans, trajectory state —
lives in ``appa-runtime``.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys

from .identity import SessionIdentity
from .plugin import AppaPluginKagent
from .wire import RESERVED_TOOL

logger = logging.getLogger("appa_kagent_adk.entrypoint")

REASONING_EFFORT_ENV = "APPA_KAGENT_OPENAI_REASONING_EFFORT"

# The request timeout of the reserved toolset's MCP client. A remedy
# execution holds `execute_remedy_plan` open for as long as its plan
# takes — a sanitizer's model call, a URL authority parked at a remote
# approval board, the runtime's whole consult window — and ADK's
# default of five seconds would fail the call at the client before the
# runtime answers. This must outlast the runtime's `[externals]`
# consult timeout.
REMEDY_CALL_TIMEOUT_SECONDS = 300.0


def fill_reasoning_effort(config: dict, environ=os.environ) -> None:
    """Fill the OpenAI model's ``reasoning_effort`` from the image env.

    The v1alpha2 ``ModelConfig`` admits ``minimal``, ``low``, ``medium``
    and ``high`` — and no ``none``. Some OpenAI models refuse function
    tools on chat completions unless the request carries
    ``reasoning_effort: "none"``, so a declarative agent on such a model
    cannot make a single tool call. This image setting supplies the
    value the CRD cannot express. A value the CRD did set wins, and a
    model of another type is untouched; kagent-adk passes the string
    through to the API as-is.
    """
    effort = environ.get(REASONING_EFFORT_ENV, "").strip()
    model = config.get("model")
    if not effort or not isinstance(model, dict) or model.get("type") != "openai":
        return
    if model.get("reasoning_effort") is None:
        model["reasoning_effort"] = effort


def build_server(filepath: str, runtime_url: str):
    """Construct the gated FastAPI server for the rendered config.

    Split from ``main`` so tests can build without serving. Raises
    ``ConfigRefused`` for a config this runtime must not run.
    """
    from a2a.types import AgentCard
    from kagent.adk import AgentConfig, KAgentApp
    from kagent.adk import cli as stock_cli
    from kagent.core import KAgentConfig, configure_tracing

    from . import config_guard, gates

    # The Substrate lane delivers the config as env; a no-op elsewhere.
    materialize = getattr(stock_cli, "materialize_from_env", None)
    if callable(materialize):
        materialize(filepath)

    with open(os.path.join(filepath, "config.json")) as handle:
        config = json.load(handle)
    fill_reasoning_effort(config)
    config_guard.refuse_unsupported(config, AgentConfig)
    agent_config = AgentConfig.model_validate(config)
    config_guard.refuse_divergent_summarizer(agent_config)
    with open(os.path.join(filepath, "agent-card.json")) as handle:
        agent_card = AgentCard.model_validate(json.load(handle))

    app_cfg = KAgentConfig()

    # The stock plugin list, built by the stock conditions.
    plugins = []
    sts_integration = stock_cli.create_sts_integration()
    if sts_integration:
        plugins.append(sts_integration)
    if agent_config.model.api_key_passthrough:
        from kagent.adk._llm_passthrough_plugin import LLMPassthroughPlugin

        plugins.append(LLMPassthroughPlugin())

    identity = SessionIdentity()
    plugin = AppaPluginKagent(runtime_url, identity=identity)
    # Appended last: no stock plugin answers a gated callback, and the
    # equivalence tests hold that per version.
    plugins.append(plugin)

    def root_agent_factory():
        root_agent = agent_config.to_agent(app_cfg.name, sts_integration, stock_cli.propagate_token)
        stock_cli.maybe_add_skills_with_config(root_agent, agent_config)
        if root_agent.code_executor is not None:
            root_agent.code_executor = gates.GatedCodeExecutor(
                root_agent.code_executor, gates.SyncHookGate(runtime_url, identity)
            )
        if gates.gate_memory_persist(root_agent, plugin):
            logger.info("the memory persist callback crosses the tool gate")
        root_agent.tools.append(_reserved_toolset(runtime_url))
        return root_agent

    kagent_app = KAgentApp(
        root_agent_factory,
        agent_card,
        app_cfg.url,
        app_cfg.app_name,
        plugins=plugins,
        stream=agent_config.stream if agent_config.stream is not None else False,
        agent_config=agent_config,
    )
    server = kagent_app.build()
    configure_tracing(app_cfg.name, app_cfg.namespace, server)
    return server


def _reserved_toolset(runtime_url: str):
    """The reserved-tool toolset: `execute_remedy_plan` over /mcp.

    The agent executes the remedies it is offered on its own: a blocked
    call answers with the offers, the model chooses one — steered by
    its instruction or the chat — and the plan runs. Human attention is
    the policy's to require, through an authority a plan element names;
    the runtime consults that authority during execution, and the tool
    itself carries no confirmation gate. A kagent-native channel for a
    human authority is an open design item (IMPLEMENTATION.md, Human
    review).
    """
    from google.adk.tools.mcp_tool.mcp_session_manager import StreamableHTTPConnectionParams
    from google.adk.tools.mcp_tool.mcp_toolset import McpToolset

    return McpToolset(
        connection_params=StreamableHTTPConnectionParams(
            url=runtime_url.rstrip("/") + "/mcp", timeout=REMEDY_CALL_TIMEOUT_SECONDS
        ),
        tool_filter=[RESERVED_TOOL],
    )


def main() -> int:
    from kagent.core import configure_logging

    configure_logging()
    parser = argparse.ArgumentParser(prog="appa-kagent-adk")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--workers", type=int, default=1)
    parser.add_argument("--filepath", default="/config")
    parser.add_argument("--reload", action="store_true")
    args = parser.parse_args()

    runtime_url = os.environ.get("APPA_RUNTIME_URL", "")
    if not runtime_url:
        print("APPA_RUNTIME_URL is not set: this image gates every agent, and it cannot", file=sys.stderr)
        print("gate without its runtime. Set APPA_RUNTIME_URL and restart.", file=sys.stderr)
        return 2

    from .config_guard import ConfigRefused

    try:
        server = build_server(args.filepath, runtime_url)
    except ConfigRefused as refusal:
        print(f"refusing to start: {refusal}", file=sys.stderr)
        return 2

    import uvicorn

    log_level = os.environ.get("UVICORN_LOG_LEVEL", os.environ.get("LOG_LEVEL", "info")).lower()
    uvicorn.run(server, host=args.host, port=args.port, workers=args.workers, reload=args.reload, log_level=log_level)
    return 0


if __name__ == "__main__":
    sys.exit(main())
