"""Read-only login check. Never prints credentials or provider response bodies."""
import json
import os
import subprocess
import urllib.error
import urllib.request

from github_token import api_root, api_hostname, TOKEN_VAR


def check(environ=None, open_url=urllib.request.urlopen, run=subprocess.run):
    environ = os.environ if environ is None else environ
    authentication = "token" if environ.get(TOKEN_VAR, "").strip() else "cli"
    def result(status, reason):
        return {"status": status, "authentication": authentication, "reason": reason}
    if authentication == "cli":
        # Check the account the helpers will actually use, without extracting or
        # exposing its token. gh performs its own authentication verification.
        try:
            completed = run(
                ["gh", "auth", "status", "--active", "--hostname", api_hostname(environ)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=8,
                env=environ,
                check=False,
            )
        except FileNotFoundError:
            return result("needs_configuration", "missing_executable")
        except subprocess.TimeoutExpired:
            return result("unavailable", "check_timed_out")
        except OSError:
            return result("unavailable", "check_failed")
        if completed.returncode == 0:
            return result("ready", "verified")
        return result("needs_configuration", "cli_not_authenticated")
    token = environ[TOKEN_VAR].strip()
    try:
        # Viewer and email access are used by this battery. Repository-specific
        # collaborator permissions cannot be verified without a repository.
        for path, expected in [("/user", dict), ("/user/emails", list)]:
            request = urllib.request.Request(
                api_root(environ) + path,
                headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json"},
            )
            with open_url(request, timeout=4) as response:
                body = json.load(response)
                if not isinstance(body, expected) or (path == "/user" and not body.get("login")):
                    return result("unavailable", "check_failed")
    except urllib.error.HTTPError as error:
        if error.code == 401:
            return result("needs_configuration", "invalid_credential")
        if error.code == 403:
            return result("needs_configuration", "insufficient_access")
        return result("unavailable", "provider_unavailable")
    except (OSError, ValueError):
        return result("unavailable", "provider_unavailable")
    return result("ready", "verified")


if __name__ == "__main__":
    print(json.dumps(check()))
