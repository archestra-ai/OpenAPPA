"""The Databricks CLI environment of a multi-user deployment.

A multi-user deployment acts as one service principal, never as the person
who installed the CLI: the CLI otherwise falls back to the default profile in
~/.databrickscfg, the profile DATABRICKS_CONFIG_PROFILE names, a token in
DATABRICKS_TOKEN, or the OAuth login `databricks auth login` cached for the
host, each of them a person's. So the environment keeps the principal's
OAuth machine-to-machine login alone: every other DATABRICKS_* variable is
dropped, the auth type is pinned to it, and the config file is empty.
"""

import os

LOGIN_VARS = ("DATABRICKS_HOST", "DATABRICKS_CLIENT_ID", "DATABRICKS_CLIENT_SECRET")
CLI_VAR_PREFIX = "DATABRICKS_"


class MissingLogin(Exception):
    def __init__(self, missing):
        super().__init__(f"the service principal's login needs {', '.join(missing)}")


def environment(environ):
    """The CLI environment of the service principal `environ` names."""
    missing = [name for name in LOGIN_VARS if not environ.get(name, "").strip()]
    if missing:
        raise MissingLogin(missing)
    env = {name: value for name, value in environ.items() if not name.startswith(CLI_VAR_PREFIX)}
    env.update({name: environ[name].strip() for name in LOGIN_VARS})
    env["DATABRICKS_AUTH_TYPE"] = "oauth-m2m"
    env["DATABRICKS_CONFIG_FILE"] = os.devnull
    return env
