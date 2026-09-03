import json
import sys
from pathlib import Path


PRIVATE_FILENAMES = frozenset(
    {
        "auth.json",
        "credentials",
        "credentials.json",
        "gshadow",
        "passwd",
        "secrets",
        "secrets.json",
        "secrets.yaml",
        "secrets.yml",
        "service-account.json",
        "service_account.json",
        "shadow",
        "token",
        "token.json",
    }
)
PRIVATE_KEY_FILENAMES = frozenset({"id_dsa", "id_ecdsa", "id_ed25519", "id_rsa"})
PRIVATE_KEY_SUFFIXES = frozenset({".jks", ".key", ".keystore", ".p12", ".pem", ".pfx"})
PRIVATE_SYSTEM_PATHS = (
    Path("/etc/gshadow"),
    Path("/etc/shadow"),
    Path("/etc/ssh"),
    Path("/etc/sudoers"),
    Path("/etc/sudoers.d"),
    Path("/run/secrets"),
    Path("/var/run/secrets"),
)


def is_within(path, parent):
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def has_private_name(path):
    parts = path.parts[1:] if path.is_absolute() else path.parts
    if any(part.startswith(".") and part not in {".", ".."} for part in parts):
        return True

    name = path.name.casefold()
    return (
        name in PRIVATE_FILENAMES
        or name in PRIVATE_KEY_FILENAMES
        or path.suffix.casefold() in PRIVATE_KEY_SUFFIXES
    )


def is_private_system_path(path):
    if any(is_within(path, parent) for parent in PRIVATE_SYSTEM_PATHS):
        return True

    parts = tuple(part.casefold() for part in path.parts)
    process_data = len(parts) >= 3 and parts[1] == "proc" and parts[-1] in {"cmdline", "environ"}
    keychain = any(
        parts[index : index + 2] == ("library", "keychains")
        for index in range(len(parts) - 1)
    )
    return process_data or keychain


def is_sensitive(file_path):
    path = Path(file_path)
    try:
        resolved = path.resolve(strict=False)
    except (OSError, RuntimeError):
        return True

    for candidate in {path, resolved}:
        if has_private_name(candidate) or is_private_system_path(candidate):
            return True
    return False


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    if request.get("name") != "claude-code.read-sensitivity":
        raise ValueError("unexpected resolver name")

    artifact = request.get("artifact")
    if not isinstance(artifact, dict) or artifact.get("tool") != "Read":
        raise ValueError("artifact.tool must be Read")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    file_path = args.get("file_path") if isinstance(args, dict) else None
    if not isinstance(file_path, str) or not file_path:
        raise ValueError("artifact.args.file_path must be a non-empty string")

    audience = ["private"] if is_sensitive(file_path) else "public"
    annotation = {
        "delta": {"audience": audience},
        "requires": {"history": [], "attention": []},
        "emits": [],
    }
    json.dump({"version": 1, "answer": annotation}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"read-sensitivity: {error}", file=sys.stderr)
        raise SystemExit(1)
