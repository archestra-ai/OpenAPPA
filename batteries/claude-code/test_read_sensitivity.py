import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("read-sensitivity.py")
SPEC = importlib.util.spec_from_file_location("read_sensitivity", SCRIPT)
READ_SENSITIVITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(READ_SENSITIVITY)


class ReadSensitivityTests(unittest.TestCase):
    def test_hidden_file_and_hidden_directory_are_private(self):
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/workspace/.env.example"))
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/workspace/.config/tool/settings.json"))

    def test_credential_and_private_key_names_are_private(self):
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/workspace/config/credentials.json"))
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/workspace/certs/client.key"))
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/workspace/keys/id_ed25519"))

    def test_system_secret_locations_are_private(self):
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/etc/ssh/ssh_host_ed25519_key"))
        self.assertTrue(READ_SENSITIVITY.is_sensitive("/proc/self/environ"))
        self.assertTrue(
            READ_SENSITIVITY.is_sensitive("/Users/me/Library/Keychains/login.keychain-db")
        )

    def test_sensitive_symlink_target_is_private(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / ".credentials"
            target.write_text("secret", encoding="utf-8")
            link = root / "visible-name"
            try:
                link.symlink_to(target)
            except OSError as error:
                self.skipTest(f"symlinks unavailable: {error}")

            self.assertTrue(READ_SENSITIVITY.is_sensitive(link))

    def test_ordinary_source_path_is_public(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "src" / "main.py"
            source.parent.mkdir()
            source.write_text("print('hello')\n", encoding="utf-8")

            self.assertFalse(READ_SENSITIVITY.is_sensitive(source))

    def test_protocol_returns_private_audience(self):
        request = {
            "version": 1,
            "kind": "annotation",
            "name": "claude-code.read-sensitivity",
            "declaration": {
                "inputs": [],
                "trust_ranks": ["suspicious", "trusted"],
                "audiences": ["private"],
                "attention_marks": [],
                "effects": [],
            },
            "artifact": {
                "args": {
                    "name": "Read",
                    "description": "Reads a file and returns its contents.",
                    "arguments": {"file_path": "/workspace/.env"},
                }
            },
        }
        result = subprocess.run(
            [sys.executable, str(SCRIPT)],
            check=True,
            input=json.dumps(request),
            capture_output=True,
            text=True,
        )

        self.assertEqual(
            json.loads(result.stdout),
            {
                "version": 1,
                "answer": {
                    "delta": {"audience": ["private"]},
                    "requires": {"history": [], "attention": []},
                    "emits": [],
                },
            },
        )


if __name__ == "__main__":
    unittest.main()
