"""Where the Databricks battery's helper gets its workspace and token.

The workspace is APPA_PROVIDER_DATABRICKS_HOST when the deployment sets
it, else DATABRICKS_HOST, the Databricks SDK's own variable, else the
host the Databricks CLI resolves for its login (`databricks auth
describe`). The token is APPA_PROVIDER_DATABRICKS_TOKEN when set, else
the CLI's cached login for that host (`databricks auth token`), which
refreshes an expiring token itself. The runtime forwards the helper its
own binding's variable and the rest of the process environment, so the
CLI runs with the PATH and HOME it needs. Neither present, the helper
stops with the two ways to fix it; nothing is guessed.
"""

import json
import os
import subprocess
from urllib.parse import urlparse

TOKEN_VAR = "APPA_PROVIDER_DATABRICKS_TOKEN"
HOST_VAR = "APPA_PROVIDER_DATABRICKS_HOST"
SDK_HOST_VAR = "DATABRICKS_HOST"
CLI_TIMEOUT_SECONDS = 5


def workspace_url(text):
    """`https://<host>` for a workspace spelled with or without its scheme,
    `None` for a blank or unusable value."""
    text = (text or "").strip()
    if not text:
        return None
    if "://" not in text:
        text = f"https://{text}"
    parsed = urlparse(text)
    if parsed.scheme != "https" or not parsed.hostname or parsed.path not in ("", "/") or parsed.username is not None:
        return None
    return f"https://{parsed.netloc}"


def cli_json(environ, arguments):
    """The parsed JSON one `databricks` command prints, `None` on any failure."""
    try:
        completed = subprocess.run(
            ["databricks", *arguments],
            capture_output=True,
            text=True,
            timeout=CLI_TIMEOUT_SECONDS,
            env=environ,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if completed.returncode != 0:
        return None
    try:
        document = json.loads(completed.stdout)
    except ValueError:
        return None
    return document if isinstance(document, dict) else None


def resolve_host(environ):
    """The workspace a set variable names, else the CLI's; a variable that is set
    to something unusable is refused, never skipped for the next source."""
    for variable in (HOST_VAR, SDK_HOST_VAR):
        value = (environ.get(variable) or "").strip()
        if not value:
            continue
        host = workspace_url(value)
        if not host:
            raise RuntimeError(f"{variable} is {value!r}, not a workspace URL such as https://<workspace>.cloud.databricks.com")
        return host
    described = cli_json(environ, ["auth", "describe", "-o", "json"]) or {}
    details = described.get("details")
    return workspace_url(details.get("host")) if isinstance(details, dict) else None


def cli_token(environ, host):
    answer = cli_json(environ, ["auth", "token", "--host", host]) or {}
    token = answer.get("access_token")
    return token.strip() if isinstance(token, str) and token.strip() else None


def resolve(environ=None):
    """The workspace URL and the token the helper calls it with."""
    environ = os.environ if environ is None else environ
    host = resolve_host(environ)
    if not host:
        raise RuntimeError(
            f"{HOST_VAR} is not set and the Databricks CLI resolves no workspace: "
            f"run `databricks auth login --host <workspace>`, or export {HOST_VAR} where the runtime runs and restart it"
        )
    token = (environ.get(TOKEN_VAR) or "").strip()
    if token:
        return host, token
    token = cli_token(environ, host)
    if token:
        return host, token
    raise RuntimeError(
        f"{TOKEN_VAR} is not set and the Databricks CLI is not logged in to {host}: "
        f"run `databricks auth login --host {host}`, or export {TOKEN_VAR} where the runtime runs and restart it"
    )
