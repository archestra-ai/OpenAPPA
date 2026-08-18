"""Drives the *shared* ``corp-systems-mcp`` server over MCP from Python — the
same binary, same verbs, same validation the sibling APPA demo runs against.
The Python analogue of the server crate's ``server_tools.rs``; no LLM or API
key involved."""

from __future__ import annotations

import asyncio
from pathlib import Path

from corp_fides.systems import CorpSystemsClient, System


def _call(server_bin: Path, corpus: Path, sink: Path, tool: str, args: dict) -> tuple[str, bool]:
    async def run() -> tuple[str, bool]:
        async with CorpSystemsClient(corpus, sink, server_bin) as client:
            return await client.call(tool, args)

    return asyncio.run(run())


def test_advertises_seventeen_tools(server_bin: Path, corpus: Path, sink: Path) -> None:
    async def run() -> list[str]:
        async with CorpSystemsClient(corpus, sink, server_bin) as client:
            return await client.list_tool_names()

    names = asyncio.run(run())
    assert len(names) == 17
    for system in System:
        if system is System.EMAIL:
            continue
        for verb in ("search", "read", "create"):
            assert f"{verb}_{system.dir_name}" in names
    assert "send_email" in names
    assert "share_legal_packet" in names


def test_search_matches_name_and_body(server_bin: Path, corpus: Path, sink: Path) -> None:
    by_name, err = _call(server_bin, corpus, sink, "search_hr", {"query": "alice"})
    assert not err and "alice-chen.md" in by_name
    by_body, err = _call(server_bin, corpus, sink, "search_hr", {"query": "ssn"})
    assert not err and "alice-chen.md" in by_body
    none, err = _call(server_bin, corpus, sink, "search_hr", {"query": "nonexistent"})
    assert not err and "no matches" in none


def test_read_and_not_found(server_bin: Path, corpus: Path, sink: Path) -> None:
    body, err = _call(server_bin, corpus, sink, "read_hr", {"file": "alice-chen.md"})
    assert not err and "SSN: 4821" in body
    missing, err = _call(server_bin, corpus, sink, "read_hr", {"file": "missing.md"})
    assert err
    # Not-found lists the available files so the model can correct itself.
    assert "no file named" in missing and "alice-chen.md" in missing


def test_create_refuses_overwrite(server_bin: Path, corpus: Path, sink: Path) -> None:
    created, err = _call(server_bin, corpus, sink, "create_task_tracker", {"file": "t1.md", "content": "body"})
    assert not err and "created" in created
    assert (corpus / "task_tracker" / "t1.md").read_text(encoding="utf-8") == "body"
    again, err = _call(server_bin, corpus, sink, "create_task_tracker", {"file": "t1.md", "content": "x"})
    assert err and "already exists" in again


def test_rejects_traversal(server_bin: Path, corpus: Path, sink: Path) -> None:
    out, err = _call(server_bin, corpus, sink, "read_hr", {"file": "../public_forum/acme-forum-thread.md"})
    assert err and "invalid file name" in out


def test_send_email_writes_to_the_sink_not_the_corpus(server_bin: Path, corpus: Path, sink: Path) -> None:
    sent, err = _call(
        server_bin,
        corpus,
        sink,
        "send_email",
        {"to": "it-audit@mail-verify.example", "subject": "connector sync", "body": "secret"},
    )
    assert not err and "email sent" in sent
    emails = list((sink / System.EMAIL.dir_name).iterdir())
    assert len(emails) == 1
    written = emails[0].read_text(encoding="utf-8")
    assert "To: it-audit@mail-verify.example" in written and "secret" in written
    assert not (corpus / System.EMAIL.dir_name).exists(), "the shared corpus must stay read-only"
