import argparse
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("updater", Path(__file__).with_name("appa-archestra-update.py"))
updater = importlib.util.module_from_spec(spec)
spec.loader.exec_module(updater)
BOT = "openappa-archestra-updater[bot]"
CORE = "# Core\r\nNon-ASCII: żółw — ✓\r\nno trailing newline".encode()


def command(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def manifest(commit):
    return '# Pinned to OpenAPPA main at old.\n[dependencies]\n' + "\n".join(
        f'{name} = {{ git = "{updater.GIT_URL}", rev = "{commit}" }}'
        for name in ("appa-runtime", "appa-runtime-api", "appa-eventlog", "appa-package")
    ) + '\nother = "1"\n'


def lock(candidate):
    return 'version = 4\n' + "\n".join(
        f'[[package]]\nname = "{name}"\nversion = "{candidate["tag"][1:]}"\n'
        f'source = "git+{updater.GIT_URL}?rev={candidate["sha"]}#{candidate["sha"]}"\n'
        for name in ("appa", "appa-runtime-api", "appa-eventlog", "appa-package", "appa-engine")
    )


def pr(candidate, number=1, login=BOT, state="open"):
    return {"number": number, "html_url": f'https://github.com/{updater.TARGET}/pull/{number}',
            "state": state, "user": {"login": login}, "base": {"ref": "main"},
            "head": {"ref": "chore/openappa-" + candidate["tag"], "repo": {"full_name": updater.TARGET}},
            "body": f'<!-- appa-archestra-update: {candidate["tag"]} {candidate["sha"]} {candidate["id"]} -->'}


class API:
    """In-memory GitHub boundary; repository and ancestry checks use real Git."""
    def __init__(self, source, candidate, files):
        self.source, self.candidate, self.files = source, candidate, files
        self.prs, self.writes, self.ref, self.refs = [], [], None, {}
        self.release_record = {"id": candidate["id"], "tag_name": candidate["tag"], "draft": False,
                               "prerelease": False, "published_at": "2026-10-02T12:00:00Z", "html_url": candidate["url"]}

    def request(self, path, data=None, method="GET", missing_ok=False):
        if method != "GET":
            self.writes.append((method, path, data))
        if path.startswith(f"/repos/{updater.SOURCE}/releases/tags/"):
            return self.release_record
        if path.startswith(f"/repos/{updater.SOURCE}/commits/"):
            return {"sha": self.candidate["sha"]}
        if path.endswith("/git/trees"):
            return {"sha": "f" * 40}
        if path.endswith("/git/commits"):
            self.commit = {**data, "tree": {"sha": data["tree"]}, "sha": "e" * 40,
                           "parents": [{"sha": p} for p in data["parents"]]}
            return self.commit
        if "/git/commits/" in path:
            return self.commit if path.endswith("e" * 40) else {"tree": {"sha": "d" * 40}}
        if "/git/ref/heads/" in path:
            return self.ref
        if path.endswith("/git/refs"):
            self.ref = {"object": {"sha": data["sha"]}}
            return self.ref
        if path.endswith("/pulls") and method == "POST":
            candidate_pr = pr(self.candidate, number=100)
            candidate_pr["body"] = data["body"]
            self.prs.append(candidate_pr)
            return candidate_pr
        if "/pulls/" in path and method == "PATCH":
            next(p for p in self.prs if str(p["number"]) == path.rsplit("/", 1)[1])["state"] = data["state"]
            return {}
        raise AssertionError((method, path, data))

    def pulls(self, state="open", head=None):
        return [p for p in self.prs if (state == "all" or p["state"] == state)
                and (head is None or head == "archestra-ai:" + p["head"]["ref"])]

    def content(self, path, ref, missing_ok=False):
        files = self.refs.get(ref, self.files)
        return None if missing_ok and path not in files else files[path]


class UpdaterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.origin = self.root / "origin"
        self.origin.mkdir()
        command(self.origin, "init", "-b", "main")
        command(self.origin, "config", "user.email", "test@example.invalid")
        command(self.origin, "config", "user.name", "Test")
        command(self.origin, "config", "core.autocrlf", "false")
        commits = []
        core = self.origin / updater.CORE
        core.parent.mkdir(parents=True)
        for number in (1, 2, 3):
            (self.origin / "Cargo.toml").write_text(f'[workspace.package]\nversion = "1.2.{number}"\n')
            # Only the v1.2.2 release ships the appa-guide core.
            if number == 2:
                core.write_bytes(CORE)
            else:
                core.unlink(missing_ok=True)
            command(self.origin, "add", "-A")
            command(self.origin, "commit", "-m", f"release {number}")
            commits.append(command(self.origin, "rev-parse", "HEAD"))
            command(self.origin, "tag", f"v1.2.{number}")
        self.old, current, future = commits
        self.source = self.root / "source"
        subprocess.run(["git", "clone", "--quiet", str(self.origin), str(self.source)], check=True)
        self.candidate = {"tag": "v1.2.2", "sha": current, "id": 22, "url": f"https://github.com/{updater.SOURCE}/releases/tag/v1.2.2"}
        self.future = {"tag": "v1.2.3", "sha": future, "id": 23, "url": f"https://github.com/{updater.SOURCE}/releases/tag/v1.2.3"}
        self.files = {updater.MANIFEST: manifest(self.old), updater.LOCK: lock({**self.candidate, "tag": "v1.2.1", "sha": self.old})}
        self.api = API(self.source, self.candidate, self.files)
        self.bundle = self.root / "update.json"
        self.bundle.write_text(json.dumps({"release": self.candidate, "previous": self.old, "compile_passed": True,
            "base": "a" * 40, "original_hashes": {p: updater.digest(t) for p, t in self.files.items()},
            "files": {updater.MANIFEST: updater.updated_manifest(self.files[updater.MANIFEST], self.candidate), updater.LOCK: lock(self.candidate)}}))
        self.args = argparse.Namespace(source=self.source, bundle=self.bundle, bot_login=BOT)

    def publish(self):
        with patch.object(updater, "GitHub", return_value=self.api), patch.dict(os.environ, {"GH_TOKEN": "test-only"}):
            updater.publish(self.args)

    def with_guide(self, original, prepared=CORE.decode()):
        self.files[updater.GUIDE] = original
        bundle = json.loads(self.bundle.read_text())
        bundle["original_hashes"][updater.GUIDE] = updater.digest(original)
        bundle["files"][updater.GUIDE] = prepared
        self.bundle.write_text(json.dumps(bundle))

    def tree_paths(self):
        tree = next(data for _, path, data in self.api.writes if path.endswith("/git/trees"))
        return {entry["path"]: entry["content"] for entry in tree["tree"]}

    def run_prepare(self, guide=None):
        target = self.root / "target"
        target.mkdir()
        command(target, "init", "-b", "main")
        command(target, "config", "user.email", "test@example.invalid")
        command(target, "config", "user.name", "Test")
        command(target, "config", "core.autocrlf", "false")
        installed = {p: t.encode() for p, t in self.files.items()} | ({updater.GUIDE: guide} if guide is not None else {})
        for path, data in installed.items():
            (target / path).parent.mkdir(parents=True, exist_ok=True)
            (target / path).write_bytes(data)
        command(target, "add", ".")
        command(target, "commit", "-m", "installed runtime")
        real_run = subprocess.run
        self.cargo_calls = []
        def run(args, **kwargs):
            if args[0] == "cargo":
                self.cargo_calls.append((args, kwargs["cwd"]))
                (target / updater.LOCK).write_text(lock(self.api.candidate))
                return subprocess.CompletedProcess(args, 0)
            return real_run(args, **kwargs)
        args = argparse.Namespace(source=self.source, target=target, bundle=self.bundle,
                                  tag=self.api.candidate["tag"], expected_sha=self.api.candidate["sha"])
        with patch.object(updater, "GitHub", return_value=self.api), patch.object(subprocess, "run", side_effect=run):
            updater.prepare(args)
        return target, json.loads(self.bundle.read_text())

    def test_release_verification_and_annotated_tag(self):
        command(self.origin, "tag", "-f", "-a", "v1.2.2", self.candidate["sha"], "-m", "annotated release")
        self.assertEqual(updater.release(self.api, self.source, "v1.2.2"), self.candidate)
        with self.assertRaisesRegex(ValueError, "does not match"):
            updater.release(self.api, self.source, "v1.2.2", self.old)

    def test_refuses_unpublished_prerelease_and_wrong_repository(self):
        for update in ({"draft": True}, {"prerelease": True}, {"published_at": None}, {"html_url": "https://github.com/other/repo/releases/tag/v1.2.2"}):
            with self.subTest(update=update):
                original = self.api.release_record.copy()
                self.api.release_record.update(update)
                with self.assertRaises(ValueError):
                    updater.release(self.api, self.source, "v1.2.2")
                self.api.release_record = original
        for tag in ("v1.2.2-rc.1", "v01.2.2", "v1.2.2+meta", "main", "v1.2.2\nfile", "--help", "v1.2.2/evil"):
            with self.assertRaises(ValueError):
                updater.release(self.api, self.source, tag)

    def test_refuses_tag_version_mismatch_and_non_main_commit(self):
        command(self.origin, "tag", "-f", "v1.2.2", self.old)
        self.api.candidate = {**self.candidate, "sha": self.old}
        with self.assertRaisesRegex(ValueError, "version disagree"):
            updater.release(self.api, self.source, "v1.2.2")
        command(self.origin, "checkout", "-b", "side", self.old)
        (self.origin / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.2"\n')
        (self.origin / "side").write_text("side")
        command(self.origin, "add", ".")
        command(self.origin, "commit", "-m", "side release")
        side = command(self.origin, "rev-parse", "HEAD")
        command(self.origin, "tag", "-f", "v1.2.2", side)
        self.api.candidate = {**self.candidate, "sha": side}
        with self.assertRaisesRegex(ValueError, "not on OpenAPPA main"):
            updater.release(self.api, self.source, "v1.2.2")

    def test_no_downgrade_of_newer_manual_pin_and_no_repeat(self):
        self.assertTrue(updater.newer(self.source, self.old, self.candidate))
        self.assertFalse(updater.newer(self.source, self.candidate["sha"], self.candidate))
        self.assertFalse(updater.newer(self.source, self.future["sha"], self.candidate))

    def test_all_pins_preserve_features_and_reject_partial_manifest(self):
        text = self.files[updater.MANIFEST]
        text = text.replace(f'rev = "{self.old}" }}', f'rev = "{self.old}", features = ["postgres"] }}')
        result = updater.updated_manifest(text, self.candidate)
        self.assertEqual(updater.pin(result), self.candidate["sha"])
        self.assertEqual(result.count('features = ["postgres"]'), 4)
        self.assertIn('other = "1"', result)
        with self.assertRaises(ValueError):
            updater.pin(text.replace(self.old, self.future["sha"], 1))

    def test_lock_must_resolve_every_source_and_version(self):
        updater.validate_lock(lock(self.candidate), self.candidate)
        for broken in (lock(self.candidate).replace(self.candidate["sha"], self.old, 1), lock(self.candidate).replace('version = "1.2.2"', 'version = "1.2.1"', 1)):
            with self.assertRaises(ValueError):
                updater.validate_lock(broken, self.candidate)

    def test_new_draft_then_close_only_older_owned_prs(self):
        older = {**self.candidate, "tag": "v1.2.1", "id": 21, "sha": self.old}
        self.api.prs = [pr(older, 1), pr(older, 2, login="developer")]
        self.publish()
        posts = [write for write in self.api.writes if write[1].endswith("/pulls")]
        self.assertEqual(len(posts), 1)
        self.assertTrue(posts[0][2]["draft"])
        self.assertEqual(posts[0][2]["base"], "main")
        self.assertIn(self.candidate["url"], posts[0][2]["body"])
        self.assertEqual(self.api.prs[0]["state"], "closed")
        self.assertEqual(self.api.prs[1]["state"], "open")
        self.assertLess(self.api.writes.index(posts[0]), next(i for i, w in enumerate(self.api.writes) if w[0] == "PATCH"))
        self.assertEqual({w[0] for w in self.api.writes}, {"POST", "PATCH"})
        self.assertFalse(any("merge" in path or "/reviews" in path for _, path, _ in self.api.writes))

    def test_compile_failure_opens_blocked_draft_with_failed_evidence(self):
        updater.record_check(argparse.Namespace(bundle=self.bundle, outcome="failure"))
        with patch.dict(os.environ, {"GITHUB_RUN_ID": "123"}):
            self.publish()
        post = next(data for method, path, data in self.api.writes if method == "POST" and path.endswith("/pulls"))
        self.assertTrue(post["draft"])
        self.assertIn("**Failed:**", post["body"])
        self.assertIn("requires a compatibility fix", post["body"])
        self.assertIn("actions/runs/123", post["body"])

    def test_missing_compile_evidence_refuses_publication(self):
        bundle = json.loads(self.bundle.read_text())
        del bundle["compile_passed"]
        self.bundle.write_text(json.dumps(bundle))
        with self.assertRaisesRegex(ValueError, "compilation result"):
            self.publish()
        self.assertEqual(self.api.writes, [])

    def test_repeat_preserves_reviewer_edits_and_closed_prs(self):
        for state in ("open", "closed"):
            self.api.prs = [pr(self.candidate, state=state)]
            self.api.prs[0]["body"] += "\nHuman reviewer notes"
            self.api.writes = []
            self.publish()
            self.assertEqual(self.api.writes, [])
            self.assertIn("Human reviewer notes", self.api.prs[0]["body"])

    def test_older_release_skips_newer_open_pr_or_installed_pin(self):
        self.api.prs = [pr(self.future)]
        self.publish()
        self.assertEqual(self.api.writes, [])
        self.api.prs = []
        self.api.files[updater.MANIFEST] = manifest(self.future["sha"])
        self.publish()
        self.assertEqual(self.api.writes, [])

    def test_refuses_tag_movement_and_human_branch_collision(self):
        self.api.prs = [pr({**self.candidate, "sha": self.old})]
        with self.assertRaisesRegex(ValueError, "identity changed"):
            self.publish()
        self.api.prs = [pr(self.candidate, login="developer")]
        with self.assertRaisesRegex(ValueError, "does not own"):
            self.publish()
        self.assertEqual(self.api.writes, [])

    def test_refuses_changed_target_or_unexpected_artifact_path(self):
        self.api.files[updater.LOCK] += "\n# another dependency update\n"
        with self.assertRaisesRegex(ValueError, "dependencies changed"):
            self.publish()
        self.assertEqual(self.api.writes, [])
        bundle = json.loads(self.bundle.read_text())
        bundle["files"][".github/workflows/evil.yml"] = "evil"
        self.bundle.write_text(json.dumps(bundle))
        with self.assertRaisesRegex(ValueError, "unexpected"):
            self.publish()

    def test_recovers_branch_created_before_pr_and_refuses_overwrite(self):
        original = self.api.request
        def failed_pr(path, data=None, method="GET", missing_ok=False):
            if method == "POST" and path.endswith("/pulls"):
                raise ValueError("interrupted before PR creation")
            return original(path, data, method, missing_ok)
        with patch.object(self.api, "request", side_effect=failed_pr):
            with self.assertRaisesRegex(ValueError, "interrupted"):
                self.publish()
        self.assertIsNotNone(self.api.ref)
        self.publish()
        self.assertEqual(len(self.api.prs), 1)
        self.assertEqual(sum(path.endswith("/git/refs") for _, path, _ in self.api.writes), 1)

        self.api.prs = []
        def different_head(path, data=None, method="GET", missing_ok=False):
            if method == "GET" and path.endswith("/git/commits/" + "e" * 40):
                return {**self.api.commit, "message": "A human changed this branch"}
            return original(path, data, method, missing_ok)
        with patch.object(self.api, "request", side_effect=different_head):
            with self.assertRaisesRegex(ValueError, "refusing to overwrite"):
                self.publish()

    def test_closure_failure_preserves_new_draft_and_retry_repairs_closure(self):
        older = {**self.candidate, "tag": "v1.2.1", "id": 21, "sha": self.old}
        self.api.prs = [pr(older)]
        original = self.api.request
        def failed_close(path, data=None, method="GET", missing_ok=False):
            if method == "PATCH":
                raise ValueError("closure interrupted")
            return original(path, data, method, missing_ok)
        with patch.object(self.api, "request", side_effect=failed_close):
            with self.assertRaisesRegex(ValueError, "closure interrupted"):
                self.publish()
        self.assertEqual(len(self.api.prs), 2)
        self.assertEqual(self.api.prs[0]["state"], "open")
        self.publish()
        self.assertEqual(len(self.api.prs), 2)
        self.assertEqual(self.api.prs[0]["state"], "closed")

    def test_prepare_executes_targeted_cargo_regeneration(self):
        target, bundle = self.run_prepare()
        self.assertEqual(self.cargo_calls, [(["cargo", "update", "--manifest-path", "archestra-rs/Cargo.toml", "-p", "appa"], target / "platform")])
        self.assertEqual(set(bundle["files"]), set(updater.FILES))
        self.assertEqual(set(bundle["original_hashes"]), set(updater.FILES))
        self.assertEqual(updater.pin(bundle["files"][updater.MANIFEST]), self.candidate["sha"])
        updater.validate_lock(bundle["files"][updater.LOCK], self.candidate)

    def test_prepare_copies_release_core_byte_for_byte(self):
        target, bundle = self.run_prepare(guide=b"stale\n")
        self.assertEqual((target / updater.GUIDE).read_bytes(), CORE)
        self.assertEqual(bundle["files"][updater.GUIDE].encode(), CORE)
        self.assertEqual(bundle["original_hashes"][updater.GUIDE], updater.digest("stale\n"))
        self.assertEqual(set(command(target, "diff", "--name-only").splitlines()), {*updater.FILES, updater.GUIDE})

    def test_prepare_accepts_unchanged_core(self):
        target, bundle = self.run_prepare(guide=CORE)
        self.assertEqual(set(command(target, "diff", "--name-only").splitlines()), set(updater.FILES))
        self.assertEqual(bundle["files"][updater.GUIDE].encode(), CORE)
        self.assertEqual(bundle["original_hashes"][updater.GUIDE], updater.digest(CORE.decode()))

    def test_prepare_refuses_release_without_core(self):
        self.api.candidate = self.future
        self.api.release_record.update(id=self.future["id"], tag_name=self.future["tag"], html_url=self.future["url"])
        with self.assertRaisesRegex(ValueError, "no integrations/appa-guide/references/core.md"):
            self.run_prepare(guide=b"stale\n")

    def test_publish_refreshes_changed_core_only(self):
        self.with_guide("stale\n")
        self.publish()
        self.assertEqual(self.tree_paths()[updater.GUIDE].encode(), CORE)
        self.api.prs, self.api.writes, self.api.ref = [], [], None
        self.with_guide(CORE.decode())
        self.publish()
        self.assertEqual(set(self.tree_paths()), set(updater.FILES))

    def test_publish_refuses_tampered_core(self):
        self.with_guide("stale\n", prepared=CORE.decode() + "\ninjected")
        with self.assertRaisesRegex(ValueError, "not the verified release"):
            self.publish()
        self.assertEqual(self.api.writes, [])

    def test_publish_refuses_core_changed_appeared_or_removed_on_main(self):
        self.with_guide("stale\n")
        for main in ({**self.files, updater.GUIDE: "edited\n"}, {p: self.files[p] for p in updater.FILES}):
            with self.subTest(main=main.get(updater.GUIDE)):
                self.api.refs["main"] = main
                with self.assertRaisesRegex(ValueError, "rerun"):
                    self.publish()
        bundle = json.loads(self.bundle.read_text())
        for field in ("files", "original_hashes"):
            del bundle[field][updater.GUIDE]
        self.bundle.write_text(json.dumps(bundle))
        self.api.refs = {"a" * 40: {p: self.files[p] for p in updater.FILES}, "main": self.files}
        with self.assertRaisesRegex(ValueError, "rerun"):
            self.publish()
        self.assertEqual(self.api.writes, [])

    def test_github_pull_list_paginates_before_filtering(self):
        api = updater.GitHub()
        with patch.object(api, "request", side_effect=[[{"number": n} for n in range(100)], [{"number": 100}]]) as requests:
            self.assertEqual(len(list(api.pulls())), 101)
        self.assertIn("page=2", requests.call_args[0][0])


if __name__ == "__main__":
    unittest.main()
