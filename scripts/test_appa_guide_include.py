"""appa-guide's battery include against the runtime's own composition.

Drives a built appa binary: `APPA_BIN`, else `target/debug/appa`.
"""

from __future__ import annotations

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

import tomllib

SCRIPT = Path(__file__).with_name("appa-guide-runtime.py")
SPEC = importlib.util.spec_from_file_location("appa_guide_runtime", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
guide = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guide)

REPOSITORY = SCRIPT.parent.parent
APPA = Path(os.environ.get("APPA_BIN", REPOSITORY / "target/debug/appa"))
BOOTSTRAP = (REPOSITORY / "charts/appa-runtime/files/appa.toml").read_text()
MEMBERS = (
    "[policy]\nversion = 2\n\n[externals.audience.host-members]\n"
    'readers = { "github:583231" = "ana@corp.example" }\n'
)
RELAY = (
    "[policy]\nversion = 2\n\n[externals.audience.relay]\n"
    'command = ["python3", "relay.py"]\nlookup = "relay-members"\n'
    'selectors = [{ template = "team/<team>" }]\n'
)


def include(root: str, battery: str, target: str, batteries: Path | None = None) -> str:
    extra = () if batteries is None else (batteries,)
    composition = guide.Composition(
        str(APPA), (*extra, REPOSITORY / "marketplace/batteries"), target
    )
    candidate, _ = guide.with_battery_include(root, battery)
    return guide.validated_include(candidate, composition)


def fragments(directory: Path, **policies: str) -> Path:
    for name, policy in policies.items():
        (directory / name).mkdir()
        (directory / name / "appa.toml").write_text(policy)
    return directory


class BatteryIncludeCompositionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not os.access(APPA, os.X_OK):
            raise RuntimeError(f"build the runtime binary first: {APPA}")

    def test_multi_user_adds_the_empty_roster_its_target_looks_up(self) -> None:
        included = include(BOOTSTRAP, "github", "multi-user")
        audience = tomllib.loads(included)["externals"]["audience"]
        self.assertEqual(audience, {"host-members": {"readers": {}}})
        self.assertTrue(
            guide.preserves_policy_shape(
                tomllib.loads(BOOTSTRAP), tomllib.loads(included)
            )
        )

    def test_single_user_adds_no_roster(self) -> None:
        included = include(BOOTSTRAP, "github", "single-user")
        self.assertNotIn("audience", tomllib.loads(included)["externals"])

    def test_a_manifestless_battery_is_included(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            batteries = fragments(Path(directory), plain="[policy]\nversion = 2\n")
            included = include(BOOTSTRAP, "plain", "multi-user", batteries)
        parsed = tomllib.loads(included)
        self.assertEqual(parsed["include"], ["batteries/plain/appa.toml"])
        self.assertNotIn("audience", parsed["externals"])

    def test_a_roster_another_include_declares_is_not_duplicated(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            batteries = fragments(Path(directory), members=MEMBERS)
            root, _ = guide.with_battery_include(BOOTSTRAP, "members")
            included = include(root, "github", "multi-user", batteries)
        parsed = tomllib.loads(included)
        self.assertEqual(
            parsed["include"],
            ["batteries/members/appa.toml", "batteries/github/appa.toml"],
        )
        self.assertNotIn("audience", parsed["externals"])

    def test_only_the_lookup_no_source_declares_is_added(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            batteries = fragments(Path(directory), members=MEMBERS, relay=RELAY)
            root, _ = guide.with_battery_include(BOOTSTRAP, "members")
            root, _ = guide.with_battery_include(root, "relay")
            included = include(root, "github", "multi-user", batteries)
        audience = tomllib.loads(included)["externals"]["audience"]
        self.assertEqual(audience, {"relay-members": {"readers": {}}})

    def test_inline_externals_tables_take_the_roster(self) -> None:
        policy = "[policy]\nversion = 2\n"
        for root, expected in (
            (
                "externals = { timeout_ms = 5000, max_body_bytes = 4096 }\n" + policy,
                {"timeout_ms": 5000, "max_body_bytes": 4096},
            ),
            (
                policy
                + "[externals]\ntimeout_ms = 5000\nmax_body_bytes = 4096\naudience = {}\n",
                {"timeout_ms": 5000, "max_body_bytes": 4096},
            ),
        ):
            included = include(root, "github", "multi-user")
            externals = tomllib.loads(included)["externals"]
            self.assertEqual(
                externals, {**expected, "audience": {"host-members": {"readers": {}}}}
            )


if __name__ == "__main__":
    unittest.main()
