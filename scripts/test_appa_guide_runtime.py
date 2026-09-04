from __future__ import annotations

from contextlib import redirect_stdout
import importlib.util
import io
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("appa-guide-runtime.py")
SPEC = importlib.util.spec_from_file_location("appa_guide_runtime", SCRIPT)
assert SPEC and SPEC.loader
guide = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guide)


class GuideRuntimeTests(unittest.TestCase):
    def test_inspect_reports_source_description_and_batteries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory, "appa.toml")
            config.write_text("version = 2\n", encoding="utf-8")
            described = subprocess.CompletedProcess([], 0, b"Config: loadable\n", b"")
            output = io.BytesIO()
            stdout = mock.Mock(buffer=output)
            with (
                mock.patch.dict(guide.os.environ, {"APPA_CONFIG": str(config)}),
                mock.patch.object(guide.subprocess, "run", return_value=described) as run,
                mock.patch.object(guide, "runtime_request", return_value=b'{"batteries":[]}'),
                mock.patch.object(guide.sys, "stdout", stdout),
            ):
                guide.inspect()
            run.assert_called_once_with(
                ["/usr/local/bin/appa", "describe", "--adapter", "kagent"],
                check=True,
                capture_output=True,
            )
            self.assertIn(b"version = 2", output.getvalue())
            self.assertIn(b"Config: loadable", output.getvalue())
            self.assertIn(b'{"batteries": []}', output.getvalue())

    def test_refresh_stage_uses_the_release_recorded_by_check(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory, "candidate")
            checked = subprocess.CompletedProcess([], 0, "v1.2.3\n", "")
            with (
                mock.patch.object(guide, "CANDIDATE", candidate),
                mock.patch.object(guide.subprocess, "run", return_value=checked) as run,
            ):
                with redirect_stdout(io.StringIO()):
                    guide.refresh_check()
                self.assertEqual(candidate.read_text(encoding="utf-8"), "v1.2.3\n")
                guide.refresh_stage()
            self.assertEqual(
                run.call_args_list,
                [
                    mock.call([guide.REFRESH, "--check"], check=True, capture_output=True, text=True),
                    mock.call([guide.REFRESH, "--tag", "v1.2.3"], check=True),
                ],
            )
            self.assertFalse(candidate.exists())


if __name__ == "__main__":
    unittest.main()
