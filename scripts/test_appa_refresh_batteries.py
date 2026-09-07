import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("appa-refresh-batteries.py")
SPEC = importlib.util.spec_from_file_location("appa_refresh_batteries", SCRIPT)
refresh_batteries = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(refresh_batteries)


def archive(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as package:
        for name, body in entries:
            member = tarfile.TarInfo(name)
            member.size = len(body)
            member.mode = 0o644
            package.addfile(member, io.BytesIO(body))
    return output.getvalue()


class Response(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class RefreshBatteriesTests(unittest.TestCase):
    def test_refresh_verifies_and_atomically_replaces_only_the_release_layer(self):
        package = archive(
            [
                ("./batteries/slack/appa.toml", b"[policy]\nversion = 2\n"),
                ("./batteries/README.md", b"catalog\n"),
            ]
        )
        digest = hashlib.sha256(package).hexdigest()
        release_root = "https://github.com/archestra-ai/OpenAPPA/releases/download/v1.2.3"
        release = json.dumps(
            {
                "tag_name": "v1.2.3",
                "assets": [
                    {"name": "SHA256SUMS", "browser_download_url": f"{release_root}/SHA256SUMS"},
                    {
                        "name": "appa-plugin-1.2.3.tar.gz",
                        "browser_download_url": f"{release_root}/appa-plugin-1.2.3.tar.gz",
                    },
                ],
            }
        ).encode()
        responses = {
            "https://api.github.com/repos/archestra-ai/OpenAPPA/releases/latest": release,
            f"{release_root}/SHA256SUMS": f"{digest}  appa-plugin-1.2.3.tar.gz\n".encode(),
            f"{release_root}/appa-plugin-1.2.3.tar.gz": package,
        }

        def opener(request, timeout):
            self.assertEqual(timeout, 30)
            return Response(responses[request.full_url])

        with tempfile.TemporaryDirectory() as temporary:
            data = Path(temporary)
            target = data / "release-batteries"
            target.mkdir()
            (target / "old").write_text("old")
            marketplace = data / "batteries" / "custom"
            marketplace.mkdir(parents=True)
            (marketplace / "appa.toml").write_text("custom")

            tag = refresh_batteries.refresh("archestra-ai/OpenAPPA", target, opener=opener)

            self.assertEqual(tag, "v1.2.3")
            self.assertEqual((target / ".appa-release").read_text(), "v1.2.3\n")
            self.assertTrue((target / "slack/appa.toml").is_file())
            self.assertFalse((target / "old").exists())
            self.assertEqual((marketplace / "appa.toml").read_text(), "custom")
            refresh_batteries.finish(target, commit=False)
            self.assertEqual((target / "old").read_text(), "old")

            tag = refresh_batteries.refresh("archestra-ai/OpenAPPA", target, opener=opener)
            self.assertEqual(tag, "v1.2.3")
            refresh_batteries.finish(target, commit=True)
            self.assertFalse(refresh_batteries.previous_path(target).exists())

    def test_an_unsafe_archive_preserves_the_installed_release(self):
        package = archive(
            [
                ("batteries/slack/appa.toml", b"[policy]\nversion = 2\n"),
                ("batteries/../../escape", b"no"),
            ]
        )
        with tempfile.TemporaryDirectory() as temporary:
            data = Path(temporary)
            target = data / "release-batteries"
            target.mkdir()
            (target / "current").write_text("kept")

            with self.assertRaisesRegex(refresh_batteries.RefreshError, "unsafe path"):
                refresh_batteries.install(package, target, "v1.2.3")

            self.assertEqual((target / "current").read_text(), "kept")
            self.assertFalse((data / "escape").exists())

    def test_rollback_recovers_a_crash_after_the_old_layer_moved(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "release-batteries"
            target.mkdir()
            (target / "current").write_text("kept")
            target.rename(refresh_batteries.previous_path(target))

            refresh_batteries.finish(target, commit=False)

            self.assertEqual((target / "current").read_text(), "kept")

    def test_release_metadata_must_name_one_stable_semver_bundle(self):
        payload = json.dumps({"tag_name": "main", "assets": []}).encode()

        def opener(_request, timeout):
            self.assertEqual(timeout, 30)
            return Response(payload)

        with self.assertRaisesRegex(refresh_batteries.RefreshError, "not stable semver"):
            refresh_batteries.release_assets("archestra-ai/OpenAPPA", opener=opener)

        with self.assertRaisesRegex(refresh_batteries.RefreshError, "invalid GitHub repository"):
            refresh_batteries.release_assets("../../other", opener=opener)

        payload = json.dumps({"tag_name": "v1.2.3", "assets": None}).encode()
        with self.assertRaisesRegex(refresh_batteries.RefreshError, "assets field is not a list"):
            refresh_batteries.release_assets("archestra-ai/OpenAPPA", opener=opener)

    def test_checksum_mismatch_refuses_the_archive(self):
        with self.assertRaisesRegex(refresh_batteries.RefreshError, "checksum mismatch"):
            refresh_batteries.verify_archive(
                b"archive",
                f"{'0' * 64}  appa-plugin-1.2.3.tar.gz\n".encode(),
                "appa-plugin-1.2.3.tar.gz",
            )

    def test_staged_validation_substitutes_the_target_and_clears_its_env(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "appa"
            arguments = root / "arguments"
            executable.write_text(
                "#!/bin/sh\n"
                "test -z \"${APPA_BATTERIES_DIR+x}\"\n"
                "printf '%s\\n' \"$@\" >\"$ARGS_OUT\"\n"
            )
            executable.chmod(0o755)
            target = root / "release-batteries"
            staging = root / "staging"
            config = root / "appa.toml"
            for directory in (target, staging, root / "marketplace", root / "image"):
                directory.mkdir()
            config.write_text("[policy]\nversion = 2\n")
            environment = {
                "APPA_BATTERIES_DIR": "/wrong/env/path",
                "ARGS_OUT": str(arguments),
                "PATH": f"{root}:{os.environ['PATH']}",
            }
            with mock.patch.dict(os.environ, environment):
                refresh_batteries.validate_staged_config(
                    config,
                    target,
                    staging,
                    [root / "marketplace", target, root / "image"],
                )

            invoked = arguments.read_text().splitlines()
            self.assertIn(str(staging), invoked)
            self.assertNotIn(str(target), invoked)

    def test_cli_rejects_conflicting_actions_without_network_access(self):
        checked = subprocess.run(
            [sys.executable, SCRIPT, "--check", "--tag", "v1.2.3"],
            capture_output=True,
            check=False,
            text=True,
        )
        self.assertEqual(checked.returncode, 2)
        self.assertIn("--tag applies only", checked.stderr)


if __name__ == "__main__":
    unittest.main()
