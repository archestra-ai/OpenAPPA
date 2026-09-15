import hashlib
import json
import tarfile
from pathlib import Path

import pytest
import zstandard

from appa_bench_publish import publish as publisher
from appa_bench_publish.publish import Bundle, PublishError, prepare_bundle, relay_bundle

COMMIT = "a" * 40


def archive_names(path: Path) -> list[str]:
    with path.open("rb") as raw, zstandard.ZstdDecompressor().stream_reader(raw) as decompressed:
        with tarfile.open(fileobj=decompressed, mode="r|") as archive:
            return [member.name for member in archive]


def test_prepares_reproducible_contract_bundle(tmp_path: Path) -> None:
    run = tmp_path / "run-20260915"
    run.mkdir()
    (run / "summary.json").write_text('{"score": 1, "endpoint": "https://example.com/tmp/results"}\n')
    records = run / "records"
    records.mkdir()
    (records / "trajectory.jsonl").write_text('{"message": "safe"}\n')

    first = prepare_bundle(run, "corp", COMMIT, tmp_path / "first")
    second = prepare_bundle(run, "corp", COMMIT, tmp_path / "second")

    assert first.sha256 == second.sha256
    assert first.archive.name == f"run-20260915-{first.sha256}.tar.zst"
    assert hashlib.sha256(first.archive.read_bytes()).hexdigest() == first.sha256
    assert archive_names(first.archive) == [
        "run-20260915",
        "run-20260915/records",
        "run-20260915/records/trajectory.jsonl",
        "run-20260915/summary.json",
    ]
    assert json.loads(first.index.read_text()) == {
        "format_version": 1,
        "benchmark": "corp",
        "git_commit": COMMIT,
        "run_id": "run-20260915",
        "archive": first.archive.name,
        "sha256": first.sha256,
    }

    reused = prepare_bundle(run, "corp", COMMIT, tmp_path / "first")
    assert reused == first


def test_refuses_to_replace_bundle_when_run_changed(tmp_path: Path) -> None:
    run = tmp_path / "run-1"
    run.mkdir()
    record = run / "summary.json"
    record.write_text('{"score": 1}\n')
    prepare_bundle(run, "corp", COMMIT, tmp_path / "bundle")
    record.write_text('{"score": 0}\n')

    with pytest.raises(PublishError, match="not this run"):
        prepare_bundle(run, "corp", COMMIT, tmp_path / "bundle")


@pytest.mark.parametrize(
    ("contents", "message"),
    [
        ('{"OPENROUTER_API_KEY":"sk-or-v1-this-is-a-planted-secret"}', "provider credential"),
        ('{"log_file":"/home/alice/OpenAPPA/runs/result.json"}', "absolute local path"),
    ],
)
def test_refuses_sensitive_run_contents(tmp_path: Path, contents: str, message: str) -> None:
    run = tmp_path / "unsafe-run"
    run.mkdir()
    (run / "record.json").write_text(contents)
    output = tmp_path / "bundle"

    with pytest.raises(PublishError, match=message):
        prepare_bundle(run, "corp", COMMIT, output)

    assert list(output.iterdir()) == []


def test_refuses_absolute_path_inside_nested_eval_zip(tmp_path: Path) -> None:
    import zipfile

    run = tmp_path / "unsafe-eval"
    run.mkdir()
    with zipfile.ZipFile(run / "result.eval", "w", compression=zipfile.ZIP_ZSTANDARD) as nested:
        nested.writestr("samples/1.json", '{"working_dir":"C:\\\\Users\\\\alice\\\\OpenAPPA"}')

    with pytest.raises(PublishError, match="absolute local path"):
        prepare_bundle(run, "agentthreatbench", COMMIT, tmp_path / "bundle")


def test_relay_uses_draft_release_and_workflow_contract(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    archive = tmp_path / f"run-1-{'b' * 64}.tar.zst"
    index = tmp_path / "index.json"
    archive.touch()
    index.touch()
    bundle = Bundle(archive, index, "corp", COMMIT, "run-1", "b" * 64)
    calls: list[tuple[str, ...]] = []

    def fake_gh(*args: str) -> str:
        calls.append(args)
        if args[:2] == ("release", "view"):
            raise PublishError("gh release view failed: HTTP 404: release not found")
        if args[:2] == ("workflow", "run"):
            return "https://github.com/archestra-ai/OpenAPPA/actions/runs/123456"
        return ""

    monkeypatch.setattr(publisher, "_gh", fake_gh)

    assert relay_bundle(bundle).endswith("/actions/runs/123456")
    assert calls == [
        ("workflow", "view", "bench-publish.yml", "--repo", "archestra-ai/OpenAPPA"),
        (
            "release",
            "view",
            "bench-relay-corp-run-1-bbbbbbbbbbbb",
            "--repo",
            "archestra-ai/OpenAPPA",
            "--json",
            "isDraft",
            "--jq",
            ".isDraft",
        ),
        (
            "release",
            "create",
            "bench-relay-corp-run-1-bbbbbbbbbbbb",
            "--repo",
            "archestra-ai/OpenAPPA",
            "--draft",
            "--title",
            "Benchmark relay: corp/run-1",
            "--notes",
            "",
        ),
        (
            "release",
            "upload",
            "bench-relay-corp-run-1-bbbbbbbbbbbb",
            str(archive),
            str(index),
            "--repo",
            "archestra-ai/OpenAPPA",
            "--clobber",
        ),
        (
            "workflow",
            "run",
            "bench-publish.yml",
            "--repo",
            "archestra-ai/OpenAPPA",
            "-f",
            "bench=corp",
            "-f",
            f"commit={COMMIT}",
            "-f",
            "run_id=run-1",
            "-f",
            "relay_tag=bench-relay-corp-run-1-bbbbbbbbbbbb",
        ),
    ]


def test_relay_reuses_existing_draft(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    archive = tmp_path / f"run-1-{'b' * 64}.tar.zst"
    index = tmp_path / "index.json"
    bundle = Bundle(archive, index, "corp", COMMIT, "run-1", "b" * 64)
    calls: list[tuple[str, ...]] = []

    def fake_gh(*args: str) -> str:
        calls.append(args)
        if args[:2] == ("release", "view"):
            return "true"
        if args[:2] == ("workflow", "run"):
            return "https://github.com/archestra-ai/OpenAPPA/actions/runs/123456"
        return ""

    monkeypatch.setattr(publisher, "_gh", fake_gh)

    relay_bundle(bundle)

    assert not any(call[:2] == ("release", "create") for call in calls)
    upload = next(call for call in calls if call[:2] == ("release", "upload"))
    assert upload[-1] == "--clobber"


def test_url_lookup_failure_says_dispatch_succeeded(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    bundle = Bundle(tmp_path / "archive.tar.zst", tmp_path / "index.json", "corp", COMMIT, "run-1", "b" * 64)

    def fake_gh(*args: str) -> str:
        if args[:2] == ("release", "view"):
            return "true"
        return ""

    monkeypatch.setattr(publisher, "_gh", fake_gh)
    monkeypatch.setattr(publisher, "_find_workflow_url", lambda _: (_ for _ in ()).throw(PublishError("not found")))

    with pytest.raises(PublishError, match="workflow was dispatched.*run URL lookup failed"):
        relay_bundle(bundle)
