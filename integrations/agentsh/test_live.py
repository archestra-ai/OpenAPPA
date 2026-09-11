"""Explicit Linux backend acceptance probes: python3 test_live.py BACKEND_DIR."""

import fcntl
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

BACKEND = Path(sys.argv.pop(1)).resolve()
RUNNER = Path(__file__).with_name("run.py")


class LiveTests(unittest.TestCase):
    def execute(self, command, *, inherited_fd=None):
        job = tempfile.TemporaryDirectory()
        self.addCleanup(job.cleanup)
        root = Path(job.name)
        (root / "inputs").mkdir()
        (root / "output").mkdir()
        (root / "inputs/source").write_bytes(b"seven cobalt widgets\n")
        (root / "request.json").write_text(json.dumps({"command": command}))
        proc = subprocess.run(
            [sys.executable, "-I", str(RUNNER), str(BACKEND), str(root)],
            capture_output=True,
            check=False,
            text=True,
            timeout=150,
            env={**os.environ, "APPA_CANARY_SECRET": "must-not-inherit"},
            pass_fds=() if inherited_fd is None else (inherited_fd,),
        )
        self.assertEqual(
            proc.returncode, 0, proc.stderr + str(list(root.glob("control/*")))
        )
        return json.loads(proc.stdout)["result"], root

    def test_allowed_inputs_and_output(self):
        result, root = self.execute(
            "cat inputs/source > output/result; printf completed"
        )
        self.assertEqual(result["exit_code"], 0, result)
        self.assertEqual(result["stdout"], "completed")
        self.assertEqual(
            (root / "output/result").read_bytes(), b"seven cobalt widgets\n"
        )

    def test_control_files_and_input_writes_denied(self):
        for command in [
            "cat /job/control/config.json",
            "echo overwrite > inputs/source",
        ]:
            result, root = self.execute(command)
            self.assertNotEqual(result["exit_code"], 0, result)
            self.assertEqual(
                (root / "inputs/source").read_bytes(), b"seven cobalt widgets\n"
            )

    def test_network_and_parent_memory_denied(self):
        result, _ = self.execute("python3 -c 'import socket; print(123)' ")
        self.assertEqual(result["exit_code"], 0, result)
        self.assertEqual(result["stdout"], "123\n")
        for script in [
            "import socket; socket.socket()",
            "open('/proc/1/mem','rb').read(1)",
        ]:
            result, _ = self.execute("python3 -c " + shlex.quote(script))
            self.assertNotEqual(result["exit_code"], 0, result)
            self.assertIn("PermissionError", result.get("stderr", ""))

    def test_keyring_and_environment_denied(self):
        key = subprocess.check_output(
            ["keyctl", "add", "user", "appa-isolation-test", "KEYRING_CANARY", "@s"],
            text=True,
        ).strip()
        try:
            result, _ = self.execute("keyctl pipe " + key)
            self.assertNotEqual(result["exit_code"], 0, result)
            self.assertNotIn("KEYRING_CANARY", result.get("stdout", ""))
        finally:
            subprocess.run(["keyctl", "revoke", key], check=True)
            subprocess.run(["keyctl", "unlink", key, "@s"], check=True)
        result, _ = self.execute("printf '%s' \"$APPA_CANARY_SECRET\"")
        self.assertEqual(result["exit_code"], 0, result)
        self.assertEqual(result.get("stdout", ""), "")

    def test_inherited_descriptor_closed(self):
        with tempfile.TemporaryFile() as source:
            source.write(b"FD_CANARY")
            source.seek(0)
            # Use a high descriptor unlikely to be reused by the child's libraries.
            fd = fcntl.fcntl(source, fcntl.F_DUPFD, 100)
            try:
                script = f"import os; print(os.read({fd}, 9))"
                result, _ = self.execute(
                    "python3 -c " + shlex.quote(script), inherited_fd=fd
                )
                self.assertNotIn("FD_CANARY", result.get("stdout", ""))
                self.assertNotEqual(result["exit_code"], 0, result)
                self.assertIn("Bad file descriptor", result.get("stderr", ""))
            finally:
                os.close(fd)

    def test_descendants_cannot_modify_after_return(self):
        result, root = self.execute(
            "setsid sh -c 'printf started > output/started; sleep 2; printf late >> output/result' >/dev/null 2>&1 & "
            "while [ ! -f output/started ]; do sleep .01; done; printf first > output/result"
        )
        self.assertEqual(result["exit_code"], 0, result)
        self.assertEqual((root / "output/started").read_text(), "started")
        self.assertEqual((root / "output/result").read_text(), "first")
        time.sleep(3)
        self.assertEqual((root / "output/result").read_text(), "first")


if __name__ == "__main__":
    unittest.main()
