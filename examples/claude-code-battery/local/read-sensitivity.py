import json
import sys


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    if request.get("name") != "local.read-sensitivity":
        raise ValueError("unexpected annotator name")

    artifact = request.get("artifact")
    if not isinstance(artifact, dict) or artifact.get("tool") != "Read":
        raise ValueError("artifact.tool must be Read")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    file_path = args.get("file_path") if isinstance(args, dict) else None
    if not isinstance(file_path, str) or not file_path:
        raise ValueError("artifact.args.file_path must be a non-empty string")

    sensitive = file_path != ".env.example" and (
        file_path.startswith(".") or file_path.startswith("clients/")
    )
    audience = ["private"] if sensitive else "public"
    annotation = {
        "delta": {"trust": "suspicious", "audience": audience},
        "requires": {"history": [], "attention": []},
        "emits": [],
    }
    json.dump({"version": 1, "answer": annotation}, sys.stdout)
    sys.stdout.write("\n")


try:
    main()
except Exception as error:
    print(f"local-read-sensitivity: {error}", file=sys.stderr)
    raise SystemExit(1)
