#!/usr/bin/env python3
"""Prepare an immutable Cargo update; publish it separately without running Cargo."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

SOURCE = "archestra-ai/OpenAPPA"
TARGET = "archestra-ai/archestra"
GIT_URL = f"https://github.com/{SOURCE}.git"
MANIFEST = "platform/archestra-rs/openappa-rs/Cargo.toml"
LOCK = "platform/archestra-rs/Cargo.lock"
FILES = (MANIFEST, LOCK)
VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
SHA = re.compile(r"[0-9a-f]{40}")
MARKER = re.compile(r"<!-- appa-archestra-update: (v" + VERSION + r") ([0-9a-f]{40}) ([0-9]+) -->")


def version(tag: str) -> tuple[int, int, int]:
    match = re.fullmatch("v" + VERSION, tag)
    if not match:
        raise ValueError("release tag must be canonical stable vMAJOR.MINOR.PATCH")
    return tuple(map(int, match.groups()))


def sha(value: str) -> str:
    if not SHA.fullmatch(value):
        raise ValueError("expected a full lowercase commit SHA")
    return value


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def ancestor(root: Path, before: str, after: str) -> bool:
    result = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", sha(before), sha(after)])
    if result.returncode not in (0, 1):
        raise ValueError("cannot establish OpenAPPA commit ancestry")
    return result.returncode == 0


class GitHub:
    def __init__(self, token: str = "", base_url: str = "https://api.github.com"):
        self.token, self.base_url = token, base_url

    def request(self, path: str, data=None, method="GET", missing_ok=False):
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "appa-archestra-update"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        body = None if data is None else json.dumps(data).encode()
        req = urllib.request.Request(self.base_url + path, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if missing_ok and error.code == 404:
                return None
            # Do not echo response bodies, headers, or credentials into CI logs.
            raise ValueError(f"GitHub {method} {path.split('?')[0]} returned HTTP {error.code}") from None

    def pulls(self, state="open", head=None):
        page = 1
        while True:
            query = urllib.parse.urlencode({"state": state, "per_page": 100, "page": page, **({"head": head} if head else {})})
            batch = self.request(f"/repos/{TARGET}/pulls?{query}")
            yield from batch
            if len(batch) < 100:
                return
            page += 1

    def content(self, path, ref):
        record = self.request(f"/repos/{TARGET}/contents/{path}?ref={ref}")
        if record.get("type") != "file" or record.get("encoding") != "base64":
            raise ValueError("expected an ordinary UTF-8 repository file")
        return base64.b64decode(record["content"]).decode()


def release(api: GitHub, source: Path, tag: str, expected="") -> dict:
    version(tag)
    record = api.request(f"/repos/{SOURCE}/releases/tags/{tag}")
    url = f"https://github.com/{SOURCE}/releases/tag/{tag}"
    if (record.get("tag_name") != tag or record.get("draft") is not False
            or record.get("prerelease") is not False or not record.get("published_at")
            or record.get("html_url") != url or type(record.get("id")) is not int):
        raise ValueError("expected a published stable release in archestra-ai/OpenAPPA")
    # The API resolves lightweight and annotated tags to commits. Fetching the
    # exact ref separately detects tag movement between those reads.
    commit = sha(api.request(f"/repos/{SOURCE}/commits/{tag}")["sha"])
    if expected and sha(expected) != commit:
        raise ValueError("release tag moved or does not match the publishing workflow")
    git(source, "fetch", "--no-tags", "origin", "+refs/heads/main:refs/remotes/origin/main", f"+refs/tags/{tag}:refs/tags/{tag}")
    if git(source, "rev-parse", f"refs/tags/{tag}^{{commit}}") != commit:
        raise ValueError("release tag moved during verification")
    if not ancestor(source, commit, git(source, "rev-parse", "origin/main")):
        raise ValueError("release commit is not on OpenAPPA main")
    actual = tomllib.loads(git(source, "show", f"{commit}:Cargo.toml"))["workspace"]["package"]["version"]
    if actual != tag[1:]:
        raise ValueError("release tag and Cargo workspace version disagree")
    return {"tag": tag, "sha": commit, "id": record["id"], "url": url}


def pin(text: str) -> str:
    deps = tomllib.loads(text)["dependencies"]
    appa = {name: dep for name, dep in deps.items() if isinstance(dep, dict) and dep.get("git") == GIT_URL}
    if set(appa) != {"appa-runtime", "appa-runtime-api", "appa-eventlog", "appa-package"}:
        raise ValueError("unexpected embedded OpenAPPA dependency set; review updater")
    revisions = {sha(dep.get("rev", "")) for dep in appa.values()}
    if len(revisions) != 1 or any("branch" in dep or "tag" in dep for dep in appa.values()):
        raise ValueError("embedded dependencies must share one immutable revision")
    return revisions.pop()


def newer(source: Path, current: str, candidate: dict) -> bool:
    if current == candidate["sha"] or ancestor(source, candidate["sha"], current):
        return False  # Covers a manually adopted, newer unreleased main commit.
    if not ancestor(source, current, candidate["sha"]):
        raise ValueError("candidate release diverges from the installed OpenAPPA revision")
    current_version = tomllib.loads(git(source, "show", f"{current}:Cargo.toml"))["workspace"]["package"]["version"]
    if version("v" + current_version) > version(candidate["tag"]):
        return False
    return True


def updated_manifest(text: str, candidate: dict) -> str:
    old = pin(text)
    text, count = re.subn(r'(git = "' + re.escape(GIT_URL) + r'", rev = ")' + old + r'"', lambda m: m[1] + candidate["sha"] + '"', text)
    if count != 4:
        raise ValueError("cannot update all four OpenAPPA revisions without changing manifest structure")
    text = re.sub(r"^# Pinned to OpenAPPA .*$", f'# Pinned to OpenAPPA {candidate["tag"]} (release commit {candidate["sha"]}).', text, flags=re.MULTILINE)
    if pin(text) != candidate["sha"]:
        raise ValueError("manifest update did not preserve the dependency pins")
    return text


def validate_lock(text: str, candidate: dict):
    packages = tomllib.loads(text)["package"]
    appa = [p for p in packages if p.get("source", "").startswith("git+" + GIT_URL)]
    expected = f'git+{GIT_URL}?rev={candidate["sha"]}#{candidate["sha"]}'
    if not appa or any(p["source"] != expected or p["version"] != candidate["tag"][1:] for p in appa):
        raise ValueError("Cargo.lock must resolve every OpenAPPA crate to the verified release")
    if not {"appa", "appa-runtime-api", "appa-eventlog", "appa-package"} <= {p["name"] for p in appa}:
        raise ValueError("Cargo.lock is missing an embedded runtime dependency")


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def output(name, value):
    if path := os.environ.get("GITHUB_OUTPUT"):
        with open(path, "a") as stream:
            stream.write(f"{name}={value}\n")
    print(f"{name}={value}")


def prepare(args):
    api = GitHub(os.environ.get("GH_TOKEN", ""))
    candidate = release(api, args.source, args.tag, args.expected_sha)
    if git(args.target, "status", "--porcelain"):
        raise ValueError("target checkout must be clean")
    originals = {path: (args.target / path).read_text() for path in FILES}
    old = pin(originals[MANIFEST])
    if not newer(args.source, old, candidate):
        output("changed", "false")
        return
    (args.target / MANIFEST).write_text(updated_manifest(originals[MANIFEST], candidate))
    # A targeted update preserves unrelated locked dependencies; Cargo resolves
    # upstream's changed dependency graph and checksums rather than editing them.
    subprocess.run(["cargo", "update", "--manifest-path", "archestra-rs/Cargo.toml", "-p", "appa"], cwd=args.target / "platform", check=True)
    validate_lock((args.target / LOCK).read_text(), candidate)
    changed = set(git(args.target, "diff", "--name-only").splitlines())
    if changed != set(FILES):
        raise ValueError("Cargo update must change only the embedded manifest and workspace lockfile")
    args.bundle.parent.mkdir(parents=True, exist_ok=True)
    args.bundle.write_text(json.dumps({"release": candidate, "previous": old, "base": git(args.target, "rev-parse", "HEAD"), "original_hashes": {p: digest(t) for p, t in originals.items()}, "files": {p: (args.target / p).read_text() for p in FILES}}, indent=2) + "\n")
    output("changed", "true")


def managed_pr(pr, bot_login):
    match = MARKER.search(pr.get("body") or "")
    if not match:
        return None
    tag = match[1]
    if (pr["user"]["login"] != bot_login or pr["head"]["ref"] != "chore/openappa-" + tag
            or (pr["head"].get("repo") or {}).get("full_name") != TARGET
            or pr["base"]["ref"] != "main"):
        return None
    return {"tag": tag, "sha": match[5], "id": int(match[6]), "number": pr["number"], "url": pr["html_url"]}


def record_check(args):
    bundle = json.loads(args.bundle.read_text())
    bundle["compile_passed"] = args.outcome == "success"
    args.bundle.write_text(json.dumps(bundle, indent=2) + "\n")


def supersede(api, candidate, bot_login, keep):
    for pr in api.pulls():
        managed = managed_pr(pr, bot_login)
        if managed and managed["number"] != keep and version(managed["tag"]) < version(candidate["tag"]):
            api.request(f'/repos/{TARGET}/pulls/{managed["number"]}', {"state": "closed"}, method="PATCH")
            print(f'Closed superseded update PR #{managed["number"]}')


def publish(args):
    api = GitHub(os.environ["GH_TOKEN"])
    bundle = json.loads(args.bundle.read_text())
    read_api = GitHub(os.environ.get("GH_READ_TOKEN", ""))
    candidate = release(read_api, args.source, bundle["release"]["tag"], bundle["release"]["sha"])
    if candidate != bundle["release"] or set(bundle["files"]) != set(FILES) or set(bundle["original_hashes"]) != set(FILES):
        raise ValueError("prepared update has unexpected release metadata or files")
    if type(bundle.get("compile_passed")) is not bool:
        raise ValueError("prepared update must record the bridge compilation result")
    output("compile_passed", str(bundle["compile_passed"]).lower())
    branch = "chore/openappa-" + candidate["tag"]
    sha(bundle["base"])
    sha(bundle["previous"])
    exact = list(api.pulls(state="all", head="archestra-ai:" + branch))
    if exact:
        if len(exact) != 1 or managed_pr(exact[0], args.bot_login) is None:
            raise ValueError("release branch has a PR this updater does not own")
        managed = managed_pr(exact[0], args.bot_login)
        if managed["sha"] != candidate["sha"] or managed["id"] != candidate["id"]:
            raise ValueError("release identity changed after its update PR was created")
        output("pull_request", managed["url"])
        if exact[0]["state"] == "open":
            supersede(api, candidate, args.bot_login, managed["number"])
        return  # Never reopen a closed/merged/rejected update or overwrite edits.
    for pr in api.pulls():
        managed = managed_pr(pr, args.bot_login)
        if managed and version(managed["tag"]) > version(candidate["tag"]):
            print("A newer OpenAPPA update is already awaiting human review")
            return
    current_manifest = api.content(MANIFEST, "main")
    if not newer(args.source, pin(current_manifest), candidate):
        print("Archestra already contains this release or a newer OpenAPPA revision")
        return
    for path in FILES:
        if digest(api.content(path, bundle["base"])) != bundle["original_hashes"][path] or digest(api.content(path, "main")) != bundle["original_hashes"][path]:
            raise ValueError("Archestra's embedded dependencies changed during validation; rerun the workflow")
    if bundle["files"][MANIFEST] != updated_manifest(current_manifest, candidate) or pin(current_manifest) != bundle["previous"]:
        raise ValueError("prepared manifest is not the verified release update")
    validate_lock(bundle["files"][LOCK], candidate)
    base = api.request(f'/repos/{TARGET}/git/commits/{bundle["base"]}')
    tree = api.request(f"/repos/{TARGET}/git/trees", {"base_tree": base["tree"]["sha"], "tree": [{"path": p, "mode": "100644", "type": "blob", "content": bundle["files"][p]} for p in FILES]}, method="POST")
    message = f'chore(deps): bump OpenAPPA to {candidate["tag"]}\n\nOpenAPPA release: {candidate["id"]}\nSource commit: {candidate["sha"]}'
    commit = api.request(f"/repos/{TARGET}/git/commits", {"message": message, "tree": tree["sha"], "parents": [bundle["base"]]}, method="POST")
    ref_path = f"/repos/{TARGET}/git/ref/heads/{branch}"
    existing = api.request(ref_path, missing_ok=True)
    if existing:
        # Recover a failure between branch creation and PR creation only when
        # that branch contains exactly our prepared tree and commit provenance.
        head = api.request(f'/repos/{TARGET}/git/commits/{sha(existing["object"]["sha"])}')
        if head["tree"]["sha"] != tree["sha"] or head["message"] != message or [p["sha"] for p in head["parents"]] != [bundle["base"]]:
            raise ValueError("release branch already exists with different content; refusing to overwrite it")
    else:
        api.request(f"/repos/{TARGET}/git/refs", {"ref": "refs/heads/" + branch, "sha": commit["sha"]}, method="POST")
    check_result = ('**Passed:** `cargo check --locked -p openappa_rs` compiled the embedded bridge.'
                    if bundle["compile_passed"] else
                    '**Failed:** `cargo check --locked -p openappa_rs` found a bridge compatibility error. '
                    '**This draft requires a compatibility fix before review/merge.**')
    run_link = ""
    if re.fullmatch(r"[0-9]+", os.environ.get("GITHUB_RUN_ID", "")):
        run_link = f' [Updater logs](https://github.com/{SOURCE}/actions/runs/{os.environ["GITHUB_RUN_ID"]}).'
    body = (f'Adopt OpenAPPA [{candidate["tag"]}]({candidate["url"]}), release ID `{candidate["id"]}`, '
            f'pinned to [`{candidate["sha"]}`](https://github.com/{SOURCE}/commit/{candidate["sha"]}).\n\n'
            f'Previous source: `{bundle["previous"]}`. [Source comparison](https://github.com/{SOURCE}/compare/{bundle["previous"]}...{candidate["sha"]}).\n\n'
            f'Cargo regenerated the Rust workspace lockfile. {check_result}{run_link} '
            'Archestra runs its native runtime/PostgreSQL regression suite on this PR.\n\n'
            '**Human review and merge are required.** Review compatibility, transitive dependency changes, and the native test results before marking this draft ready. '
            'This automation does not approve, enable auto-merge, enqueue, or merge the PR.\n\n'
            f'<!-- appa-archestra-update: {candidate["tag"]} {candidate["sha"]} {candidate["id"]} -->')
    pr = api.request(f"/repos/{TARGET}/pulls", {"title": f'chore(deps): bump OpenAPPA to {candidate["tag"]}', "head": branch, "base": "main", "body": body, "draft": True}, method="POST")
    output("pull_request", pr["html_url"])
    # Close older managed PRs only after the new draft exists. Their branches
    # and any reviewer edits remain available; unrelated PRs are untouched.
    supersede(api, candidate, args.bot_login, pr["number"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("record-check")
    check.add_argument("--bundle", type=Path, required=True)
    check.add_argument("--outcome", choices=("success", "failure"), required=True)
    check.set_defaults(run=record_check)
    for name in ("prepare", "publish"):
        command = sub.add_parser(name)
        command.add_argument("--source", type=Path, required=True)
        command.add_argument("--bundle", type=Path, required=True)
        command.set_defaults(run=prepare if name == "prepare" else publish)
        if name == "prepare":
            command.add_argument("--target", type=Path, required=True)
            command.add_argument("--tag", required=True)
            command.add_argument("--expected-sha", default="")
        else:
            command.add_argument("--bot-login", required=True)
    args = parser.parse_args()
    try:
        args.run(args)
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Update refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
