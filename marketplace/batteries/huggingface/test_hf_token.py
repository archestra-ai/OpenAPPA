import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
import contextlib
from unittest.mock import patch


MODULE = Path(__file__).with_name("hf_token.py")
SPEC = importlib.util.spec_from_file_location("hf_token", MODULE)
HF_TOKEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HF_TOKEN)


class ResolveToken(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.TemporaryDirectory()
        self.addCleanup(self.home.cleanup)

    def stored(self, token, path=None):
        path = path or Path(self.home.name) / ".cache" / "huggingface" / "token"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(token)
        return path

    def test_the_variable_wins_over_the_stored_login(self):
        self.stored("hf_stored")
        token = HF_TOKEN.resolve_token({"HOME": self.home.name, "APPA_PROVIDER_HUGGINGFACE_TOKEN": " hf_var\n"})
        self.assertEqual(token, "hf_var")

    def test_the_stored_login_answers_when_the_variable_is_blank_or_unset(self):
        self.stored("hf_stored\n")
        self.assertEqual(HF_TOKEN.resolve_token({"HOME": self.home.name}), "hf_stored")
        self.assertEqual(HF_TOKEN.resolve_token({"HOME": self.home.name, "APPA_PROVIDER_HUGGINGFACE_TOKEN": "  "}), "hf_stored")

    def test_hf_home_and_hf_token_path_relocate_the_stored_login(self):
        home = self.stored("from_hf_home", Path(self.home.name) / "hfhome" / "token")
        self.assertEqual(HF_TOKEN.resolve_token({"HF_HOME": str(home.parent)}), "from_hf_home")
        explicit = self.stored("from_path", Path(self.home.name) / "elsewhere")
        self.assertEqual(HF_TOKEN.resolve_token({"HF_HOME": str(home.parent), "HF_TOKEN_PATH": str(explicit)}), "from_path")

    def test_xdg_cache_home_relocates_the_default_login(self):
        cache = Path(self.home.name) / "xdg"
        self.stored("from_xdg", cache / "huggingface" / "token")
        self.assertEqual(HF_TOKEN.resolve_token({"HOME": self.home.name, "XDG_CACHE_HOME": str(cache)}), "from_xdg")

    def test_neither_present_names_both_fixes(self):
        self.stored("")
        with self.assertRaises(RuntimeError) as refused:
            HF_TOKEN.resolve_token({"HOME": self.home.name})
        self.assertIn("APPA_PROVIDER_HUGGINGFACE_TOKEN", str(refused.exception))
        self.assertIn("hf auth login", str(refused.exception))
        with self.assertRaises(RuntimeError):
            HF_TOKEN.resolve_token({})

    def test_the_hub_root_is_hf_endpoint(self):
        self.assertEqual(HF_TOKEN.hub_root({}), "https://huggingface.co")
        self.assertEqual(HF_TOKEN.hub_root({"HF_ENDPOINT": "http://127.0.0.1:9/"}), "http://127.0.0.1:9")

    def test_tilde_expansion_in_paths(self):
        self.stored("from_token_path")

        token = HF_TOKEN.resolve_token({
            "HOME": self.home.name,
            "HF_TOKEN_PATH": "~/.cache/huggingface/token"
        })
        self.assertEqual(token, "from_token_path")

        token = HF_TOKEN.resolve_token({
            "HOME": self.home.name,
            "HF_HOME": "~/.cache/huggingface"
        })
        self.assertEqual(token, "from_token_path")

        token = HF_TOKEN.resolve_token({
            "HOME": self.home.name,
            "XDG_CACHE_HOME": "~/.cache"
        })
        self.assertEqual(token, "from_token_path")

    def test_unresolved_explicit_tilde_remains_literal(self):
        with contextlib.chdir(self.home.name):
            with self.assertRaises(RuntimeError) as exc:
                HF_TOKEN.resolve_token({
                    "HF_TOKEN_PATH": "~/token"
                })
            self.assertIn("~/token", str(exc.exception).replace("\\", "/"))

    def test_unchanged_relative_path(self):
        with self.assertRaises(RuntimeError) as exc:
            HF_TOKEN.resolve_token({
                "HF_TOKEN_PATH": "./relative/token"
            })
        self.assertIn("relative/token", str(exc.exception).replace("\\", "/"))

    def test_empty_home_must_not_load_working_directory_token(self):
        with contextlib.chdir(self.home.name):
            fake_token_path = Path(".cache") / "huggingface" / "token"
            fake_token_path.parent.mkdir(parents=True, exist_ok=True)
            fake_token_path.write_text("cwd_token")

            with self.assertRaises(RuntimeError) as exc:
                HF_TOKEN.resolve_token({"HOME": ""})
            self.assertIn("APPA_PROVIDER_HUGGINGFACE_TOKEN is not set", str(exc.exception))


class ExpandTildeTests(unittest.TestCase):
    def check_expand(self, os_name, environ, path, expected):
        with patch.object(HF_TOKEN.os, "name", os_name):
            self.assertEqual(HF_TOKEN._expand_tilde(path, environ), expected)

    def test_windows_precedence(self):
        cases = [
            ({"USERPROFILE": "C:\\Users\\profile", "HOMEDRIVE": "D:", "HOMEPATH": "\\Users\\home", "HOME": "E:\\home"}, "~", "C:\\Users\\profile"),
            ({"HOMEDRIVE": "D:", "HOMEPATH": "\\Users\\home", "HOME": "E:\\home"}, "~", "D:\\Users\\home"),
            ({"HOME": "E:\\home"}, "~", "E:\\home"),
            ({"USERPROFILE": "", "HOME": "E:\\home"}, "~", "E:\\home"),
            ({"HOMEDRIVE": "", "HOMEPATH": "\\Users\\test", "HOME": "E:\\home"}, "~", "E:\\home"),
            ({"HOMEDRIVE": "C:", "HOMEPATH": "", "HOME": "E:\\home"}, "~", "E:\\home"),
            ({"USERPROFILE": "", "HOME": ""}, "~/foo", "~/foo"),
            ({"USERPROFILE": "C:\\Users\\test"}, "~/foo", "C:\\Users\\test/foo"),
            ({"USERPROFILE": "C:\\Users\\test"}, "~\\foo", "C:\\Users\\test\\foo"),
        ]
        for env, path, expected in cases:
            with self.subTest(env=env, path=path):
                self.check_expand("nt", env, path, expected)

    def test_unix_precedence(self):
        cases = [
            ({"HOME": "/home/test"}, "~", "/home/test"),
            ({"USERPROFILE": "/home/windows", "HOME": "/home/test"}, "~", "/home/test"),
            ({"HOME": ""}, "~/foo", "~/foo"),
            ({}, "~/foo", "~/foo"),
            ({"HOME": "/home/test"}, "~\\foo", "~\\foo"),
        ]
        for env, path, expected in cases:
            with self.subTest(env=env, path=path):
                self.check_expand("posix", env, path, expected)

    def test_shared_behavior(self):
        cases = [
            ({"HOME": "/home/test"}, "/absolute/path", "/absolute/path"),
            ({"HOME": "/home/test"}, "relative/path", "relative/path"),
            ({"HOME": "/home/test"}, "~", "/home/test"),
            ({"HOME": "/home/test"}, "~username/foo", "~username/foo"),
            ({"HOME": "/home/ test "}, "~", "/home/ test "),
            ({"HOME": " "}, "~", " "),
        ]
        for os_name in ("nt", "posix"):
            for env, path, expected in cases:
                with self.subTest(os_name=os_name, env=env, path=path):
                    self.check_expand(os_name, env, path, expected)


if __name__ == "__main__":
    unittest.main()
