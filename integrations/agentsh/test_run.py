import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "agentsh_run", Path(__file__).with_name("run.py")
)
run = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(run)


class RunnerTests(unittest.TestCase):
    def make_job(self, root):
        job = Path(root) / "job"
        (job / "inputs").mkdir(parents=True)
        (job / "output").mkdir()
        (job / "request.json").write_text(json.dumps({"command": "echo ok"}))
        return job

    def test_bwrap_is_closed_and_has_required_mounts(self):
        with tempfile.TemporaryDirectory() as root:
            job = self.make_job(root)
            cmd = run.bwrap_command(Path(root) / "backend", job, Path(root) / "run.py")
        for flag in (
            "--unshare-all",
            "--as-pid-1",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--proc",
            "--dev",
            "--tmpfs",
        ):
            self.assertIn(flag, cmd)
        self.assertNotIn("/", cmd)
        self.assertNotIn(str(Path.home()), cmd)
        self.assertEqual(cmd[-2:], ["/runner/run.py", "--inner"])

    def test_outer_clears_env_closes_fds_and_waits_before_output(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            job = self.make_job(root)
            backend = root / "backend"
            backend.mkdir()
            (backend / "agentsh").write_text("")
            (backend / "agentsh-unixwrap").write_text("")
            proc = mock.Mock(returncode=0)
            events = []

            def completed(**kwargs):
                events.append("completed")
                return ('{"result":{"exit_code":7,"stderr":"bad"}}', "secret")

            proc.communicate.side_effect = completed
            with (
                mock.patch.object(run.subprocess, "Popen", return_value=proc) as popen,
                mock.patch.object(
                    run.sys.stdout,
                    "write",
                    side_effect=lambda _: events.append("reported"),
                ) as write,
            ):
                run.outer(backend, job)
            self.assertTrue(popen.call_args.kwargs["close_fds"])
            self.assertEqual(popen.call_args.kwargs["env"], run.ENV)
            proc.communicate.assert_called_once()
            self.assertEqual(events, ["completed", "reported"])
            write.assert_called_once()

    def test_seccomp_denies_all_non_file_channels(self):
        blocked = set(run.config_document()["sandbox"]["seccomp"]["syscalls"]["block"])
        required = {
            "socket",
            "socketpair",
            "connect",
            "keyctl",
            "ptrace",
            "pidfd_getfd",
            "kill",
            "tgkill",
            "mount",
            "setns",
            "unshare",
            "semget",
            "msgget",
            "shmget",
            "bpf",
            "perf_event_open",
            "io_uring_setup",
        }
        self.assertTrue(required <= blocked)
        self.assertFalse(run.config_document()["sandbox"]["seccomp"]["wait_killable"])

    def test_outer_failclosed_on_failure_or_malformed_output(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            job = self.make_job(root)
            backend = root / "backend"
            backend.mkdir()
            (backend / "agentsh").write_text("")
            (backend / "agentsh-unixwrap").write_text("")
            for code, output in ((1, '{"result":{"exit_code":0}}'), (0, "not-json")):
                proc = mock.Mock(returncode=code)
                proc.communicate.return_value = (output, "child secret")
                with (
                    mock.patch.object(run.subprocess, "Popen", return_value=proc),
                    mock.patch.object(run.sys.stdout, "write") as write,
                ):
                    with self.assertRaises(run.RunnerError):
                        run.outer(backend, job)
                    write.assert_not_called()

    def test_timeout_kills_and_waits(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            job = self.make_job(root)
            backend = root / "backend"
            backend.mkdir()
            (backend / "agentsh").write_text("")
            (backend / "agentsh-unixwrap").write_text("")
            proc = mock.Mock()
            proc.communicate.side_effect = run.subprocess.TimeoutExpired("bwrap", 1)
            with (
                mock.patch.object(run.subprocess, "Popen", return_value=proc),
                self.assertRaises(run.RunnerError),
            ):
                run.outer(backend, job)
            self.assertEqual(
                proc.method_calls[-2:], [mock.call.kill(), mock.call.wait()]
            )

    def test_daemon_teardown_waits_after_terminate(self):
        daemon = mock.Mock()
        daemon.poll.return_value = None
        run._stop_daemon(daemon)
        self.assertEqual(
            daemon.method_calls,
            [mock.call.poll(), mock.call.terminate(), mock.call.wait(timeout=3)],
        )


if __name__ == "__main__":
    unittest.main()
