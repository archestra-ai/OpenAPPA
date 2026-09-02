import hashlib
import urllib.parse
from dataclasses import asdict
from pathlib import Path

import pytest

from appa_shellrisk import DATASET_REVISION, dataset
from appa_shellrisk.dataset import CommandRow, fetch_test, select_balanced


def row(command: str, label: str, source: str = "source") -> CommandRow:
    digest = hashlib.sha256(command.strip().encode()).hexdigest()
    return CommandRow(f"sha256:{digest}", source, command, command, label)


def test_row_id_commits_to_the_normalized_command() -> None:
    valid = row("  git status  ", "not_risky")
    assert CommandRow.parse(asdict(valid)) == valid

    invalid = asdict(valid)
    invalid["command"] = "rm -rf ~"
    with pytest.raises(ValueError, match="does not match"):
        CommandRow.parse(invalid)


def test_balanced_selection_is_deterministic_and_interleaves_sources() -> None:
    rows = [
        row("safe-a", "not_risky", "a"),
        row("safe-b", "not_risky", "b"),
        row("safe-c", "not_risky", "c"),
        row("risky-a", "risky", "a"),
        row("risky-b", "risky", "b"),
        row("risky-c", "risky", "c"),
        row("safe-extra", "not_risky", "a"),
        row("risky-extra", "risky", "a"),
    ]

    selected = select_balanced(rows, 6)

    assert [(item.label, item.source) for item in selected] == [
        ("not_risky", "a"),
        ("risky", "a"),
        ("not_risky", "b"),
        ("risky", "b"),
        ("not_risky", "c"),
        ("risky", "c"),
    ]
    assert select_balanced(rows, 6) == selected


def test_fetch_pins_revision_pages_and_validates_the_cached_split(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rows = [row("safe-a", "not_risky"), row("risky", "risky"), row("safe-b", "not_risky")]
    monkeypatch.setattr(dataset, "EXPECTED_TEST_ROWS", 3)
    monkeypatch.setattr(dataset, "EXPECTED_RISKY", 1)
    monkeypatch.setattr(dataset, "EXPECTED_NOT_RISKY", 2)
    monkeypatch.setattr(dataset, "PAGE_SIZE", 2)
    requested: list[str] = []

    def get_json(url: str):
        requested.append(url)
        if "/api/datasets/" in url:
            return {"sha": DATASET_REVISION}
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
        offset = int(query["offset"][0])
        length = int(query["length"][0])
        return {
            "num_rows_total": len(rows),
            "rows": [{"row": asdict(item)} for item in rows[offset : offset + length]],
        }

    cache = tmp_path / "test.jsonl"
    assert fetch_test(cache, get_json=get_json) == rows
    assert len(requested) == 3
    assert fetch_test(cache, get_json=lambda _: pytest.fail("cache should be used")) == rows


def test_fetch_refuses_a_moved_dataset_head(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="moved from pinned revision"):
        fetch_test(tmp_path / "test.jsonl", get_json=lambda _: {"sha": "different"})
