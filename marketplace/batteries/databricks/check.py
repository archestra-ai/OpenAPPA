"""Check Databricks CLI authentication with a read-only current-user request."""
import json
import os
import subprocess

import service_principal

TOKEN_VAR = "APPA_PROVIDER_DATABRICKS_TOKEN"


def result(status, authentication, reason):
    return {"status": status, "authentication": authentication, "reason": reason}


def current_user(environ, run, authentication):
    """Whether the CLI, run in `environ`, signs in as anyone."""
    try:
        response = run(["databricks", "current-user", "me", "-o", "json"],
                       env=environ, capture_output=True, text=True, timeout=10, check=False)
        if response.returncode:
            # CLI errors conflate login failures and network failures; do not
            # claim credentials are invalid based only on its exit status.
            return result("unavailable", authentication, "check_failed")
        body = json.loads(response.stdout)
        if not isinstance(body, dict) or not body.get("id"):
            return result("unavailable", authentication, "check_failed")
    except FileNotFoundError:
        return result("needs_configuration", authentication, "missing_executable")
    except subprocess.TimeoutExpired:
        return result("unavailable", authentication, "check_timed_out")
    except (OSError, ValueError):
        return result("unavailable", authentication, "check_failed")
    return result("ready", authentication, "verified")


def check(environ=None, run=subprocess.run):
    environ = dict(os.environ if environ is None else environ)
    token = environ.get(TOKEN_VAR, "").strip()
    authentication = "token" if token else "cli"
    if token:
        environ["DATABRICKS_TOKEN"] = token
        if not environ.get("DATABRICKS_HOST"):
            return result("needs_configuration", authentication, "missing_configuration")
    return current_user(environ, run, authentication)


def check_service_principal(environ=None, run=subprocess.run):
    """A multi-user deployment's check: the CLI signs in as the service
    principal its environment names, and as nobody else."""
    try:
        environ = service_principal.environment(os.environ if environ is None else environ)
    except service_principal.MissingLogin:
        return result("needs_configuration", "token", "missing_configuration")
    return current_user(environ, run, "token")


if __name__ == "__main__":
    print(json.dumps(check()))
