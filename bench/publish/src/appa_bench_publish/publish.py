"""Build deterministic benchmark archives and relay them through GitHub Actions."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from collections.abc import Iterator
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path, PurePosixPath
from typing import BinaryIO

import zstandard

if sys.version_info < (3, 14):
    import zipfile_zstd  # type: ignore[import-not-found, import-untyped]  # noqa: F401

REPOSITORY = "archestra-ai/OpenAPPA"
WORKFLOW = "bench-publish.yml"
BENCH_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
RUN_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
PROVIDER_TOKEN_RE = re.compile(rb"(?<![A-Za-z0-9_-])sk-(?:ant-|or-)?[A-Za-z0-9_-]{16,}", re.IGNORECASE)
CREDENTIAL_ASSIGNMENT_RE = re.compile(
    rb"(?:OPENROUTER_API_KEY|ANTHROPIC_API_KEY|OPENAI_API_KEY|GOOGLE_API_KEY|"
    rb"api[_-]?key|access[_-]?token)[\"']?\s*[:=]\s*[\"']?"
    rb"(?!null\b|none\b|redacted\b|unset\b|<redacted>)[^\s\"',}]{8,}",
    re.IGNORECASE,
)
LOCAL_PATH_RE = re.compile(
    rb"(?:(?:file://|(?<![A-Za-z0-9:/]))/(?:(?:home|Users|workspace|tmp|root)(?:/|\\)|"
    rb"var/tmp(?:/|\\))[^\s\"'<>]*|"
    rb"(?<![A-Za-z0-9])[A-Za-z]:\\(?:[^\s\"'<>]+\\)*[^\s\"'<>]*)"
)


class PublishError(ValueError):
    """A benchmark run cannot be safely packaged or relayed."""


@dataclass(frozen=True)
class Bundle:
    archive: Path
    index: Path
    benchmark: str
    git_commit: str
    run_id: str
    sha256: str


def add_publish_parser(commands: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:
    parser = commands.add_parser("publish", help="package a completed run and publish it through GitHub Actions")
    parser.add_argument("run_dir", type=Path, help="completed benchmark run directory")
    parser.add_argument("--commit", help="full lowercase Git commit for the run (default: recorded run commit)")
    parser.add_argument("--output-dir", type=Path, help="bundle directory (default: RUN_DIR.parent/publish/RUN_ID)")
    parser.add_argument(
        "--prepare-only",
        action="store_true",
        help="write the archive and index without creating a release or dispatching the workflow",
    )


def publish_from_args(args: argparse.Namespace, benchmark: str) -> Bundle:
    bundle = prepare_bundle(args.run_dir, benchmark, args.commit, args.output_dir)
    if args.prepare_only:
        print(f"Prepared {bundle.archive}")
        print(f"Prepared {bundle.index}")
        return bundle

    workflow_url = relay_bundle(bundle)
    print(f"Workflow run: {workflow_url}")
    return bundle


def prepare_bundle(run_dir: Path, benchmark: str, git_commit: str | None, output_dir: Path | None = None) -> Bundle:
    run_dir = run_dir.resolve()
    if not run_dir.is_dir():
        raise PublishError(f"run directory does not exist: {run_dir}")
    if not BENCH_RE.fullmatch(benchmark):
        raise PublishError("benchmark must contain lowercase letters, digits, or hyphens")
    if git_commit is not None and not COMMIT_RE.fullmatch(git_commit):
        raise PublishError("commit must be a full lowercase Git SHA")
    recorded_commit = _recorded_commit(run_dir, benchmark)
    if git_commit is not None and git_commit != recorded_commit:
        raise PublishError(f"requested commit {git_commit} does not match the run's recorded commit {recorded_commit}")
    git_commit = recorded_commit
    run_id = run_dir.name
    if not RUN_ID_RE.fullmatch(run_id):
        raise PublishError("run directory name contains unsupported run-id characters")

    destination = (output_dir or run_dir.parent / "publish" / run_id).resolve()
    if destination == run_dir or run_dir in destination.parents:
        raise PublishError("output directory must be outside the run directory")
    destination.mkdir(parents=True, exist_ok=True)
    existing = list(destination.iterdir())

    entries = list(_run_entries(run_dir))
    if not entries:
        raise PublishError("run directory is empty")

    temporary = destination / f".{run_id}.tar.zst.tmp"
    try:
        _write_archive(temporary, run_dir, run_id, entries)
        _scan_archive(temporary)
        digest = _sha256(temporary)
        archive = destination / f"{run_id}-{digest}.tar.zst"
        payload = {
            "format_version": 1,
            "benchmark": benchmark,
            "git_commit": git_commit,
            "run_id": run_id,
            "archive": archive.name,
            "sha256": digest,
        }
        index = destination / "index.json"
        if existing:
            bundle = Bundle(archive, index, benchmark, git_commit, run_id, digest)
            _verify_existing_bundle(bundle, payload, existing)
            temporary.unlink()
            return bundle

        temporary.replace(archive)
        index.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    except Exception:
        temporary.unlink(missing_ok=True)
        if not existing:
            for child in destination.iterdir():
                child.unlink()
        raise

    return Bundle(archive, index, benchmark, git_commit, run_id, digest)


def relay_bundle(bundle: Bundle) -> str:
    tag = f"bench-relay-{bundle.benchmark}-{bundle.run_id}-{bundle.sha256[:12]}"
    title = f"Benchmark relay: {bundle.benchmark}/{bundle.run_id}"
    _gh("workflow", "view", WORKFLOW, "--repo", REPOSITORY)
    try:
        release_state = _gh("release", "view", tag, "--repo", REPOSITORY, "--json", "isDraft", "--jq", ".isDraft")
    except PublishError as error:
        if "not found" not in str(error).lower() and "404" not in str(error):
            raise
        _gh("release", "create", tag, "--repo", REPOSITORY, "--draft", "--title", title, "--notes", "")
    else:
        if release_state != "true":
            raise PublishError(f"relay tag {tag} identifies a published release and cannot be reused")
    try:
        _gh("release", "upload", tag, str(bundle.archive), str(bundle.index), "--repo", REPOSITORY, "--clobber")
        dispatched_after = time.time() - 5
        output = _gh(
            "workflow",
            "run",
            WORKFLOW,
            "--repo",
            REPOSITORY,
            "-f",
            f"bench={bundle.benchmark}",
            "-f",
            f"commit={bundle.git_commit}",
            "-f",
            f"run_id={bundle.run_id}",
            "-f",
            f"relay_tag={tag}",
        )
    except Exception as error:
        raise PublishError(
            f"relay release {tag} was left in place after publication failed; rerun to reuse it, "
            f"or delete it with `gh release delete {tag} --yes`: {error}"
        ) from error

    match = re.search(r"https://github\.com/[^\s]+/actions/runs/\d+", output)
    if match:
        return match.group(0)
    try:
        return _find_workflow_url(dispatched_after)
    except PublishError as error:
        raise PublishError(
            f"workflow was dispatched for relay {tag}, but its run URL lookup failed: {error}"
        ) from error


def _verify_existing_bundle(bundle: Bundle, payload: dict[str, object], existing: list[Path]) -> None:
    expected = {bundle.archive, bundle.index}
    if set(existing) != expected or not bundle.archive.is_file() or not bundle.index.is_file():
        raise PublishError(
            f"existing output is not this run's bundle; remove it or choose --output-dir: {bundle.index.parent}"
        )
    try:
        current_index = json.loads(bundle.index.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PublishError(f"existing bundle has an unreadable index: {bundle.index}") from error
    if current_index != payload or _sha256(bundle.archive) != bundle.sha256:
        raise PublishError(
            f"existing output does not match this run; remove it or choose --output-dir: {bundle.index.parent}"
        )


def _recorded_commit(run_dir: Path, benchmark: str) -> str:
    match benchmark:
        case "corp":
            path = run_dir / "config.json"
            provenance_path: tuple[str, ...] = ()
        case "agentthreatbench":
            path = run_dir / "run-config.json"
            provenance_path = ("config",)
        case _:
            raise PublishError(f"unsupported benchmark: {benchmark}")

    try:
        provenance: object = json.loads(path.read_text(encoding="utf-8"))
        for key in provenance_path:
            if not isinstance(provenance, dict):
                raise KeyError(key)
            provenance = provenance[key]
        if not isinstance(provenance, dict):
            raise KeyError("git_sha")
        commit = provenance["git_sha"]
        dirty = provenance["git_dirty"]
    except (OSError, KeyError, json.JSONDecodeError) as error:
        raise PublishError(f"run does not contain readable Git provenance: {path}") from error
    if not isinstance(commit, str) or not COMMIT_RE.fullmatch(commit) or not isinstance(dirty, bool):
        raise PublishError(f"run contains an invalid recorded Git commit: {path}")
    if dirty:
        raise PublishError(f"run was produced from a dirty Git worktree and cannot be attributed to {commit}")
    return commit


def _gh(*args: str) -> str:
    try:
        result = subprocess.run(["gh", *args], capture_output=True, text=True, check=False)
    except FileNotFoundError as error:
        raise PublishError("gh is required to relay a benchmark bundle") from error
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit status {result.returncode}"
        raise PublishError(f"gh {' '.join(args[:2])} failed: {detail}")
    return result.stdout.strip()


def _find_workflow_url(not_before: float) -> str:
    for _ in range(10):
        output = _gh(
            "run",
            "list",
            "--repo",
            REPOSITORY,
            "--workflow",
            WORKFLOW,
            "--event",
            "workflow_dispatch",
            "--limit",
            "1",
            "--json",
            "createdAt,url",
        )
        runs = json.loads(output)
        if runs:
            created = datetime.fromisoformat(runs[0]["createdAt"].replace("Z", "+00:00")).timestamp()
            if created >= not_before:
                return str(runs[0]["url"])
        time.sleep(1)
    raise PublishError("workflow was dispatched, but its run URL could not be resolved")


def _run_entries(run_dir: Path) -> Iterator[tuple[Path, str]]:
    for path in sorted(run_dir.rglob("*"), key=lambda item: item.relative_to(run_dir).as_posix()):
        relative = path.relative_to(run_dir).as_posix()
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise PublishError(f"run contains an unsupported filesystem entry: {relative}")
        yield path, relative


def _write_archive(destination: Path, run_dir: Path, run_id: str, entries: list[tuple[Path, str]]) -> None:
    compressor = zstandard.ZstdCompressor(level=19, write_checksum=True, write_content_size=False, threads=0)
    with destination.open("wb") as raw, compressor.stream_writer(raw, closefd=False) as compressed:
        with tarfile.open(fileobj=compressed, mode="w|") as archive:
            root = tarfile.TarInfo(run_id)
            root.type = tarfile.DIRTYPE
            root.mode = 0o755
            _normalize_tar_info(root)
            archive.addfile(root)
            for path, relative in entries:
                info = tarfile.TarInfo(f"{run_id}/{relative}")
                info.mode = 0o755 if path.is_dir() else 0o644
                _normalize_tar_info(info)
                if path.is_dir():
                    info.type = tarfile.DIRTYPE
                    archive.addfile(info)
                else:
                    info.size = path.stat().st_size
                    with path.open("rb") as source:
                        archive.addfile(info, source)


def _normalize_tar_info(info: tarfile.TarInfo) -> None:
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""


def _scan_archive(path: Path) -> None:
    decompressor = zstandard.ZstdDecompressor()
    with path.open("rb") as raw, decompressor.stream_reader(raw) as decompressed:
        with tarfile.open(fileobj=decompressed, mode="r|") as archive:
            for member in archive:
                _check_sensitive(member.name, member.name.encode())
                if not member.isfile():
                    continue
                source = archive.extractfile(member)
                if source is None:
                    raise PublishError(f"cannot inspect archive member {member.name}")
                _scan_stream(member.name, source)


def _scan_stream(name: str, source: BinaryIO, depth: int = 0) -> None:
    if depth > 3:
        raise PublishError(f"run contains archives nested more than three levels deep in {name}")
    with tempfile.SpooledTemporaryFile(max_size=8 * 1024 * 1024) as copy:
        previous = b""
        while chunk := source.read(1024 * 1024):
            copy.write(chunk)
            window = previous + chunk
            _check_sensitive(name, window)
            previous = window[-1024:]
        copy.seek(0)
        if zipfile.is_zipfile(copy):
            _scan_zip(name, copy, depth)


def _scan_zip(name: str, source: BinaryIO, depth: int) -> None:
    source.seek(0)
    with zipfile.ZipFile(source) as nested:
        for member in nested.infolist():
            member_path = PurePosixPath(member.filename)
            _check_sensitive(f"{name}!{member.filename}", member.filename.encode())
            if member.is_dir():
                continue
            if member_path.is_absolute() or ".." in member_path.parts:
                raise PublishError(f"nested archive contains an unsafe member name in {name}: {member.filename}")
            with nested.open(member) as child:
                _scan_stream(f"{name}!{member.filename}", child, depth + 1)


def _check_sensitive(name: str, data: bytes) -> None:
    if PROVIDER_TOKEN_RE.search(data) or CREDENTIAL_ASSIGNMENT_RE.search(data):
        raise PublishError(f"run contains a provider credential in {name}")
    if LOCAL_PATH_RE.search(data):
        raise PublishError(f"run contains an absolute local path in {name}")


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()
