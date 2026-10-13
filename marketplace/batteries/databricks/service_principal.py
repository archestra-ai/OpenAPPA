"""The Databricks CLI environment of a multi-user deployment.

A multi-user deployment acts as one service principal, never as the person
who installed the CLI: the CLI otherwise falls back to the default profile in
~/.databrickscfg, the profile DATABRICKS_CONFIG_PROFILE names, a token in
DATABRICKS_TOKEN, or the OAuth login `databricks auth login` cached for the
host, each of them a person's. So the environment keeps the principal's
OAuth machine-to-machine login alone: every other DATABRICKS_* variable is
dropped, the auth type is pinned to it, and the config file is empty.
The login comes from the APPA_PROVIDER_DATABRICKS_* credentials the
binding forwards, each handed to the CLI under its own DATABRICKS_* name.
"""

import os

CLI_VAR_PREFIX = "DATABRICKS_"
CREDENTIAL_PREFIX = "APPA_PROVIDER_"
LOGIN_VARS = {
    f"{CREDENTIAL_PREFIX}{CLI_VAR_PREFIX}{name}": f"{CLI_VAR_PREFIX}{name}"
    for name in ("HOST", "CLIENT_ID", "CLIENT_SECRET")
}


class MissingLogin(Exception):
    def __init__(self, missing):
        super().__init__(f"the service principal's login needs {', '.join(missing)}")


def environment(environ):
    """The CLI environment of the service principal `environ` names."""
    missing = [name for name in LOGIN_VARS if not environ.get(name, "").strip()]
    if missing:
        raise MissingLogin(missing)
    env = {
        name: value
        for name, value in environ.items()
        if not name.startswith(CLI_VAR_PREFIX) and name not in LOGIN_VARS
    }
    env.update({cli: environ[name].strip() for name, cli in LOGIN_VARS.items()})
    env["DATABRICKS_AUTH_TYPE"] = "oauth-m2m"
    env["DATABRICKS_CONFIG_FILE"] = os.devnull
    return env
