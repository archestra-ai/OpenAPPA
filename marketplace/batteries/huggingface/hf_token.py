"""Where the Hugging Face battery's helpers get their token and Hub client.

APPA_PROVIDER_HUGGINGFACE_TOKEN when the deployment sets it; otherwise
the token the Hugging Face CLI stored at login (`hf auth login`):
HF_TOKEN_PATH when set, else `$HF_HOME/token`, else
`~/.cache/huggingface/token`. Neither present, the helper stops with the
two ways to fix it; nothing is guessed. The Hub root is HF_ENDPOINT when
set, else https://huggingface.co; `hub_api` reads one JSON path of it
with the token, telling a 404 and a 401/403 apart for the caller.
"""

import json
import os
import urllib.error
import urllib.request
from pathlib import Path

TOKEN_VAR = "APPA_PROVIDER_HUGGINGFACE_TOKEN"
TIMEOUT_SECONDS = 30


class NotFound(Exception):
    """The Hub answered 404: the path names nothing the token can see."""


class Forbidden(Exception):
    """The Hub answered 401 or 403: the token lacks the right for this path."""


def hub_api(token, environ=None):
    root = hub_root(os.environ if environ is None else environ)

    def call(path):
        request = urllib.request.Request(
            f"{root}{path}",
            headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=TIMEOUT_SECONDS) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            match error.code:
                case 404:
                    raise NotFound(path) from error
                case 401 | 403:
                    raise Forbidden(path) from error
                case status:
                    raise RuntimeError(f"GET {path} failed: {status}") from error
        except json.JSONDecodeError as error:
            raise RuntimeError(f"GET {path} answered no JSON") from error

    return call


def hub_root(environ):
    return (environ.get("HF_ENDPOINT") or "https://huggingface.co").rstrip("/")


def token_path(environ):
    if path := environ.get("HF_TOKEN_PATH"):
        return Path(path)
    if home := environ.get("HF_HOME"):
        return Path(home) / "token"
    return Path(environ.get("HOME") or "/nonexistent") / ".cache" / "huggingface" / "token"


def stored_token(environ):
    try:
        token = token_path(environ).read_text().strip()
    except OSError:
        return None
    return token or None


def resolve_token(environ=None):
    environ = os.environ if environ is None else environ
    token = (environ.get(TOKEN_VAR) or "").strip()
    if token:
        return token
    token = stored_token(environ)
    if token:
        return token
    raise RuntimeError(
        f"{TOKEN_VAR} is not set and {token_path(environ)} holds no token: "
        f"run `hf auth login`, or export {TOKEN_VAR} where the runtime runs and restart it"
    )
