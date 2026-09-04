#!/usr/bin/env python3
"""Install batteries from the latest verified OpenAPPA release."""

from __future__ import annotations

import argparse
import hashlib
import hmac
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
from typing import Callable
from urllib.parse import urlsplit
from urllib.request import Request, urlopen


MAX_CHECKSUM_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 64 * 1024 * 1024
MAX_FILES = 4096
MAX_EXTRACTED_BYTES = 32 * 1024 * 1024
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
RELEASE_TAG = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")


class RefreshError(Exception):
    """A refusal that leaves the installed release batteries unchanged."""


def download(url: str, limit: int, opener: Callable = urlopen) -> bytes:
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "appa-refresh-batteries",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if urlsplit(url).hostname == "api.github.com" and (token := os.environ.get("GITHUB_TOKEN")):
        headers["Authorization"] = f"Bearer {token}"
    request = Request(url, headers=headers)
    try:
        with opener(request, timeout=30) as response:
            chunks = []
            size = 0
            while chunk := response.read(min(1024 * 1024, limit + 1 - size)):
                size += len(chunk)
                if size > limit:
                    raise RefreshError(f"download exceeds {limit} bytes: {url}")
                chunks.append(chunk)
    except RefreshError:
        raise
    except Exception as error:
        raise RefreshError(f"cannot download {url}: {error}") from error
    return b"".join(chunks)


def release_assets(repository: str, tag: str | None = None, opener: Callable = urlopen) -> tuple[str, str, str]:
    if not REPOSITORY.fullmatch(repository):
        raise RefreshError(f"invalid GitHub repository: {repository}")
    if tag is not None and not RELEASE_TAG.fullmatch(tag):
        raise RefreshError(f"release tag is not stable semver: {tag!r}")
    release_path = f"tags/{tag}" if tag else "latest"
    endpoint = f"https://api.github.com/repos/{repository}/releases/{release_path}"
    try:
        release = json.loads(download(endpoint, MAX_CHECKSUM_BYTES, opener))
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise RefreshError("the latest GitHub release response is not valid JSON") from error
    if not isinstance(release, dict):
        raise RefreshError("the latest GitHub release response is not an object")
    resolved_tag = release.get("tag_name")
    if not isinstance(resolved_tag, str) or not RELEASE_TAG.fullmatch(resolved_tag):
        raise RefreshError(f"the resolved release tag is not stable semver: {resolved_tag!r}")
    if tag is not None and resolved_tag != tag:
        raise RefreshError(f"GitHub resolved {tag} as unexpected tag {resolved_tag}")
    version = resolved_tag.removeprefix("v")
    assets = release.get("assets")
    if not isinstance(assets, list):
        raise RefreshError("the latest GitHub release assets field is not a list")
    names = {
        asset.get("name"): asset.get("browser_download_url")
        for asset in assets
        if isinstance(asset, dict)
    }
    archive_name = f"appa-plugin-{version}.tar.gz"
    checksums_url = names.get("SHA256SUMS")
    archive_url = names.get(archive_name)
    if not isinstance(checksums_url, str) or not isinstance(archive_url, str):
        raise RefreshError(f"release {resolved_tag} does not carry SHA256SUMS and {archive_name}")
    release_root = f"https://github.com/{repository}/releases/download/{resolved_tag}"
    if checksums_url != f"{release_root}/SHA256SUMS" or archive_url != f"{release_root}/{archive_name}":
        raise RefreshError(f"release {resolved_tag} carries an unexpected asset URL")
    return resolved_tag, checksums_url, archive_url


def expected_digest(checksums: bytes, archive_name: str) -> str:
    try:
        lines = checksums.decode("utf-8").splitlines()
    except UnicodeDecodeError as error:
        raise RefreshError("SHA256SUMS is not UTF-8") from error
    matches = []
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64})  \*?([^/]+)", line)
        if match and match.group(2) == archive_name:
            matches.append(match.group(1))
    if len(matches) != 1:
        raise RefreshError(f"SHA256SUMS must name {archive_name} exactly once")
    return matches[0]


