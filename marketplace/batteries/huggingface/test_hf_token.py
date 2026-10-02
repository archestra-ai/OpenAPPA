import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


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

    def test_isolation_from_global_os_environ(self):
        self.stored("fake_token")

        old_home = os.environ.get("HOME")
        old_userprofile = os.environ.get("USERPROFILE")

        os.environ["HOME"] = self.home.name
        os.environ["USERPROFILE"] = self.home.name

        try:
            with self.assertRaises(RuntimeError):
                HF_TOKEN.resolve_token({})
        finally:
            if old_home is not None:
                os.environ["HOME"] = old_home
            else:
                del os.environ["HOME"]

            if old_userprofile is not None:
                os.environ["USERPROFILE"] = old_userprofile
            else:
                del os.environ["USERPROFILE"]

    def test_windows_userprofile_takes_precedence_over_home(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        with tempfile.TemporaryDirectory() as other_dir:
            self.stored("from_userprofile")
            self.stored("from_home", Path(other_dir) / ".cache" / "huggingface" / "token")

            token = HF_TOKEN.resolve_token({
                "USERPROFILE": self.home.name,
                "HOME": other_dir
            })
            self.assertEqual(token, "from_userprofile")

    def test_windows_homedrive_homepath_fallback(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        with tempfile.TemporaryDirectory() as other_dir:
            self.stored("from_homedrive")
            self.stored("from_home", Path(other_dir) / ".cache" / "huggingface" / "token")

            drive, path = os.path.splitdrive(self.home.name)

            token = HF_TOKEN.resolve_token({
                "HOMEDRIVE": drive,
                "HOMEPATH": path,
                "HOME": other_dir
            })
            self.assertEqual(token, "from_homedrive")

    def test_windows_incomplete_homedrive_falls_back_to_home(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        self.stored("from_home")

        token = HF_TOKEN.resolve_token({
            "HOMEDRIVE": "C:",
            "HOME": self.home.name
        })
        self.assertEqual(token, "from_home")

    def test_windows_incomplete_homedrive_fails_without_home(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        with self.assertRaises(RuntimeError):
            HF_TOKEN.resolve_token({
                "HOMEDRIVE": "C:"
            })

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

    def test_platform_specific_separator(self):
        self.stored("from_backslash")

        if os.name == 'nt':
            token = HF_TOKEN.resolve_token({
                "HOME": self.home.name,
                "HF_TOKEN_PATH": "~\\.cache\\huggingface\\token"
            })
            self.assertEqual(token, "from_backslash")
        else:
            with self.assertRaises(RuntimeError):
                HF_TOKEN.resolve_token({
                    "HOME": self.home.name,
                    "HF_TOKEN_PATH": "~\\.cache/huggingface/token"
                })

    def test_unresolved_explicit_tilde_remains_literal(self):
        old_cwd = os.getcwd()
        os.chdir(self.home.name)
        try:
            with self.assertRaises(RuntimeError) as exc:
                HF_TOKEN.resolve_token({
                    "HF_TOKEN_PATH": "~/token"
                })
            self.assertIn("~/token", str(exc.exception).replace("\\", "/"))
        finally:
            os.chdir(old_cwd)

    def test_unchanged_relative_path(self):
        with self.assertRaises(RuntimeError) as exc:
            HF_TOKEN.resolve_token({
                "HF_TOKEN_PATH": "./relative/token"
            })
        self.assertIn("relative/token", str(exc.exception).replace("\\", "/"))

    def test_empty_home_must_not_load_working_directory_token(self):
        old_cwd = os.getcwd()
        os.chdir(self.home.name)
        try:
            fake_token_path = Path(".cache") / "huggingface" / "token"
            fake_token_path.parent.mkdir(parents=True, exist_ok=True)
            fake_token_path.write_text("cwd_token")

            with self.assertRaises(RuntimeError) as exc:
                HF_TOKEN.resolve_token({"HOME": ""})
            self.assertIn("APPA_PROVIDER_HUGGINGFACE_TOKEN is not set", str(exc.exception))
        finally:
            os.chdir(old_cwd)

    def test_empty_userprofile_allows_fallback_to_home(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        with tempfile.TemporaryDirectory() as other_dir:
            self.stored("from_home", Path(other_dir) / ".cache" / "huggingface" / "token")

            token = HF_TOKEN.resolve_token({
                "USERPROFILE": "",
                "HOME": other_dir
            })
            self.assertEqual(token, "from_home")

    def test_empty_userprofile_with_no_valid_fallback(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        old_cwd = os.getcwd()
        os.chdir(self.home.name)
        try:
            fake_token_path = Path(".cache") / "huggingface" / "token"
            fake_token_path.parent.mkdir(parents=True, exist_ok=True)
            fake_token_path.write_text("cwd_token")

            with self.assertRaises(RuntimeError):
                HF_TOKEN.resolve_token({
                    "USERPROFILE": ""
                })
        finally:
            os.chdir(old_cwd)

    def test_empty_homedrive_or_homepath_fails_or_falls_back(self):
        if os.name != 'nt':
            self.skipTest("Windows-specific precedence test")

        with tempfile.TemporaryDirectory() as other_dir:
            self.stored("from_home", Path(other_dir) / ".cache" / "huggingface" / "token")

            token = HF_TOKEN.resolve_token({
                "HOMEDRIVE": "",
                "HOMEPATH": "\\Users\\test",
                "HOME": other_dir
            })
            self.assertEqual(token, "from_home")

            token = HF_TOKEN.resolve_token({
                "HOMEDRIVE": "C:",
                "HOMEPATH": "",
                "HOME": other_dir
            })
            self.assertEqual(token, "from_home")

        old_cwd = os.getcwd()
        os.chdir(self.home.name)
        try:
            fake_token_path = Path(".cache") / "huggingface" / "token"
            fake_token_path.parent.mkdir(parents=True, exist_ok=True)
            fake_token_path.write_text("cwd_token")

            with self.assertRaises(RuntimeError):
                HF_TOKEN.resolve_token({
                    "HOMEDRIVE": "",
                    "HOMEPATH": "\\Users\\test"
                })
        finally:
            os.chdir(old_cwd)


if __name__ == "__main__":
    unittest.main()
