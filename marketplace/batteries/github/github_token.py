"""Where the GitHub battery's helpers get their token.

APPA_PROVIDER_GITHUB_TOKEN when the deployment sets it; otherwise the
GitHub CLI's login for the API host, read with `gh auth token`, which
also honours GH_TOKEN and GITHUB_TOKEN. The runtime forwards the helper
its own binding's variable and the rest of the process environment, so
`gh` runs with the PATH and HOME it needs. Neither present, the helper
stops with the two ways to fix it; nothing is guessed.
"""

import os
import subprocess
from urllib.parse import urlparse

TOKEN_VAR = "APPA_PROVIDER_GITHUB_TOKEN"
GH_TIMEOUT_SECONDS = 5


def api_root(environ):
    """The REST root the helpers call: GITHUB_API_URL when set, else api.github.com."""
    return (environ.get("GITHUB_API_URL") or "https://api.github.com").rstrip("/")


def api_hostname(environ):
    """The host `gh` is logged in to for the configured API root."""
    host = urlparse(api_root(environ)).hostname or "api.github.com"
    return "github.com" if host == "api.github.com" else host


def gh_token(environ):
    try:
        completed = subprocess.run(
            ["gh", "auth", "token", "--hostname", api_hostname(environ)],
            capture_output=True,
            text=True,
            timeout=GH_TIMEOUT_SECONDS,
            env=environ,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    token = completed.stdout.strip()
    return token if completed.returncode == 0 and token else None


def resolve_token(environ=None):
    environ = os.environ if environ is None else environ
    token = (environ.get(TOKEN_VAR) or "").strip()
    if token:
        return token
    token = gh_token(environ)
    if token:
        return token
    raise RuntimeError(
        f"{TOKEN_VAR} is not set and gh is not logged in to {api_hostname(environ)}: "
        f"run `gh auth login`, or export {TOKEN_VAR} where the runtime runs and restart it"
    )
