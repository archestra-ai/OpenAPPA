#!/usr/bin/env python3
"""Fixed runtime operations for appa-guide on kagent."""

from __future__ import annotations

import fcntl
import json
import os
import re
import ssl
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

import tomllib

RUNTIME_URL = os.environ.get("APPA_GUIDE_RUNTIME_URL", "http://127.0.0.1:8787").rstrip(
    "/"
)
REFRESH = "/usr/local/bin/appa-refresh-batteries"
IDENTITY_DIR = Path("/var/run/appa/identity")
POLICY_CONFIGMAP = os.environ.get("APPA_POLICY_CONFIGMAP_NAME")
POLICY_CONFIGMAP_KEY = os.environ.get("APPA_POLICY_CONFIGMAP_KEY", "appa.toml")
RUNTIME_RELEASE = os.environ.get("APPA_RUNTIME_RELEASE_NAME")
BATTERY_NAME = re.compile(r"^[a-z0-9][a-z0-9-]*$")
MANAGEMENT_LOCK = Path(
    os.environ.get(
        "APPA_GUIDE_MANAGEMENT_LOCK", "/var/lib/appa/.appa-guide-management.lock"
    )
)
PERSISTENCE_ENABLED = (
    os.environ.get("APPA_PERSISTENCE_ENABLED", "false").strip().lower() == "true"
)
RELOAD_ERRORS = (
    OSError,
    TimeoutError,
    HTTPError,
    URLError,
    RuntimeError,
    TypeError,
    ValueError,
    json.JSONDecodeError,
)


def runtime_request(path: str, method: str = "GET") -> bytes:
    request = Request(
        RUNTIME_URL + path, data=b"" if method == "POST" else None, method=method
    )
    with urlopen(request, timeout=30) as response:
        return response.read()


def management_request() -> dict:
    request = json.load(sys.stdin)
    if not isinstance(request, dict):
        raise TypeError("management request must be an object")
    return request


@contextmanager
def management_lock():
    MANAGEMENT_LOCK.parent.mkdir(parents=True, exist_ok=True)
    with MANAGEMENT_LOCK.open("a+", encoding="utf-8") as locked:
        fcntl.flock(locked.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(locked.fileno(), fcntl.LOCK_UN)


def mutate(operation, request: dict) -> dict:
    with management_lock():
        return operation(request)


def policy_key() -> str:
    return runtime_request("/policy-key").decode("utf-8").strip()


def expect_policy_key(request: dict) -> None:
    expected = request.get("expected_policy_key")
    if not isinstance(expected, str) or not expected:
        raise ValueError("expected_policy_key is required")
    current = policy_key()
    if current != expected:
        raise RuntimeError(
            f"serving policy changed: expected {expected}, found {current}"
        )


def battery_dirs() -> list[Path]:
    return [
        Path(item)
        for item in os.environ.get("APPA_BATTERIES_DIR", "").split(":")
        if item
    ]


def runtime_state() -> dict:
    config = Path(os.environ["APPA_CONFIG"])
    root = config.read_text(encoding="utf-8")
    parsed = tomllib.loads(root)
    return {
        "policy": root,
        "policy_key": policy_key(),
        "included_batteries": parsed.get("include", []),
        "battery_refresh": refresh_state(),
        "policy_configmap": {
            "name": POLICY_CONFIGMAP,
            "namespace": optional_text(IDENTITY_DIR / "namespace"),
            "key": POLICY_CONFIGMAP_KEY,
            "release": RUNTIME_RELEASE,
        },
    }


def kube_request(path: str, method: str = "GET", body: dict | None = None) -> dict:
    host = os.environ.get("KUBERNETES_SERVICE_HOST")
    port = os.environ.get("KUBERNETES_SERVICE_PORT_HTTPS", "443")
    if not host:
        raise RuntimeError("Kubernetes API is not configured")
    service_account = Path("/var/run/secrets/kubernetes.io/serviceaccount")
    token = (service_account / "token").read_text(encoding="utf-8").strip()
    context = ssl.create_default_context(cafile=str(service_account / "ca.crt"))
    data = json.dumps(body).encode() if body is not None else None
    request = Request(
        f"https://{host}:{port}{path}",
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/json",
            "Content-Type": "application/merge-patch+json",
        },
    )
    try:
        with urlopen(request, timeout=30, context=context) as response:
            return json.load(response)
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")[-2000:]
        raise RuntimeError(f"Kubernetes API returned {error.code}: {detail}") from error


