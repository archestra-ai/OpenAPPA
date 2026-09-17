import importlib.util
import json
import resource
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

    def test_every_memory_backed_mount_is_sized(self):
        with tempfile.TemporaryDirectory() as root:
            job = self.make_job(root)
            cmd = run.bwrap_command(Path(root) / "backend", job, Path(root) / "run.py")
        sizes = []
        for index, argument in enumerate(cmd):
            if argument == "--tmpfs":
                self.assertEqual(cmd[index - 2], "--size", cmd)
                # bubblewrap takes a plain byte count; a suffixed size is a launch failure.
                sizes.append(cmd[index - 1])
        self.assertEqual(
            [cmd[i + 1] for i, argument in enumerate(cmd) if argument == "--tmpfs"],
            ["/tmp", "/job/work"],
        )
        self.assertEqual(sizes, [run.TMP_SIZE, run.WORK_SIZE])
        for size in sizes:
            self.assertTrue(size.isdigit() and int(size) > 0, size)

    def test_the_command_runs_under_resource_limits(self):
        # The shell that execs the command sets the ceilings dash can set, and refuses to run
        # without them.
        for limit in ("-c", "-f", "-v", "-n", "-t"):
            self.assertIn(f"ulimit {limit}", run.LIMIT_SCRIPT)
        self.assertEqual(run.LIMIT_SCRIPT.count("|| exit 125"), 5)
        self.assertTrue(run.LIMIT_SCRIPT.endswith('exec /bin/sh -c "$1"'))
        # dash has no option for the process count; that one is applied to the daemon.
        self.assertNotIn("-u ", run.LIMIT_SCRIPT)
        self.assertEqual(run.process_ceiling(resource.RLIM_INFINITY, resource.RLIM_INFINITY), run.MAX_PROCESSES)
        self.assertEqual(run.process_ceiling(64, resource.RLIM_INFINITY), 64)
        self.assertEqual(run.process_ceiling(resource.RLIM_INFINITY, 32), 32)

    def test_the_generated_configuration_is_fail_closed(self):
        run.assert_fail_closed(run.config_document())
        for path, weakened in (
            (("sandbox", "unix_sockets", "enabled"), False),
            (("sandbox", "allow_degraded"), True),
            (("sandbox", "seccomp", "mode"), "monitor"),
            (("landlock", "enabled"), False),
            (("security", "minimum_mode"), "none"),
        ):
            document = run.config_document()
            node = document
            for key in path[:-1]:
                node = node[key]
            node[path[-1]] = weakened
            with self.assertRaises(run.RunnerError):
                run.assert_fail_closed(document)
        document = run.config_document()
        document["sandbox"]["ptrace"] = {"enabled": True}
        with self.assertRaises(run.RunnerError):
            run.assert_fail_closed(document)
        document = run.config_document()
        del document["sandbox"]["seccomp"]
        with self.assertRaises(run.RunnerError):
            run.assert_fail_closed(document)

    def test_diagnostic_streams_are_truncated_with_a_mark(self):
        oversized = "x" * (run.MAX_STREAM_BYTES + 1)
        result = run.bounded({"result": {"exit_code": 0, "stdout": oversized, "stderr": "short"}})
        self.assertEqual(result["result"]["stderr"], "short")
        self.assertTrue(result["result"]["stdout"].endswith(run.TRUNCATION_MARK))
        self.assertEqual(
            len(result["result"]["stdout"].encode()),
            run.MAX_STREAM_BYTES + len(run.TRUNCATION_MARK),
        )
        # A multibyte character on the boundary is replaced, never split into invalid UTF-8.
        multibyte = "\u00e9" * run.MAX_STREAM_BYTES
        result = run.bounded({"result": {"exit_code": 0, "stdout": multibyte}})
        result["result"]["stdout"].encode()

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
