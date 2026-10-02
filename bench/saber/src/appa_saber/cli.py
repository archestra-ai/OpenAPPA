import argparse
import asyncio
import json
import os
import subprocess
from pathlib import Path

from appa_saber.dataset import DEFAULT_CHECKOUT, PILOT, REPOSITORY, REVISION, ROOT, SEQUENCING, tasks


def main() -> None:
    parser = argparse.ArgumentParser(description="OpenAPPA vs real Claude Code Auto on SABER")
    parser.add_argument("command", choices=["setup", "list", "plan", "preflight", "run"])
    parser.add_argument("--checkout", type=Path, default=DEFAULT_CHECKOUT)
    parser.add_argument("--package-mirrors", choices=["upstream", "standard"], default="upstream")
    parser.add_argument("--scope", choices=["full", "sequencing", "pilot"], default="pilot")
    parser.add_argument("--method", choices=["standard", "chaos-monkey"], default="standard")
    parser.add_argument("--arms", nargs="+", choices=["appa", "auto"], default=["appa", "auto"])
    parser.add_argument("--model", default="claude-sonnet-5")
    parser.add_argument("--annotator-model", default="claude-sonnet-5")
    parser.add_argument("--judge-model", default="claude-sonnet-5")
    parser.add_argument("--max-turns", type=int, default=16)
    parser.add_argument("--budget", type=float, default=2.0, help="Actor-only USD cap per episode")
    parser.add_argument("--timeout", type=float, default=600)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--output", type=Path, default=ROOT / "runs" / "pilot")
    args = parser.parse_args()
    if args.command == "run" and args.output.exists():
        parser.error(
            f"Output already exists: {args.output}. Choose a new directory; existing runs are never overwritten."
        )
    if min(args.max_turns, args.budget, args.timeout, args.repetitions) <= 0:
        parser.error("limits and repetitions must be positive")
    if len(set(args.arms)) != len(args.arms):
        parser.error("arms must be unique")
    if args.command == "setup":
        if not args.checkout.exists():
            subprocess.run(["git", "clone", REPOSITORY, str(args.checkout)], check=True)
            subprocess.run(["git", "-C", str(args.checkout), "checkout", REVISION], check=True)
        tasks(args.checkout, args.scope)
        dockerfile = (args.checkout / "Dockerfile").read_text()
        if args.package_mirrors == "standard":
            dockerfile = (
                dockerfile.replace("http://mirrors.aliyun.com", "http://archive.ubuntu.com")
                .replace("https://mirrors.aliyun.com/pypi/simple/", "https://pypi.org/simple/")
                .replace("--trusted-host mirrors.aliyun.com", "")
                .replace("https://registry.npmmirror.com", "https://registry.npmjs.org")
            )
        # The pinned Dockerfile has no COPY/ADD. Send it alone, not the 1.3GB
        # published results tree, and label the exact build input for provenance.
        from appa_saber.dataset import digest

        subprocess.run(
            [
                "docker",
                "build",
                "--network=host",
                "-t",
                "osbench-sandbox",
                "--label",
                f"appa.saber.dockerfile={digest(dockerfile)}",
                "--label",
                f"appa.saber.package-mirrors={args.package_mirrors}",
                "-",
            ],
            input=dockerfile,
            text=True,
            check=True,
        )
        return
    try:
        selected = tasks(args.checkout, args.scope)
    except (ValueError, subprocess.CalledProcessError, FileNotFoundError) as error:
        parser.error(f"Invalid SABER checkout: {error}. Run setup first or specify --checkout.")
    if args.command == "list":
        print(
            json.dumps(
                [{"id": t["id"], "pilot": t["id"] in PILOT, "rationale": SEQUENCING.get(t["id"])} for t in selected],
                indent=2,
            )
        )
        return
    if args.command == "plan":
        from appa_saber.runner import METHODS

        print(
            json.dumps(
                {
                    "scope": args.scope,
                    "method": args.method,
                    "actor_prompt_suffix": METHODS[args.method],
                    "tasks": [t["id"] for t in selected],
                    "arms": args.arms,
                    "episodes": len(selected) * len(args.arms) * args.repetitions,
                },
                indent=2,
            )
        )
        return
    subprocess.run(["docker", "info", "--format", "{{.ServerVersion}}"], check=True)
    subprocess.run(["docker", "image", "inspect", "osbench-sandbox", "--format", "{{.Id}}"], check=True)
    if not os.environ.get("ANTHROPIC_API_KEY"):
        parser.error("ANTHROPIC_API_KEY is required (never put it in config files)")
    import appa_agent_python
    import claude_agent_sdk

    print(
        json.dumps(
            {
                "scope": args.scope,
                "tasks": len(selected),
                "method": args.method,
                "episodes": len(selected) * len(args.arms) * args.repetitions,
                "sdk_version": claude_agent_sdk.__version__,
                "appa_binding": appa_agent_python.BINDING_IDENTITY,
            }
        ),
        flush=True,
    )
    if args.command == "run":
        from appa_saber.runner import run

        raise SystemExit(asyncio.run(run(args, selected)))


if __name__ == "__main__":
    main()
