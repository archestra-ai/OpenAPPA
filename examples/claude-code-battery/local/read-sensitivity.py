import json
import sys

# The runtime names the call by its canonical tool id: a Claude Code
# built-in is `host/claude-code/<name>`.
READ_TOOL = "host/claude-code/Read"


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    if request.get("name") != "local.read-sensitivity":
        raise ValueError("unexpected annotator name")

    artifact = request.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    if not isinstance(args, dict) or args.get("name") != READ_TOOL:
        raise ValueError(f"args.name must be {READ_TOOL}")
    arguments = args.get("arguments")
    file_path = arguments.get("file_path") if isinstance(arguments, dict) else None
    if not isinstance(file_path, str) or not file_path:
        raise ValueError("args.arguments.file_path must be a non-empty string")

    sensitive = file_path != ".env.example" and (
        file_path.startswith(".") or file_path.startswith("clients/")
    )
    annotation = {
        "delta": {"trust": "suspicious"},
        "requires": {"history": [], "attention": ["hitl"] if sensitive else []},
        "emits": [],
    }
    json.dump({"version": 1, "answer": annotation}, sys.stdout)
    sys.stdout.write("\n")


try:
    main()
except Exception as error:
    print(f"local-read-sensitivity: {error}", file=sys.stderr)
    raise SystemExit(1)
