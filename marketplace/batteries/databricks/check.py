"""Check Databricks CLI authentication with a read-only current-user request."""
import json
import os
import subprocess

TOKEN_VAR = "APPA_PROVIDER_DATABRICKS_TOKEN"


def check(environ=None, run=subprocess.run):
    environ = dict(os.environ if environ is None else environ)
    token = environ.get(TOKEN_VAR, "").strip()
    authentication = "token" if token else "cli"
    def result(status, reason):
        return {"status": status, "authentication": authentication, "reason": reason}
    if token:
        environ["DATABRICKS_TOKEN"] = token
        if not environ.get("DATABRICKS_HOST"):
            return result("needs_configuration", "missing_configuration")
    try:
        response = run(["databricks", "current-user", "me", "-o", "json"],
                       env=environ, capture_output=True, text=True, timeout=10, check=False)
        if response.returncode:
            # CLI errors conflate login failures and network failures; do not
            # claim credentials are invalid based only on its exit status.
            return result("unavailable", "check_failed")
        body = json.loads(response.stdout)
        if not isinstance(body, dict) or not body.get("id"):
            return result("unavailable", "check_failed")
    except FileNotFoundError:
        return result("needs_configuration", "missing_executable")
    except subprocess.TimeoutExpired:
        return result("unavailable", "check_timed_out")
    except (OSError, ValueError):
        return result("unavailable", "check_failed")
    return result("ready", "verified")


if __name__ == "__main__":
    print(json.dumps(check()))
