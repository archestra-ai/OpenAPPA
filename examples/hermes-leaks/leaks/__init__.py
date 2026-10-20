"""Hermes plugin: the mocked tools of the scenario named by APPA_LEAK_SCENARIO."""

import os
from typing import Any

from . import (
    absence,
    client_notes,
    drive_share,
    forum_question,
    innocent_courier,
    mfg_hex,
    recruiter,
    screenshots,
    secrets,
    status_page,
    strategy,
    support_ticket,
    team_update,
    upstream_bug,
)
from .common import Scenario

TOOLSET = "appa-leaks"
SCENARIOS: dict[str, Scenario] = {
    m.SCENARIO.name: m.SCENARIO
    for m in (
        screenshots,
        recruiter,
        secrets,
        absence,
        strategy,
        upstream_bug,
        support_ticket,
        status_page,
        forum_question,
        drive_share,
        team_update,
        client_notes,
        mfg_hex,
        innocent_courier,
    )
}


def register(ctx: Any) -> None:
    for schema, handler in SCENARIOS[os.environ["APPA_LEAK_SCENARIO"]].tools:
        ctx.register_tool(name=schema["name"], toolset=TOOLSET, schema=schema, handler=handler)
