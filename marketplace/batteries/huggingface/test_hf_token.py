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

    def test_the_hub_root_is_hf_endpoint(self):
        self.assertEqual(HF_TOKEN.hub_root({}), "https://huggingface.co")
        self.assertEqual(HF_TOKEN.hub_root({"HF_ENDPOINT": "http://127.0.0.1:9/"}), "http://127.0.0.1:9")


if __name__ == "__main__":
    unittest.main()
