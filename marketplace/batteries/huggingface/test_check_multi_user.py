from http.server import BaseHTTPRequestHandler, HTTPServer
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("check-multi-user.py")


class Whoami:
    """One stdlib HTTP server standing in for the Hub's whoami-v2."""

    def __init__(self):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                seen.append((self.path, self.headers.get("Authorization")))
                body = json.dumps({"type": "user", "name": "shared"}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        self.seen = seen
        self.server = HTTPServer(("127.0.0.1", 0), Handler)

    def __enter__(self):
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()


def run(env):
    result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True, env=env)
    return json.loads(result.stdout)


class CheckMultiUser(unittest.TestCase):
    def test_the_variable_is_checked_and_a_stored_login_is_never_read(self):
        with tempfile.TemporaryDirectory() as home, Whoami() as hub:
            stored = Path(home) / ".cache" / "huggingface" / "token"
            stored.parent.mkdir(parents=True)
            stored.write_text("hf_stored")
            env = {"PATH": "/usr/bin:/bin", "HOME": home, "HF_ENDPOINT": f"http://127.0.0.1:{hub.server.server_port}"}
            missing = run(env)
            self.assertEqual(hub.seen, [])
            ready = run({**env, "APPA_PROVIDER_HUGGINGFACE_TOKEN": "hf_shared"})
        self.assertEqual(missing, {"status": "needs_configuration", "authentication": "token", "reason": "missing_credential"})
        self.assertEqual(ready, {"status": "ready", "authentication": "token", "reason": "verified"})
        self.assertEqual(hub.seen, [("/api/whoami-v2", "Bearer hf_shared")])


if __name__ == "__main__":
    unittest.main()