def configmap_path() -> str:
    namespace = optional_text(IDENTITY_DIR / "namespace")
    if not namespace or not POLICY_CONFIGMAP:
        raise RuntimeError("runtime policy ConfigMap identity is not configured")
    return f"/api/v1/namespaces/{namespace}/configmaps/{POLICY_CONFIGMAP}"


def with_battery_include(root: str, battery: str) -> tuple[str, bool]:
    include_path = f"batteries/{battery}/appa.toml"
    parsed = tomllib.loads(root)
    includes = parsed.get("include", [])
    if not isinstance(includes, list) or not all(
        isinstance(item, str) for item in includes
    ):
        raise ValueError("root include must be a string array")
    if include_path in includes:
        return root, False
    updated = [*includes, include_path]
    replacement = f"include = {json.dumps(updated)}"
    match = re.search(r"(?m)^include\s*=\s*", root)
    if match is None:
        return replacement + "\n\n" + root, True
    start = match.start()
    index = match.end()
    while index < len(root) and root[index].isspace() and root[index] != "\n":
        index += 1
    if index >= len(root) or root[index] != "[":
        raise ValueError("root include is not an array")
    depth = 0
    quoted = False
    escaped = False
    end = index
    while end < len(root):
        character = root[end]
        if quoted:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quoted = False
        elif character == '"':
            quoted = True
        elif character == "[":
            depth += 1
        elif character == "]":
            depth -= 1
            if depth == 0:
                end += 1
                break
        end += 1
    if depth != 0:
        raise ValueError("root include array is unterminated")
    return root[:start] + replacement + root[end:], True


def validate_policy(root: str) -> None:
    with tempfile.NamedTemporaryFile(
        "w", suffix=".toml", encoding="utf-8"
    ) as candidate:
        candidate.write(root)
        candidate.flush()
        command = [
            "/usr/local/bin/appa",
            "describe",
            "--check",
            "--config",
            candidate.name,
        ]
        for directory in battery_dirs():
            command.extend(("--batteries-dir", str(directory)))
        environment = os.environ.copy()
        environment.pop("APPA_BATTERIES_DIR", None)
        checked = subprocess.run(
            command, capture_output=True, check=False, env=environment, timeout=30
        )
    if checked.returncode != 0:
        detail = (checked.stderr or checked.stdout).decode("utf-8", errors="replace")[
            -2000:
        ]
        raise RuntimeError(f"candidate policy is invalid: {detail}")


def patch_policy_configmap(expected: str, replacement: str) -> None:
    path = configmap_path()
    resource = kube_request(path)
    data = resource.get("data")
    if not isinstance(data, dict) or data.get(POLICY_CONFIGMAP_KEY) != expected:
        raise RuntimeError("policy ConfigMap changed or differs from the mounted root")
    version = resource.get("metadata", {}).get("resourceVersion")
    if not isinstance(version, str):
        raise TypeError("policy ConfigMap carries no resourceVersion")
    kube_request(
        path,
        "PATCH",
        {
            "metadata": {"resourceVersion": version},
            "data": {POLICY_CONFIGMAP_KEY: replacement},
        },
    )


def wait_for_policy(value: str, attempts: int = 90) -> bool:
    config = Path(os.environ["APPA_CONFIG"])
    for _ in range(attempts):
        if config.read_text(encoding="utf-8") == value:
            return True
        time.sleep(1)
    return False


