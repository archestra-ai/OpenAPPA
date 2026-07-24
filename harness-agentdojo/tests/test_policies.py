import subprocess
import sys
from pathlib import Path

import pytest
from agentdojo.task_suite.load_suites import get_suite

from appa_dojo.bridge import Admitted, AuthorizedCall, Blocked, Declined, SidecarClient
from appa_dojo.policies import load_policy

PROJECT_ROOT = Path(__file__).resolve().parents[1]


@pytest.mark.parametrize(
    ("suite_name", "policy_name"),
    [
        ("workspace", "workspace"),
        ("workspace", "workspace-open"),
        ("slack", "slack-open"),
        ("slack", "slack-practical"),
        ("slack", "slack-complete"),
    ],
)
def test_policy_loads_in_the_sdk_and_covers_the_suite(
    suite_name: str,
    policy_name: str,
) -> None:
    suite = get_suite("v1.2.2", suite_name)
    tools = {tool.name for tool in suite.tools}
    policy = load_policy(policy_name)
    policy.check_covers(tools)

    with SidecarClient() as sidecar:
        sidecar.open(policy.toml, sorted(tools), "evaluate the workspace task")


def test_practical_slack_policy_reads_web_but_blocks_later_egress() -> None:
    suite = get_suite("v1.2.2", "slack")
    policy = load_policy("slack-practical")

    with SidecarClient() as sidecar:
        sidecar.open(
            policy.toml,
            sorted(tool.name for tool in suite.tools),
            "read and publish",
        )
        assert isinstance(sidecar.check("get_webpage", {"url": "www.example.com"}), Blocked)
        # Informed acceptance: accept the read remedy in a later round than its offer.
        sidecar.new_round()
        assert isinstance(sidecar.resolve_remedy("remedy-0"), AuthorizedCall)
        assert isinstance(sidecar.report_success("external content"), Admitted)
        assert isinstance(
            sidecar.check(
                "post_webpage",
                {"url": "www.example.com", "content": "external content"},
            ),
            Blocked,
        )


def test_complete_slack_policy_cannot_dispatch_indivisible_web_read() -> None:
    suite = get_suite("v1.2.2", "slack")
    policy = load_policy("slack-complete")

    with SidecarClient() as sidecar:
        sidecar.open(
            policy.toml,
            sorted(tool.name for tool in suite.tools),
            "read the webpage",
        )
        assert isinstance(sidecar.check("get_webpage", {"url": "www.example.com"}), Blocked)
        assert isinstance(sidecar.resolve_remedy("remedy-0"), Declined)


def test_policy_resources_load_from_the_built_wheel(tmp_path: Path) -> None:
    subprocess.run(
        ["uv", "build", "--wheel", "--out-dir", str(tmp_path)],
        cwd=PROJECT_ROOT,
        check=True,
    )
    wheel = next(tmp_path.glob("appa_dojo-*.whl"))
    code = (
        "import sys; "
        f"sys.path.insert(0, {str(wheel)!r}); "
        "from appa_dojo.policies import load_policy; "
        "assert load_policy('workspace').tools"
    )
    subprocess.run([sys.executable, "-I", "-c", code], cwd=tmp_path, check=True)
