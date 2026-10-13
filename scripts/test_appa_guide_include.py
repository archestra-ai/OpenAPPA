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


def include(root: str, battery: str, target: str, *dirs: Path) -> str:
    composition = guide.Composition(
        str(APPA), (*dirs, REPOSITORY / "marketplace/batteries"), target
    )
    candidate, _ = guide.with_battery_include(root, battery)
    return guide.validated_include(candidate, battery, composition)


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
            plain = Path(directory) / "plain"
            plain.mkdir()
            (plain / "appa.toml").write_text("[policy]\nversion = 2\n")
            included = include(BOOTSTRAP, "plain", "multi-user", Path(directory))
            composition = guide.Composition(str(APPA), (Path(directory),), "multi-user")
            self.assertEqual(
                guide.with_lookup_rosters(included, "plain", composition), included
            )
        self.assertEqual(
            tomllib.loads(included)["include"], ["batteries/plain/appa.toml"]
        )
        self.assertNotIn("audience", tomllib.loads(included)["externals"])

    def test_a_roster_another_include_declares_is_not_duplicated(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            members = Path(directory) / "members"
            members.mkdir()
            (members / "appa.toml").write_text(
                "[policy]\nversion = 2\n\n[externals.audience.host-members]\n"
                'readers = { "github:583231" = "ana@corp.example" }\n'
            )
            root, _ = guide.with_battery_include(BOOTSTRAP, "members")
            included = include(root, "github", "multi-user", Path(directory))
        parsed = tomllib.loads(included)
        self.assertEqual(
            parsed["include"],
            ["batteries/members/appa.toml", "batteries/github/appa.toml"],
        )
        self.assertNotIn("audience", parsed["externals"])


if __name__ == "__main__":
    unittest.main()
