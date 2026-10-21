import importlib.util
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

    def test_a_multi_user_deployment_reads_the_variable_alone(self):
        self.stored("hf_stored")
        environ = {"HOME": self.home.name}
        self.assertEqual(HF_TOKEN.Deployment.SINGLE_USER.token(environ), "hf_stored")
        with self.assertRaises(RuntimeError):
            HF_TOKEN.Deployment.MULTI_USER.token(environ)
        with self.assertRaises(RuntimeError):
            HF_TOKEN.env_token({**environ, "APPA_PROVIDER_HUGGINGFACE_TOKEN": "  "})
        self.assertEqual(HF_TOKEN.Deployment.MULTI_USER.token({**environ, "APPA_PROVIDER_HUGGINGFACE_TOKEN": " hf_var\n"}), "hf_var")

    def test_the_hub_root_is_hf_endpoint(self):
        self.assertEqual(HF_TOKEN.hub_root({}), "https://huggingface.co")
        self.assertEqual(HF_TOKEN.hub_root({"HF_ENDPOINT": "http://127.0.0.1:9/"}), "http://127.0.0.1:9")


class HomeDirTests(unittest.TestCase):
    """home_dir(environ, windows) returns the home path or None."""

    POSIX_CASES = [
        # (environ,                          expected)
        ({"HOME": "/home/user"},             "/home/user"),
        ({},                                 None),
        ({"HOME": ""},                       None),
    ]

    WINDOWS_CASES = [
        # USERPROFILE takes priority
        ({"USERPROFILE": r"C:\Users\u", "HOMEDRIVE": "D:", "HOMEPATH": r"\other"}, r"C:\Users\u"),
        # HOMEDRIVE+HOMEPATH next
        ({"HOMEDRIVE": "C:", "HOMEPATH": r"\Users\u"},                             r"C:\Users\u"),
        # HOME last resort
        ({"HOME": r"C:\Users\u"},                                                   r"C:\Users\u"),
        # nothing set → None
        ({},                                                                        None),
        # empty USERPROFILE falls through
        ({"USERPROFILE": "", "HOME": r"C:\fallback"},                              r"C:\fallback"),
    ]

    def test_posix(self):
        for env, expected in self.POSIX_CASES:
            with self.subTest(env=env):
                self.assertEqual(HF_TOKEN.home_dir(env, windows=False), expected)

    def test_windows(self):
        for env, expected in self.WINDOWS_CASES:
            with self.subTest(env=env):
                self.assertEqual(HF_TOKEN.home_dir(env, windows=True), expected)



class TokenPathTests(unittest.TestCase):
    """token_path(environ, windows=) resolves the cache path on both platforms."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = self.tmp.name

    def _default(self):
        return Path(self.home) / ".cache" / "huggingface" / "token"

    def test_explicit_hf_token_path_wins(self):
        explicit = str(Path(self.home) / "tok")
        for windows in (True, False):
            with self.subTest(windows=windows):
                self.assertEqual(HF_TOKEN.token_path({"HF_TOKEN_PATH": explicit}, windows=windows), Path(explicit))

    def test_hf_home_sets_token_dir(self):
        hf_home = str(Path(self.home) / "hf")
        self.assertEqual(HF_TOKEN.token_path({"HF_HOME": hf_home}), Path(hf_home) / "token")

    def test_posix_home(self):
        self.assertEqual(HF_TOKEN.token_path({"HOME": self.home}, windows=False), self._default())

    def test_windows_userprofile(self):
        self.assertEqual(HF_TOKEN.token_path({"USERPROFILE": self.home}, windows=True), self._default())

    def test_windows_homedrive_homepath(self):
        s = self.home
        colon = s.find(":")
        drive, rest = (s[: colon + 1], s[colon + 1 :]) if colon != -1 else ("", s)
        env = {"HOMEDRIVE": drive, "HOMEPATH": rest}
        self.assertEqual(HF_TOKEN.token_path(env, windows=True), self._default())


if __name__ == "__main__":
    unittest.main()
