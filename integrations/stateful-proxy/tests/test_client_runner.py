from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from stateful_proxy import client_runner


class _Process:
    pid = 1

    def wait(self, timeout: float | None = None) -> int:
        return 0


class ClientRunnerTests(unittest.TestCase):
    def test_opencode_fixture_configuration_uses_installed_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            argv = [
                "appa-stateful-client-runner", "opencode", "--work-root", str(root),
                "--client-executable", "/bin/true", "--fixture-mcp", "--fixture-command", "/bin/true",
                "--proxy-url", "http://127.0.0.1:18765", "--opencode-title", "fixture-run",
            ]
            with redirect_stdout(io.StringIO()), patch.object(sys, "argv", argv), patch.object(client_runner.subprocess, "Popen", return_value=_Process()):
                self.assertEqual(client_runner.main(), 0)
            config = json.loads((root / "opencode-native" / "opencode.json").read_text())
            command = config["mcp"]["appa_fixture"]["command"]
            self.assertEqual(command[0], "/bin/true")
            self.assertEqual(command[1], "--output-dir")
            self.assertNotIn("lifecycle_tools.py", json.dumps(config))

    def test_codex_v1_read_only_profile_is_constructed_without_running_client(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            argv = [
                "appa-stateful-client-runner", "codex", "--work-root", directory,
                "--client-executable", "/bin/true", "--codex-read-only", "--codex-legacy-landlock",
                "--codex-v1-profile", "--compact-threshold", "1024", "--resume", "fixture-session", "--fork",
            ]
            with redirect_stdout(io.StringIO()), patch.object(sys, "argv", argv), patch.object(client_runner.subprocess, "Popen", return_value=_Process()) as popen:
                self.assertEqual(client_runner.main(), 0)
            command = popen.call_args.args[0]
            self.assertIn("--sandbox", command)
            self.assertIn("read-only", command)
            self.assertIn("features.multi_agent_v2=false", command)
            self.assertIn("features.use_legacy_landlock=true", command)
            self.assertIn("model_auto_compact_token_limit=1024", command)
            self.assertEqual(command[-3:], ["fork", "fixture-session", client_runner.PROMPT])


if __name__ == "__main__":
    unittest.main()