def verify_archive(archive: bytes, checksums: bytes, archive_name: str) -> None:
    expected = expected_digest(checksums, archive_name)
    actual = hashlib.sha256(archive).hexdigest()
    if not hmac.compare_digest(actual, expected):
        raise RefreshError(f"checksum mismatch for {archive_name}")


def extract_batteries(archive: bytes, destination: Path) -> None:
    count = 0
    extracted_bytes = 0
    battery_configs = 0
    seen = set()
    try:
        package = tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz")
    except tarfile.TarError as error:
        raise RefreshError(f"plugin archive is not a readable tarball: {error}") from error
    with package:
        for member in package:
            path = PurePosixPath(member.name)
            parts = tuple(part for part in path.parts if part != ".")
            if path.is_absolute() or ".." in parts:
                raise RefreshError(f"plugin archive has an unsafe path: {member.name}")
            if not parts or parts[0] != "batteries":
                continue
            relative = parts[1:]
            if not relative:
                continue
            count += 1
            if count > MAX_FILES:
                raise RefreshError(f"plugin archive has more than {MAX_FILES} battery entries")
            if relative in seen:
                raise RefreshError(f"plugin archive repeats batteries/{'/'.join(relative)}")
            seen.add(relative)
            target = destination.joinpath(*relative)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                raise RefreshError(f"plugin archive battery entry is not a regular file: {member.name}")
            extracted_bytes += member.size
            if extracted_bytes > MAX_EXTRACTED_BYTES:
                raise RefreshError(f"battery files exceed {MAX_EXTRACTED_BYTES} extracted bytes")
            source = package.extractfile(member)
            if source is None:
                raise RefreshError(f"plugin archive cannot read {member.name}")
            target.parent.mkdir(parents=True, exist_ok=True)
            with source, target.open("xb") as output:
                shutil.copyfileobj(source, output, length=1024 * 1024)
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
            if len(relative) == 2 and relative[1] == "appa.toml":
                battery_configs += 1
    if battery_configs == 0:
        raise RefreshError("plugin archive carries no battery appa.toml files")


def previous_path(target: Path) -> Path:
    return target.with_name(f".{target.name}.previous")


