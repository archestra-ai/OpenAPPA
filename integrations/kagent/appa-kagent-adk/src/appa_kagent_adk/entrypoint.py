"""The replacement entrypoint of the appa-kagent-adk image.

The image is a drop-in replacement for the stock kagent runtime image.
An operator can set it as the default agent image of a whole cluster.
``APPA_ENABLED`` selects the mode, and ``main`` logs the mode it serves.

The stock mode is the default. It runs when ``APPA_ENABLED`` is unset,
empty or ``false`` (``build_stock_server``). It makes the stock calls
and adds no delta. An agent that does not opt in runs as it runs on the
stock image, and one WARNING line names it as ungated. The stock mode
ignores ``APPA_RUNTIME_URL``.

The gated mode runs when ``APPA_ENABLED`` is ``true`` (``build_server``).
It replays the stock kagent startup — the same public calls
``kagent.adk.cli.static`` makes, with the same controller args — and
adds the OpenAPPA construction deltas:

1. Refuse what the runtime cannot gate: unknown config fields, compiled
   ``sub_agents``, a divergent compaction summarizer (``config_guard``).
2. Bring the out-of-band flows under the tool gate: wrap the code
   executor and the memory persist callback (``gates``).
3. Rebuild the stock plugin list with the stock conditions, then append
   ``AppaPluginKagent`` last.
4. Append the runtime-owned remedy and battery-matcher toolset over
   ``$APPA_RUNTIME_URL/mcp``.
5. Fill the OpenAI model's ``reasoning_effort`` from
   ``$APPA_KAGENT_OPENAI_REASONING_EFFORT`` when the rendered config
   leaves it unset (``fill_reasoning_effort``).

The gated mode reads its runtime from ``APPA_RUNTIME_URL``. A missing
URL refuses the start: the image never serves an agent it cannot gate
after an operator asked for the gate. Any other ``APPA_ENABLED`` value
refuses the start too, so a typo cannot disable the gate in silence.

Everything else — policy, decisions, remedy plans, trajectory state —
lives in ``appa-runtime``.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
from typing import Any

import pydantic

from .config_guard import ConfigRefused
from .identity import SessionIdentity
from .plugin import AppaPluginKagent
from .wire import RESERVED_TOOL, RUNTIME_TOOLS

logger = logging.getLogger("appa_kagent_adk.entrypoint")

ENABLED_ENV = "APPA_ENABLED"

RUNTIME_URL_ENV = "APPA_RUNTIME_URL"

REASONING_EFFORT_ENV = "APPA_KAGENT_OPENAI_REASONING_EFFORT"

# The closed value set of the knob, compared after a trim and a
# lowercase. Every other value refuses the start.
ENABLED_OFF_VALUES = ("", "false")
ENABLED_ON_VALUE = "true"

# The startup line of each mode. The stock line is a WARNING because
# the image then serves an agent that no policy gates.
GATED_STARTUP = f"{ENABLED_ENV} is true. This agent runs gated by the OpenAPPA runtime at %s"
STOCK_STARTUP = (
    f"{ENABLED_ENV} is not true. This agent runs UNGATED as the stock kagent runtime, "
    f"and no OpenAPPA policy applies. Set {ENABLED_ENV}=true to gate this agent."
)
# The one combination worth naming: a runtime URL that gates nothing.
IGNORED_RUNTIME_URL = (
    f"{RUNTIME_URL_ENV} is set, and this agent ignores it. The agent runs UNGATED because {ENABLED_ENV} is not true."
)
# The gated mode fails closed on a missing runtime.
MISSING_RUNTIME_URL = (
    f"{ENABLED_ENV} is true and {RUNTIME_URL_ENV} is not set. This image gates this agent, "
    f"and it cannot gate without its runtime. Set {RUNTIME_URL_ENV} and restart."
)

# The request timeout of the reserved toolset's MCP client. A remedy
# execution holds `execute_remedy_plan` open for as long as its plan
# takes — a sanitizer's model call, a URL authority parked at a remote
# approval board, the runtime's whole consult window — and ADK's
# default of five seconds would fail the call at the client before the
# runtime answers. This must outlast the runtime's `[externals]`
# consult timeout.
REMEDY_CALL_TIMEOUT_SECONDS = 300.0


def gate_enabled(environ=os.environ) -> bool:
    """Read the one knob that selects the mode of the image.

    The value set is closed. Unset, empty and ``false`` select the stock
    mode, which is the default. ``true`` selects the gated mode. The
    comparison trims the value and ignores case. Any other value raises
    ``ConfigRefused``, because a typo must never disable the gate in
    silence.
    """
    value = environ.get(ENABLED_ENV, "").strip()
    folded = value.lower()
    if folded in ENABLED_OFF_VALUES:
        return False
    if folded == ENABLED_ON_VALUE:
        return True
    raise ConfigRefused(
        f"{ENABLED_ENV} carries {value!r}, which is outside its value set. "
        f"Set {ENABLED_ENV}={ENABLED_ON_VALUE} to gate this agent. Leave it unset, "
        f"or set it to false, to run the stock kagent runtime."
    )


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

    The fill is an OpenAPPA delta, so only the gated mode applies it.
    """
    effort = environ.get(REASONING_EFFORT_ENV, "").strip()
    model = config.get("model")
    if not effort or not isinstance(model, dict) or model.get("type") != "openai":
        return
    if model.get("reasoning_effort") is None:
        model["reasoning_effort"] = effort


