"""Where the Hugging Face battery's helpers get their token and Hub client.

APPA_PROVIDER_HUGGINGFACE_TOKEN when the deployment sets it; otherwise
the token the Hugging Face CLI stored at login (`hf auth login`):
HF_TOKEN_PATH when set, else `$HF_HOME/token`, else
`$XDG_CACHE_HOME/huggingface/token` (`~/.cache` when unset). Neither present, the helper stops with the
two ways to fix it; nothing is guessed. The Hub root is HF_ENDPOINT when
set, else https://huggingface.co; `hub_api` reads one JSON path of it
with the token, telling a 404 and a 401/403 apart for the caller. A
multi-user deployment's helpers read the variable alone (`env_token`).
"""

import json
import os
from enum import Enum
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


def home_dir(environ, windows: bool) -> "str | None":
    """Return the user's home directory from *environ*, or None if not found.

    On Windows the canonical variables are USERPROFILE and HOMEDRIVE+HOMEPATH;
    HOME is consulted last as a fallback.  On POSIX only HOME is used.
    """
    if windows:
        userprofile = environ.get("USERPROFILE", "")
        if userprofile:
            return userprofile
        drive = environ.get("HOMEDRIVE", "")
        path = environ.get("HOMEPATH", "")
        if drive or path:
            return drive + path
        # Fall through to HOME as a last resort (e.g. Git-bash on Windows)
        home = environ.get("HOME", "")
        return home if home else None
    else:
        home = environ.get("HOME", "")
        return home if home else None


def token_path(environ, windows: bool = os.name == "nt"):
    if path := environ.get("HF_TOKEN_PATH"):
        return Path(path)
    if home := environ.get("HF_HOME"):
        return Path(home) / "token"
    cache = environ.get("XDG_CACHE_HOME")
    if not cache:
        home = home_dir(environ, windows)
        if home is None:
            cache = "/nonexistent"
        else:
            cache = str(Path(home) / ".cache")
    return Path(cache) / "huggingface" / "token"


def stored_token(environ):
    try:
        token = token_path(environ).read_text().strip()
    except OSError:
        return None
    return token or None


def env_token(environ=None):
    """The deployment's token alone. A multi-user deployment's token is
    shared, so a CLI login stored on the host, some one person's, is never
    read."""
    environ = os.environ if environ is None else environ
    token = (environ.get(TOKEN_VAR) or "").strip()
    if not token:
        raise RuntimeError(f"{TOKEN_VAR} is not set: set it where the runtime runs and restart it")
    return token


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


class Deployment(Enum):
    """Whose token the helpers hold. A single-user deployment's is one
    person's, the viewer, from the variable or the CLI login. A multi-user
    deployment's is shared: it has no viewer an answer may compare an
    owner with, and it comes from the variable alone."""

    SINGLE_USER = "single-user"
    MULTI_USER = "multi-user"

    @property
    def has_viewer(self):
        return self is Deployment.SINGLE_USER

    def token(self, environ=None):
        match self:
            case Deployment.SINGLE_USER:
                return resolve_token(environ)
            case Deployment.MULTI_USER:
                return env_token(environ)
