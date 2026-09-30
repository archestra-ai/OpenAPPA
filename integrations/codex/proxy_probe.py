#!/usr/bin/env python3
"""Exercise the APPA wrapper inside the installed Codex command sandbox.

This uses a disposable project and synthetic output. It makes no model call.
"""

import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import uuid


def main() -> int:
    root = Path(__file__).resolve().parents[2]
    appa = root / "target/debug/appa"
    if not appa.is_file():
        print(json.dumps({"status": "error", "reason": "build target/debug/appa first"}))
        return 1
    with tempfile.TemporaryDirectory(prefix="appa-codex-proxy-") as directory:
        project = Path(directory)
        home = project / "codex-home"
        home.mkdir()
        (home / "config.toml").write_text(
            '[features]\nnetwork_proxy = true\n'
            '[permissions.probe]\nextends = ":workspace"\n'
            '[permissions.probe.network]\nenabled = true\n'
            '[permissions.probe.network.domains]\n"127.0.0.1" = "allow"\n'
        )
        policy = project / "policy.toml"
        policy.write_text(
            '[policy]\nversion = 2\n[[policy.tool]]\n'
            'name = "host/codex/appa_exec"\n'
            '[externals]\ntimeout_ms = 5000\nmax_body_bytes = 65536\n'
        )
        runtime = subprocess.Popen(
            [str(appa), "runtime", "--adapter", "codex", "--config", str(policy),
             "--db", str(project / "appa.db"), "--listen", "127.0.0.1:0"],
            cwd=project, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        )
        try:
            url = runtime.stdout.readline().strip()
            if not url.startswith("http://127.0.0.1:"):
                raise RuntimeError("runtime did not print a loopback URL")
            command = "printf APPA_PROXY_BEGIN; sleep 1; printf APPA_PROXY_END"
            event = {
                "hook_event_name": "PreToolUse", "session_id": "proxy-probe",
                "tool_name": "Bash", "tool_use_id": "call-1", "cwd": str(project),
                "tool_input": {"command": command},
            }
            hook_env = dict(os.environ, APPA_GATE="1", SHELL="/bin/sh")
            hook_env.pop("APPA_RUNTIME_URL", None)
            hook_args = [str(appa), "hook", "--adapter", "codex", "--deployment-url", url]
            pre = subprocess.run(hook_args, input=json.dumps(event), text=True, capture_output=True,
                                 env=hook_env, timeout=15)
            if pre.returncode != 0:
                raise RuntimeError(f"pre-hook refused: {pre.stderr[-300:]}")
            answer = json.loads(pre.stdout)
            wrapper = answer["hookSpecificOutput"]["updatedInput"]["command"]
            if command in wrapper:
                raise RuntimeError("the wrapper exposes the original command")
            sandbox_env = dict(os.environ, CODEX_HOME=str(home))
            sandbox = subprocess.Popen(
                ["codex", "sandbox", "-P", "probe", "-C", str(project), "/bin/sh", "-c", wrapper],
                cwd=project, env=sandbox_env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
            early_polls = [bool(select.select([sandbox.stdout], [], [], 0.3)[0]) for _ in range(3)]
            stdout, stderr = sandbox.communicate(timeout=20)
            event.update({"hook_event_name": "PostToolUse", "tool_input": {"command": wrapper},
                          "tool_response": stdout.decode(errors="replace")})
            post = subprocess.run(hook_args, input=json.dumps(event), text=True, capture_output=True,
                                  env=hook_env, timeout=15)
            forged = subprocess.run(
                ["codex", "sandbox", "-P", "probe", "-C", str(project), str(appa),
                 "codex-exec", "--url", url, str(uuid.uuid4())],
                cwd=project, env=sandbox_env, capture_output=True, timeout=15,
            )
            valid = (
                not any(early_polls) and sandbox.returncode == 0 and stdout == b"APPA_PROXY_BEGINAPPA_PROXY_END"
                and post.returncode == 0 and post.stdout == "{}"
                and forged.returncode != 0 and b"APPA_PROXY" not in forged.stdout + forged.stderr
            )
            print(json.dumps({
                "status": "pass" if valid else "fail", "sandbox_exit": sandbox.returncode,
                "early_stdout_polls": early_polls,
                "output_marker": "complete" if stdout == b"APPA_PROXY_BEGINAPPA_PROXY_END" else "other",
                "post_ack": post.returncode == 0 and post.stdout == "{}",
                "forged_handle_refused": forged.returncode != 0 and b"APPA_PROXY" not in forged.stdout + forged.stderr,
                "sandbox_stderr_tail": stderr.decode(errors="replace")[-300:] if not valid else "",
            }, indent=2))
            return 0 if valid else 1
        finally:
            runtime.terminate()
            try:
                runtime.wait(timeout=3)
            except subprocess.TimeoutExpired:
                runtime.kill()
                runtime.wait()


if __name__ == "__main__":
    sys.exit(main())
