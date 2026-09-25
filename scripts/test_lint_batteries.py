import tempfile
from pathlib import Path
import shutil
import unittest

from scripts.lint_batteries import lint_battery


class BatteryLinterTests(unittest.TestCase):
    def make_battery(self, commands, helpers, files):
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, directory)
        command_text = "\n".join(
            f"[externals.audience.binding_{index}]\ncommand = {command!r}\n"
            for index, command in enumerate(commands)
        )
        (directory / "appa.toml").write_text(command_text)
        manifest = (
            'schema = 1\nname = "fixture"\ndescription = "fixture"\n\n'
            '[battery]\npolicy = "appa.toml"\nhosts = ["claude-code"]\n'
            f"helpers = {helpers!r}\n"
        )
        (directory / "appa-package.toml").write_text(manifest)
        for relative, contents in files.items():
            path = directory / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(contents)
        return directory

    @staticmethod
    def kinds(directory):
        return [diagnostic.kind for diagnostic in lint_battery(directory)]

    def test_direct_python_helper_is_used(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            ["entry.py"],
            {"entry.py": "print('ok')\n"},
        )
        self.assertEqual(self.kinds(directory), [])

    def test_python_helper_imported_by_another_helper_is_used(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            ["entry.py", "helper_module.py", "leaf_module.py"],
            {
                "entry.py": "from helper_module import answer\nprint(answer())\n",
                "helper_module.py": "from leaf_module import VALUE\ndef answer():\n    return VALUE\n",
                "leaf_module.py": "VALUE = 42\n",
            },
        )
        self.assertEqual(self.kinds(directory), [])

    def test_dynamic_python_import_is_reported_as_unsupported(self):
        for source in [
            "import importlib\nimportlib.import_module(get_module_name())\n",
            "from importlib import import_module as load\nload('helper_module')\n",
        ]:
            with self.subTest(source=source):
                directory = self.make_battery(
                    [["python3", "entry.py"]],
                    ["entry.py"],
                    {"entry.py": source},
                )
                self.assertIn("unsupported/dynamic command", self.kinds(directory))

    def test_orphan_python_script_is_reported(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            ["entry.py"],
            {"entry.py": "pass\n", "orphan.py": "pass\n"},
        )
        diagnostics = lint_battery(directory)
        self.assertIn("unreferenced production script", self.kinds(directory))
        self.assertTrue(any(diagnostic.file.name == "orphan.py" for diagnostic in diagnostics))

    def test_python_test_file_is_ignored(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            ["entry.py"],
            {
                "entry.py": "pass\n",
                "test_orphan.py": "pass\n",
                "nested/tests/not_a_helper.py": "pass\n",
                "test_cross_battery.py": "pass\n",
            },
        )
        self.assertEqual(self.kinds(directory), [])

    def test_only_python3_commands_are_accepted(self):
        for command, files in [
            (["python", "entry.py"], {"entry.py": "pass\n"}),
            (["bash", "entry.sh"], {"entry.sh": "#!/bin/sh\necho ok\n"}),
        ]:
            with self.subTest(command=command):
                directory = self.make_battery([command], [], files)
                self.assertIn("unsupported/dynamic command", self.kinds(directory))

    def test_missing_command_target_is_reported_with_a_toml_line(self):
        directory = self.make_battery(
            [["python3", "missing.py"]],
            ["missing.py"],
            {},
        )
        diagnostics = lint_battery(directory)
        missing = [d for d in diagnostics if d.kind == "missing dependency"]
        self.assertTrue(missing)
        self.assertEqual(missing[0].location.path.name, "appa.toml")
        self.assertIsNotNone(missing[0].location.line)

    def test_escaping_command_target_is_rejected(self):
        directory = self.make_battery(
            [["python3", "../outside.py"]],
            [],
            {},
        )
        diagnostics = lint_battery(directory)
        self.assertTrue(any(d.kind == "unsupported/dynamic command" and "escapes" in d.message for d in diagnostics))

    def test_command_target_must_be_a_declared_helper(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            [],
            {"entry.py": "pass\n"},
        )
        self.assertIn("command target missing from helpers", self.kinds(directory))

    def test_unused_manifest_helper_is_reported(self):
        directory = self.make_battery(
            [["python3", "entry.py"]],
            ["entry.py", "unused.py"],
            {"entry.py": "pass\n", "unused.py": "pass\n"},
        )
        self.assertIn("unused manifest helper", self.kinds(directory))

    def test_plugin_default_can_reach_an_installed_battery_helper(self):
        source = self.make_battery([], ["entry.py"], {"entry.py": "pass\n"})
        marketplace = source / "marketplace"
        battery = marketplace / "batteries" / "fixture"
        battery.parent.mkdir(parents=True)
        shutil.copytree(source, battery, ignore=shutil.ignore_patterns("marketplace"))
        plugin = marketplace / "plugins" / "claude-code"
        plugin.mkdir(parents=True)
        policy = plugin / "default.appa.toml"
        policy.write_text('[externals.inputs.helper]\ncommand = ["python3", "batteries/fixture/entry.py"]\n')
        self.assertEqual(self.kinds(battery), [])
        policy.write_text('[externals.inputs.helper]\ncommand = ["python3", "batteries/other/entry.py"]\n')
        self.assertIn("unused manifest helper", self.kinds(battery))

    def test_commands_are_discovered_inside_nested_toml_values(self):
        directory = self.make_battery(
            [],
            ["entry.py"],
            {"entry.py": "pass\n"},
        )
        (directory / "appa.toml").write_text(
            '[[bindings]]\nname = "nested"\ncommand = ["python3", "entry.py"]\n'
        )
        self.assertEqual(self.kinds(directory), [])


if __name__ == "__main__":
    unittest.main()
