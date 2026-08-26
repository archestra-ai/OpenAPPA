import json
import sys


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "dynamic":
        raise ValueError("unexpected consult kind")
    if request.get("name") != "local.read-sensitivity":
        raise ValueError("unexpected resolver name")

    artifact = request.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    if not isinstance(args, dict) or args.get("name") != "Read":
        raise ValueError("args.name must be Read")
    arguments = args.get("arguments")
    file_path = arguments.get("file_path") if isinstance(arguments, dict) else None
    if not isinstance(file_path, str) or not file_path:
        raise ValueError("args.arguments.file_path must be a non-empty string")

    sensitive = file_path != ".env.example" and (
        file_path.startswith(".") or file_path.startswith("clients/")
    )
    audience = ["private"] if sensitive else "public"
    json.dump(
        {"version": 1, "answer": {"delta.audience": audience}},
        sys.stdout,
    )
    sys.stdout.write("\n")


try:
    main()
except Exception as error:
    print(f"local-read-sensitivity: {error}", file=sys.stderr)
    raise SystemExit(1)