def validate_staged_config(
    config: Path,
    target: Path,
    staging: Path,
    battery_dirs: list[Path] | None = None,
) -> None:
    if battery_dirs is None:
        raw_dirs = os.environ.get("APPA_BATTERIES_DIR", "")
        battery_dirs = [Path(item) for item in raw_dirs.split(":") if item]
    else:
        battery_dirs = list(battery_dirs)
    try:
        target_index = battery_dirs.index(target)
    except ValueError as error:
        raise RefreshError(f"APPA_BATTERIES_DIR does not contain refresh target {target}") from error
    battery_dirs[target_index] = staging
    command = ["appa", "describe", "--check", "--config", str(config)]
    for directory in battery_dirs:
        command.extend(("--batteries-dir", str(directory)))
    environment = os.environ.copy()
    environment.pop("APPA_BATTERIES_DIR", None)
    try:
        checked = subprocess.run(
            command,
            capture_output=True,
            check=False,
            env=environment,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RefreshError(f"cannot validate staged batteries: {error}") from error
    if checked.returncode != 0:
        diagnostic = (checked.stderr or checked.stdout).decode("utf-8", errors="replace").strip()
        diagnostic = diagnostic[-2000:] if diagnostic else "appa describe returned no diagnostic"
        raise RefreshError(f"the serving root config does not load against the staged batteries: {diagnostic}")


def install(
    archive: bytes,
    target: Path,
    tag: str,
    config: Path | None = None,
    battery_dirs: list[Path] | None = None,
) -> None:
    parent = target.parent
    parent.mkdir(parents=True, exist_ok=True)
    if target.is_symlink() or (target.exists() and not target.is_dir()):
        raise RefreshError(f"battery target is not a directory: {target}")
    target.mkdir(exist_ok=True)
    backup = previous_path(target)
    if backup.exists() or backup.is_symlink():
        raise RefreshError(f"a battery refresh is already pending at {backup}")
    staging = Path(tempfile.mkdtemp(prefix=f".{target.name}.new-", dir=parent))
    moved_old = False
    try:
        extract_batteries(archive, staging)
        (staging / ".appa-release").write_text(f"{tag}\n", encoding="utf-8")
        if config is not None:
            validate_staged_config(config, target, staging, battery_dirs)
        target.rename(backup)
        moved_old = True
        staging.rename(target)
    except Exception:
        if moved_old and not target.exists():
            backup.rename(target)
            moved_old = False
        raise
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def finish(target: Path, commit: bool) -> None:
    backup = previous_path(target)
    if not backup.is_dir() or backup.is_symlink():
        raise RefreshError(f"no pending battery refresh exists at {target}")
    if commit:
        if target.is_symlink() or not target.is_dir():
            raise RefreshError(f"cannot commit a missing or non-directory battery target: {target}")
        trash = Path(tempfile.mkdtemp(prefix=f".{target.name}.trash-", dir=target.parent))
        trash.rmdir()
        backup.rename(trash)
        shutil.rmtree(trash)
        return
    if not target.exists():
        backup.rename(target)
        return
    if target.is_symlink() or not target.is_dir():
        raise RefreshError(f"cannot roll back non-directory battery target: {target}")
    failed = Path(tempfile.mkdtemp(prefix=f".{target.name}.failed-", dir=target.parent))
    failed.rmdir()
    target.rename(failed)
    try:
        backup.rename(target)
    except Exception:
        failed.rename(target)
        raise
    shutil.rmtree(failed)


def refresh(
    repository: str,
    target: Path,
    tag: str | None = None,
    config: Path | None = None,
    battery_dirs: list[Path] | None = None,
    opener: Callable = urlopen,
) -> str:
    tag, checksums_url, archive_url = release_assets(repository, tag, opener)
    archive_name = f"appa-plugin-{tag.removeprefix('v')}.tar.gz"
    checksums = download(checksums_url, MAX_CHECKSUM_BYTES, opener)
    archive = download(archive_url, MAX_ARCHIVE_BYTES, opener)
    verify_archive(archive, checksums, archive_name)
    install(archive, target, tag, config, battery_dirs)
    return tag


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", default="archestra-ai/OpenAPPA")
    parser.add_argument("--target", type=Path, default=Path("/var/lib/appa/release-batteries"))
    parser.add_argument("--tag", help="stable release tag returned by --check")
    parser.add_argument("--config", type=Path, default=os.environ.get("APPA_CONFIG"))
    parser.add_argument("--batteries-dir", action="append", type=Path)
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--check", action="store_true", help="print the latest stable release tag without changing files")
    actions.add_argument("--commit", action="store_true", help="discard the prior layer after a successful reload")
    actions.add_argument("--rollback", action="store_true", help="restore the prior layer after a refused reload")
    args = parser.parse_args()
    if (args.check or args.commit or args.rollback) and args.tag:
        parser.error("--tag applies only when staging a refresh")
    try:
        if args.check:
            tag, _, _ = release_assets(args.repository)
            print(tag)
            return 0
        if args.commit or args.rollback:
            finish(args.target, commit=args.commit)
            print(f"{'committed' if args.commit else 'rolled back'} battery refresh at {args.target}")
            return 0
        if args.config is None:
            raise RefreshError("--config or APPA_CONFIG is required before a battery refresh")
        tag = refresh(args.repository, args.target, args.tag, args.config, args.batteries_dir)
    except (RefreshError, OSError, tarfile.TarError) as error:
        print(f"appa-refresh-batteries: {error}", file=sys.stderr)
        return 1
    print(f"staged batteries from {args.repository} {tag} at {args.target}; reload, then commit or roll back")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