def _materialize_config(filepath: str) -> None:
    """Write the rendered config to the config directory when the lane delivers it as env.

    The Substrate lane delivers the config as env. Another lane leaves
    the stock module without this call, and this function does nothing.
    """
    from kagent.adk import cli as stock_cli

    materialize = getattr(stock_cli, "materialize_from_env", None)
    if callable(materialize):
        materialize(filepath)


def _read_document(filepath: str, name: str) -> dict:
    """Read one rendered JSON document from the config directory."""
    with open(os.path.join(filepath, name)) as handle:
        return json.load(handle)


def _stock_plugins(agent_config) -> tuple[Any, list]:
    """The stock plugin list and its STS integration, built by the stock conditions.

    Both modes build this list. The gated mode appends
    ``AppaPluginKagent`` to it. The stock mode serves it as it is.
    """
    from kagent.adk import cli as stock_cli

    plugins = []
    sts_integration = stock_cli.create_sts_integration()
    if sts_integration:
        plugins.append(sts_integration)
    if agent_config.model.api_key_passthrough:
        from kagent.adk._llm_passthrough_plugin import LLMPassthroughPlugin

        plugins.append(LLMPassthroughPlugin())
    return sts_integration, plugins


def _kagent_server(app_cfg, agent_config, agent_card, plugins, root_agent_factory):
    """Build the traced server from the stock arguments. Both modes end here."""
    from kagent.adk import KAgentApp
    from kagent.core import configure_tracing

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


def build_stock_server(filepath: str):
    """Construct the ungated server: the stock kagent construction, and no delta.

    The image replaces the stock runtime image for a whole fleet, so an
    agent that does not set ``APPA_ENABLED`` to ``true`` must run as it
    runs on the stock image. This path makes the calls
    ``kagent.adk.cli.static`` makes. It
    adds no plugin, no gate, no reserved toolset, no config refusal and
    no reasoning-effort fill.

    Raises ``pydantic.ValidationError`` for a config or agent card that
    does not validate. The stock startup fails on that config too.
    """
    from a2a.types import AgentCard
    from kagent.adk import AgentConfig
    from kagent.adk import cli as stock_cli
    from kagent.core import KAgentConfig

    _materialize_config(filepath)
    agent_config = AgentConfig.model_validate(_read_document(filepath, "config.json"))
    agent_card = AgentCard.model_validate(_read_document(filepath, "agent-card.json"))

    app_cfg = KAgentConfig()
    sts_integration, plugins = _stock_plugins(agent_config)

    def root_agent_factory():
        root_agent = agent_config.to_agent(app_cfg.name, sts_integration, stock_cli.propagate_token)
        stock_cli.maybe_add_skills_with_config(root_agent, agent_config)
        return root_agent

    return _kagent_server(app_cfg, agent_config, agent_card, plugins, root_agent_factory)