def update_policy_configmap(current: str, candidate: str) -> None:
    patch_policy_configmap(current, candidate)
    if wait_for_policy(candidate):
        return
    try:
        patch_policy_configmap(candidate, current)
        if not wait_for_policy(current):
            raise RuntimeError("kubelet did not restore the prior policy")
    except (OSError, TypeError, ValueError, RuntimeError, HTTPError) as rollback_error:
        raise RuntimeError(
            f"kubelet did not publish the policy within 90 seconds; rollback failed: {rollback_error}"
        ) from rollback_error
    raise RuntimeError(
        "kubelet did not publish the policy within 90 seconds; prior policy restored"
    )


def mounted_policy() -> str:
    return Path(os.environ["APPA_CONFIG"]).read_text(encoding="utf-8")


def restore_serving_policy(candidate: str, current: str) -> None:
    update_policy_configmap(candidate, current)
    runtime_request("/reload", "POST")
    if mounted_policy() != current:
        raise RuntimeError("rollback did not restore the mounted policy")


def publish_and_reload(current: str, candidate: str) -> dict:
    update_policy_configmap(current, candidate)
    if mounted_policy() != candidate:
        restore_serving_policy(candidate, current)
        raise RuntimeError(
            "mounted policy changed before reload; prior policy restored"
        )
    try:
        return json.loads(runtime_request("/reload", "POST"))
    except RELOAD_ERRORS as error:
        try:
            restore_serving_policy(candidate, current)
        except RELOAD_ERRORS as rollback_error:
            raise RuntimeError(
                "reload failed and rollback did not restore serving policy: "
                f"{rollback_error}"
            ) from error
        raise


def include_battery(request: dict) -> dict:
    expect_policy_key(request)
    battery = request.get("battery")
    if not isinstance(battery, str) or not BATTERY_NAME.fullmatch(battery):
        raise ValueError("battery must be a lowercase battery name")
    if not any(
        (directory / battery / "appa.toml").is_file() for directory in battery_dirs()
    ):
        raise RuntimeError(f"battery is not available: {battery}")
    current = mounted_policy()
    candidate, changed = with_battery_include(current, battery)
    if not changed:
        return {**runtime_state(), "changed": False, "battery": battery}
    validate_policy(candidate)
    reloaded = publish_and_reload(current, candidate)
    return {**runtime_state(), "changed": True, "battery": battery, "reload": reloaded}


def update_policy(request: dict) -> dict:
    expect_policy_key(request)
    candidate = request.get("policy")
    if not isinstance(candidate, str) or not candidate.strip():
        raise ValueError("policy must be one complete root policy")
    current = mounted_policy()
    current_value = tomllib.loads(current)
    candidate_value = tomllib.loads(candidate)
    current_include = set(current_value.pop("include", []))
    candidate_include = set(candidate_value.pop("include", []))
    if not current_include.issubset(candidate_include) or not preserves_policy_shape(
        current_value, candidate_value
    ):
        raise ValueError("candidate does not retain the current root policy structure")
    validate_policy(candidate)
    reloaded = publish_and_reload(current, candidate)
    return {**runtime_state(), "changed": candidate != current, "reload": reloaded}


def reload_policy(request: dict) -> dict:
    expect_policy_key(request)
    return {**runtime_state(), "reload": json.loads(runtime_request("/reload", "POST"))}


def refresh_batteries(request: dict) -> dict:
    expect_policy_key(request)
    if not refresh_state()["supported"]:
        raise RuntimeError("persistent battery refresh is not supported")
    try:
        refreshed = subprocess.run(
            [REFRESH], check=True, capture_output=True, text=True, timeout=180
        )
        reloaded = json.loads(runtime_request("/reload", "POST"))
        subprocess.run(
            [REFRESH, "--commit"], check=True, capture_output=True, timeout=30
        )
    except (
        OSError,
        RuntimeError,
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        HTTPError,
    ):
        subprocess.run(
            [REFRESH, "--rollback"], check=False, capture_output=True, timeout=30
        )
        try:
            runtime_request("/reload", "POST")
        except (OSError, HTTPError) as rollback_error:
            print(f"battery rollback reload failed: {rollback_error}", file=sys.stderr)
        raise
    return {**runtime_state(), "release": refreshed.stdout.strip(), "reload": reloaded}


def optional_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8").strip() or None
    except FileNotFoundError:
        return None


