import tempfile
import unittest
from pathlib import Path

from scripts.lint_battery_toml import lint


class BatteryTomlLintTests(unittest.TestCase):
    def test_accepts_only_the_required_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            battery = root / "example"
            battery.mkdir()
            (battery / "appa-package.toml").write_text('[battery]\npolicy = "appa.toml"\n')
            (battery / "appa.toml").write_text("")
            (battery / "README.md").write_text("")

            self.assertEqual(lint(root), [])

    def test_reports_missing_and_extra_toml_even_in_nested_directories(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            battery = root / "example"
            nested = battery / "fixtures"
            nested.mkdir(parents=True)
            (battery / "appa-package.toml").write_text('[battery]\npolicy = "appa.toml"\n')
            (battery / "unused.toml").write_text("")
            (nested / "copy.toml").write_text("")
            (root / "orphan.toml").write_text("")

            errors = lint(root)

            self.assertEqual(len(errors), 4)
            for name in ("appa.toml", "unused.toml", "copy.toml", "orphan.toml"):
                self.assertTrue(any(name in error for error in errors), errors)

    def test_reports_battery_without_any_toml(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "example").mkdir()

            self.assertEqual(len(lint(root)), 2)

    def test_rejects_manifest_that_uses_a_different_policy(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            battery = root / "example"
            battery.mkdir()
            (battery / "appa-package.toml").write_text('[battery]\npolicy = "policy.txt"\n')
            (battery / "appa.toml").write_text("")

            errors = lint(root)
            self.assertEqual(len(errors), 1)
            self.assertIn("battery.policy must name appa.toml", errors[0])


if __name__ == "__main__":
    unittest.main()