def build_server(filepath: str, runtime_url: str):
    """Construct the gated FastAPI server for the rendered config.

    Split from ``main`` so tests can build without serving. Raises
    ``ConfigRefused`` for a config this runtime must not run, and
    ``pydantic.ValidationError`` for a config or agent card that does
    not validate.
    """
    from a2a.types import AgentCard
    from kagent.adk import AgentConfig
    from kagent.adk import cli as stock_cli
    from kagent.core import KAgentConfig

    from . import config_guard, gates

    _materialize_config(filepath)
    config = _read_document(filepath, "config.json")
    fill_reasoning_effort(config)
    config_guard.refuse_unsupported(config, AgentConfig)
    agent_config = AgentConfig.model_validate(config)
    config_guard.refuse_divergent_summarizer(agent_config)
    agent_card = AgentCard.model_validate(_read_document(filepath, "agent-card.json"))

    app_cfg = KAgentConfig()
    sts_integration, plugins = _stock_plugins(agent_config)

    identity = SessionIdentity()
    plugin = AppaPluginKagent(runtime_url, identity=identity)
    # Appended last: no stock plugin overrides a gated callback. The
    # callbacks the stock plugins do override (before_run, after_run,
    # before_model) return None, so the chain reaches this plugin.
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
        root_agent.tools.append(_runtime_toolset(runtime_url))
        return root_agent

    return _kagent_server(app_cfg, agent_config, agent_card, plugins, root_agent_factory)


def _runtime_toolset(runtime_url: str):
    """The remedy-only toolset, or appa-guide's isolated management set.

    The agent executes the offered remedies on its own. A blocked call
    answers with the offers, the model chooses one (steered by its
    instruction or the chat), and the plan runs. Human attention is the
    policy's to require, through an authority a plan element names. The
    runtime consults that authority during execution, and the toolset
    itself carries no confirmation gate. The plugin carries the human
    channel. A reserved call that quotes a reviewed offer raises the
    stock kagent tool confirmation, and the resumed call carries the
    ruling (``AppaPluginKagent.before_tool_callback`` and
    IMPLEMENTATION.md, Human review).
    """
    from google.adk.tools.mcp_tool.mcp_session_manager import StreamableHTTPConnectionParams
    from google.adk.tools.mcp_tool.mcp_toolset import McpToolset

    guide = os.environ.get("APPA_GUIDE", "").strip().lower() == "true"
    endpoint = runtime_url.rstrip("/") + "/mcp"
    tools = [RESERVED_TOOL]
    if guide:
        endpoint = os.environ.get("APPA_GUIDE_MCP_URL", "").strip()
        if not endpoint:
            raise ConfigRefused("APPA_GUIDE=true requires a nonempty APPA_GUIDE_MCP_URL")
        tools = RUNTIME_TOOLS
    return McpToolset(
        connection_params=StreamableHTTPConnectionParams(url=endpoint, timeout=REMEDY_CALL_TIMEOUT_SECONDS),
        tool_filter=tools,
    )


def _validation_summary(error: pydantic.ValidationError) -> str:
    """One line for a config that does not validate, without the offending values.

    pydantic's own rendering quotes every input value, and a header or an
    API key can be among them. A message can quote one too: a discriminator
    error echoes the tag it did not recognize. The line carries the count,
    then the location and the error type of the first error. The error
    type is an identifier from pydantic's closed set (``string_type``,
    ``union_tag_invalid``), so it never carries input. An error on the
    whole model has no location, so the line names the model instead.
    """
    first = error.errors()[0]
    location = ".".join(str(part) for part in first["loc"]) or error.title
    return f"the config does not validate: {error.error_count()} error(s); first: {location}: {first['type']}"


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

    runtime_url = os.environ.get(RUNTIME_URL_ENV, "").strip()

    # The knob makes the whole choice, and it makes it once. Every
    # refusal exits 2 with one line on stderr and no traceback: an
    # unknown knob value, a gated mode without its runtime, a config the
    # gated mode cannot gate, and a config that does not validate.
    try:
        if gate_enabled():
            if not runtime_url:
                raise ConfigRefused(MISSING_RUNTIME_URL)
            logger.info(GATED_STARTUP, runtime_url)
            server = build_server(args.filepath, runtime_url)
        else:
            logger.warning(STOCK_STARTUP)
            if runtime_url:
                logger.warning(IGNORED_RUNTIME_URL)
            server = build_stock_server(args.filepath)
    except ConfigRefused as refusal:
        print(f"refusing to start: {refusal}", file=sys.stderr)
        return 2
    except pydantic.ValidationError as invalid:
        print(f"refusing to start: {_validation_summary(invalid)}", file=sys.stderr)
        return 2

    import uvicorn

    log_level = os.environ.get("UVICORN_LOG_LEVEL", os.environ.get("LOG_LEVEL", "info")).lower()
    uvicorn.run(server, host=args.host, port=args.port, workers=args.workers, reload=args.reload, log_level=log_level)
    return 0


if __name__ == "__main__":
    sys.exit(main())
