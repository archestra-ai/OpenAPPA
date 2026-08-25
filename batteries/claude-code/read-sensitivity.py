import json
import sys
from pathlib import PurePath


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("resolver") != "claude-code.read-sensitivity":
        raise ValueError("unexpected resolver name")

    args = request.get("args")
    if not isinstance(args, dict) or args.get("name") != "Read":
        raise ValueError("args.name must be Read")
    arguments = args.get("arguments")
    file_path = arguments.get("file_path") if isinstance(arguments, dict) else None
    if not isinstance(file_path, str) or not file_path:
        raise ValueError("args.arguments.file_path must be a non-empty string")

    # Claude Code sends an absolute path, so test the file name, not the path.
    audience = ["private"] if PurePath(file_path).name.startswith(".") else "public"
    json.dump(
        {"version": 1, "result": {"delta.audience": audience}},
        sys.stdout,
    )
    sys.stdout.write("\n")


try:
    main()
except Exception as error:
    print(f"read-sensitivity: {error}", file=sys.stderr)
    raise SystemExit(1)
