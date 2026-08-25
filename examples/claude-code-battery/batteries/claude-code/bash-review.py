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
    if not isinstance(args, dict) or args.get("name") != "Bash":
        raise ValueError("args.name must be Bash")
    arguments = args.get("arguments")
    if not isinstance(arguments, dict) or not isinstance(arguments.get("command"), str):
        raise ValueError("args.arguments.command must be a string")

    attention = [] if arguments["command"] in NO_REVIEW_COMMANDS else ["hitl"]
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
