"""Exercise runner environment and failure propagation without models or a cluster."""

import os
import subprocess
from pathlib import Path

import pytest


@pytest.mark.parametrize("runtime,agent,port", [
    ("python", "cluster-ops", 18089),
    ("go", "cluster-ops-go", 18090),
])
@pytest.mark.parametrize("exit_code", [0, 7])
def test_a2a_runner_selects_the_same_protocol_agent(tmp_path, runtime, agent, port, exit_code):
    uv = tmp_path / "uv"
    uv.write_text(
        '#!/bin/sh\n'
        'printf "probe=%s endpoint=%s enabled=%s\\n" "$APPA_E2E_AGENT" "$APPA_A2A_URL" "$APPA_A2A_E2E"\n'
        f'exit {exit_code}\n'
    )
    uv.chmod(0o755)
    env = {key: value for key, value in os.environ.items() if not key.startswith("APPA_")}
    env["PATH"] = str(tmp_path) + os.pathsep + env.get("PATH", "")
    result = subprocess.run(
        ["bash", str(Path(__file__).parent / "run-matrix.sh"), runtime, "a2a"],
        env=env, capture_output=True, text=True, timeout=10, check=False,
    )
    assert result.returncode == exit_code
    assert f"probe={agent} endpoint=http://127.0.0.1:{port}/ enabled=1" in result.stdout