def refresh_state() -> dict:
    target = Path(
        os.environ.get("APPA_BATTERY_REFRESH_TARGET", "/var/lib/appa/release-batteries")
    )
    previous = target.with_name(f".{target.name}.previous")
    return {
        "supported": PERSISTENCE_ENABLED and Path(REFRESH).is_file(),
        "persistent": PERSISTENCE_ENABLED,
        "release": optional_text(target / ".appa-release"),
        "pending_previous_layer": previous.is_dir() and not previous.is_symlink(),
    }


def annotation_arguments(request: dict, expected_name: str) -> dict:
    if request.get("version") != 1 or request.get("kind") != "annotation":
        raise ValueError("expected an annotation consult at version 1")
    if request.get("name") != expected_name:
        raise ValueError("unexpected annotator name")
    artifact = request.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    if not isinstance(arguments, dict):
        raise TypeError("annotation consult carries no tool arguments")
    return arguments


def preserves_policy_shape(current: object, candidate: object) -> bool:
    if isinstance(current, dict) and isinstance(candidate, dict):
        return all(
            key in candidate and preserves_policy_shape(value, candidate[key])
            for key, value in current.items()
        )
    if isinstance(current, list) and isinstance(candidate, list):
        if all(
            isinstance(item, dict) and isinstance(item.get("name"), str)
            for item in current
        ):
            position = 0
            for item in current:
                while position < len(candidate):
                    other = candidate[position]
                    position += 1
                    if (
                        isinstance(other, dict)
                        and other.get("name") == item["name"]
                        and preserves_policy_shape(item, other)
                    ):
                        break
                else:
                    return False
            return True
        return True
    return type(current) is type(candidate)


def annotate_apply(request: dict) -> dict:
    arguments = annotation_arguments(request, "appa-guide-apply")
    manifest = arguments.get("manifest")
    if not isinstance(manifest, str):
        raise TypeError("apply carries no manifest")
    documents = [
        document.strip()
        for document in re.split(r"(?m)^---\s*$", manifest)
        if document.strip()
    ]
    if len(documents) != 1:
        raise ValueError("apply manifest must be exactly one Agent document")
    document = documents[0]
    kinds = re.findall(r"(?m)^kind:\s*(\S+)\s*$", document)
    if kinds != ["Agent"]:
        raise ValueError("appa-guide Kubernetes apply supports only Agent manifests")
    if re.search(r"(?m)^status:\s*$", document) or not re.search(
        r"(?m)^spec:\s*$", document
    ):
        raise ValueError("Agent apply must carry complete spec and no status")
    return {
        "version": 1,
        "answer": {
            "delta": {},
            "requires": {
                "trust": "trusted",
                "history": [],
                "attention": ["human-approval"],
            },
            "emits": [],
        },
    }


def main() -> int:
    action = Path(sys.argv[0]).name
    try:
        if action == "appa-guide-apply-annotator":
            json.dump(annotate_apply(json.load(sys.stdin)), sys.stdout)
            print()
        elif action == "appa-guide-runtime-state":
            json.dump(runtime_state(), sys.stdout, sort_keys=True)
            print()
        elif action == "appa-guide-include-battery":
            json.dump(
                mutate(include_battery, management_request()),
                sys.stdout,
                sort_keys=True,
            )
            print()
        elif action == "appa-guide-update-policy":
            json.dump(
                mutate(update_policy, management_request()), sys.stdout, sort_keys=True
            )
            print()
        elif action == "appa-guide-reload-policy":
            json.dump(
                mutate(reload_policy, management_request()), sys.stdout, sort_keys=True
            )
            print()
        elif action == "appa-guide-refresh-batteries":
            json.dump(
                mutate(refresh_batteries, management_request()),
                sys.stdout,
                sort_keys=True,
            )
            print()
        else:
            raise RuntimeError(f"unsupported appa-guide runtime operation: {action}")
    except (
        OSError,
        TypeError,
        ValueError,
        RuntimeError,
        subprocess.CalledProcessError,
        json.JSONDecodeError,
    ) as error:
        print(f"{action}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
