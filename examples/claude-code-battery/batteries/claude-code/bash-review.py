import json
import sys


NO_REVIEW_COMMANDS = {
    "cargo check",
    "cargo fmt --check",
    "git diff --check",
    "git status --short",
}


def main():
    request = json.load(sys.stdin)

    if request.get("version") != 1:
        raise ValueError("unsupported request version")
    if request.get("resolver") != "claude-code.bash-review":
        raise ValueError("unexpected resolver name")

    args = request.get("args")
    if not isinstance(args, dict) or not isinstance(args.get("command"), str):
        raise ValueError("args.command must be a string")

    attention = [] if args["command"] in NO_REVIEW_COMMANDS else ["hitl"]
    json.dump(
        {"version": 1, "result": {"requires.attention": attention}},
        sys.stdout,
    )
    sys.stdout.write("\n")


try:
    main()
except Exception as error:
    print(f"bash-review: {error}", file=sys.stderr)
    raise SystemExit(1)
