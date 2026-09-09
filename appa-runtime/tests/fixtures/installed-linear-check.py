#!/usr/bin/env python3
"""Exercise a registered Claude plugin and running runtime, without a model.

Run inside an isolated Claude profile after installing the Linear and GitHub batteries with the Linear example mappings:
    python3 installed-linear-check.py <installed-plugin-root> <runtime-url>

Uses the installed hook commands, not repository hooks or an in-process engine.
Tool responses are fixtures; this does not execute GitHub tools or prove MCP
connectivity. No credentials or external service are required.
Requires the MCP Python SDK (available in the kagent test environment).
"""

import asyncio
from datetime import timedelta
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

from mcp import ClientSession
from mcp.client.streamable_http import streamablehttp_client


async def main():
    plugin = Path(sys.argv[1]).resolve(strict=True)
    hooks = json.loads((plugin / "hooks/hooks.json").read_text())["hooks"]
    environment = dict(os.environ, APPA_GATE="1", CLAUDE_PLUGIN_ROOT=str(plugin))
    session = f"installed-linear-{uuid.uuid4().hex}"

    def event(name, **fields):
        hook = hooks[name][0]["hooks"][0]
        result = subprocess.run(
            ["sh", "-c", hook["command"]],
            env=environment,
            input=json.dumps(dict(hook_event_name=name, session_id=session, **fields)),
            capture_output=True,
            text=True,
            timeout=30,
            check=True,
        )
        return json.loads(result.stdout) if result.stdout.strip() else {}

    event("SessionStart", source="startup")
    event("UserPromptSubmit", prompt="Read the restricted fixture Linear issue, then propose a write.")
    read = dict(tool_name="mcp__linear__get_issue", tool_use_id="fixture-read",
                tool_input=dict(id="ENG-1"))
    answer = event("PreToolUse", **read)
    assert answer["hookSpecificOutput"]["permissionDecision"] == "deny", answer
    offers = re.findall(r'offer_id: "([^"]+)"', answer["hookSpecificOutput"]["permissionDecisionReason"])
    assert len(offers) == 1, answer
    control = dict(tool_name="mcp__plugin_appa-runtime_appa__execute_remedy_plan",
                   tool_use_id="fixture-remedy", tool_input=dict(offer_id=offers[0]))
    event("PreToolUse", **control)
    budget = timedelta(seconds=10)
    async with streamablehttp_client(f"{sys.argv[2]}/mcp", timeout=budget) as (reader, writer, _):
        async with ClientSession(reader, writer) as client:
            await client.initialize()
            remedy = await client.call_tool("execute_remedy_plan", control["tool_input"], read_timeout_seconds=budget)
    assert not remedy.isError, remedy
    event("PostToolUse", **control, tool_response=remedy.model_dump(mode="json"))
    answer = event("PreToolUse", **read)
    assert answer["hookSpecificOutput"]["permissionDecision"] == "allow", answer
    event("PostToolUse", **read, tool_response=dict(body="Untrusted issue content."))
    answer = event("PreToolUse", tool_name="mcp__github__issue_write", tool_use_id="fixture-write",
                   tool_input=dict(owner="fixture", repo="example", issue_number=1,
                                   method="update", body="Untrusted issue content."))
    assert answer["hookSpecificOutput"]["permissionDecision"] == "deny", answer
    event("Stop", stop_hook_active=False, last_assistant_message="The write was refused.")
    print(json.dumps(dict(status="ok", session=session,
                          trust_downgrade="explicitly_accepted", linear_read="allowed", github_write="denied")))


if __name__ == "__main__":
    asyncio.run(main())
