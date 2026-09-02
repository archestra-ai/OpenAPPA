"""Pinned ShellRisk-Bench dataset loading and deterministic smoke selection."""

from __future__ import annotations

import hashlib
import json
import urllib.parse
import urllib.request
from collections import defaultdict, deque
from collections.abc import Callable, Iterable
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from . import DATASET_ID, DATASET_REVISION

EXPECTED_TEST_ROWS = 4_194
EXPECTED_RISKY = 193
EXPECTED_NOT_RISKY = 4_001
PAGE_SIZE = 100


@dataclass(frozen=True)
class CommandRow:
    id: str
    source: str
    upstream_id: str
    command: str
    label: str

    @staticmethod
    def parse(value: object) -> CommandRow:
        if not isinstance(value, dict) or set(value) != {"id", "source", "upstream_id", "command", "label"}:
            raise ValueError("a ShellRisk row must have exactly id, source, upstream_id, command, and label")
        if not all(isinstance(value[field], str) for field in value):
            raise ValueError("every ShellRisk row field must be a string")
        row = CommandRow(**value)
        if row.label not in {"risky", "not_risky"}:
            raise ValueError(f"unsupported ShellRisk label {row.label!r}")
        expected_id = "sha256:" + hashlib.sha256(row.command.strip().encode()).hexdigest()
        if row.id != expected_id:
            raise ValueError(f"ShellRisk id does not match its command: {row.id}")
        return row


def _get_json(url: str) -> Any:
    request = urllib.request.Request(url, headers={"User-Agent": "OpenAPPA-ShellRisk-eval/0.1"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def _verify_unique(rows: list[CommandRow]) -> None:
    ids = {row.id for row in rows}
    if len(ids) != len(rows):
        raise ValueError("the ShellRisk rows contain duplicate ids")


def verify_complete_test(rows: list[CommandRow]) -> None:
    _verify_unique(rows)
    risky = sum(row.label == "risky" for row in rows)
    not_risky = sum(row.label == "not_risky" for row in rows)
    actual = (len(rows), risky, not_risky)
    expected = (EXPECTED_TEST_ROWS, EXPECTED_RISKY, EXPECTED_NOT_RISKY)
    if actual != expected:
        raise ValueError(f"ShellRisk test counts do not match v0.1: got {actual}, expected {expected}")


def load_jsonl(path: Path, *, require_complete: bool = False) -> list[CommandRow]:
    rows = [CommandRow.parse(json.loads(line)) for line in path.read_text().splitlines() if line.strip()]
    if require_complete:
        verify_complete_test(rows)
    else:
        _verify_unique(rows)
    return rows


def fetch_test(
    cache: Path,
    *,
    get_json: Callable[[str], Any] = _get_json,
) -> list[CommandRow]:
    """Fetch the public test split only after its Hub head matches the pinned revision."""
    if cache.exists():
        return load_jsonl(cache, require_complete=True)

    metadata = get_json(f"https://huggingface.co/api/datasets/{DATASET_ID}")
    revision = metadata.get("sha") if isinstance(metadata, dict) else None
    if revision != DATASET_REVISION:
        raise ValueError(f"{DATASET_ID} moved from pinned revision {DATASET_REVISION} to {revision!r}")

    encoded = urllib.parse.quote(DATASET_ID, safe="")
    rows: list[CommandRow] = []
    total: int | None = None
    offset = 0
    while total is None or offset < total:
        page = get_json(
            "https://datasets-server.huggingface.co/rows"
            f"?dataset={encoded}&config=default&split=test&offset={offset}&length={PAGE_SIZE}"
        )
        if not isinstance(page, dict) or not isinstance(page.get("rows"), list):
            raise ValueError("the Hugging Face dataset server returned no ShellRisk rows")
        if total is None:
            total = page.get("num_rows_total")
            if not isinstance(total, int):
                raise ValueError("the Hugging Face dataset server returned no test row count")
        entries = page["rows"]
        if not entries and offset < total:
            raise ValueError(f"the Hugging Face dataset server stopped at row {offset} of {total}")
        for entry in entries:
            value = entry.get("row") if isinstance(entry, dict) else None
            rows.append(CommandRow.parse(value))
        offset += len(entries)

    verify_complete_test(rows)
    cache.parent.mkdir(parents=True, exist_ok=True)
    temporary = cache.with_suffix(cache.suffix + ".tmp")
    temporary.write_text("".join(json.dumps(asdict(row), separators=(",", ":")) + "\n" for row in rows))
    temporary.replace(cache)
    return rows


def _round_robin(rows: Iterable[CommandRow], count: int) -> list[CommandRow]:
    groups: dict[str, deque[CommandRow]] = defaultdict(deque)
    for row in rows:
        groups[row.source].append(row)
    selected: list[CommandRow] = []
    sources = sorted(groups)
    while len(selected) < count:
        progressed = False
        for source in sources:
            if groups[source] and len(selected) < count:
                selected.append(groups[source].popleft())
                progressed = True
        if not progressed:
            break
    return selected


def select_balanced(rows: list[CommandRow], limit: int) -> list[CommandRow]:
    """Select a deterministic, approximately balanced, source-interleaved smoke set."""
    if limit < 1:
        raise ValueError("the evaluation limit must be at least one")
    if limit >= len(rows):
        return list(rows)
    risky_count = min(limit // 2, sum(row.label == "risky" for row in rows))
    not_risky_count = limit - risky_count
    risky = _round_robin((row for row in rows if row.label == "risky"), risky_count)
    not_risky = _round_robin((row for row in rows if row.label == "not_risky"), not_risky_count)
    selected = [row for pair in zip(not_risky, risky, strict=False) for row in pair]
    selected.extend(not_risky[len(risky) :])
    selected.extend(risky[len(not_risky) :])
    return selected
