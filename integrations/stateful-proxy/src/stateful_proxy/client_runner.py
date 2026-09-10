"""Run synthetic client probes without exposing provider credentials to tools."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time

FIXTURE_MCP_NAME = "appa_fixture"
PROMPT = """This is a bounded correlation experiment in a disposable directory.
Do not inspect environment variables, credentials, home directories, or network.
Read alpha.txt and beta.txt using two independent tool calls in parallel if supported.
Then spawn one child subagent to read alpha.txt and return its value; await its result.
Write combined.txt containing both values using a local write/edit tool.
Run only the harmless shell command pwd. Return a brief summary of completed actions.
If subagents are unavailable, report that rather than emulating them with a shell.
"""


def positive_int(value: str) -> int:
    try:
        result = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a positive integer") from error
    if result <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("client", choices=["claude", "codex", "opencode"])
    parser.add_argument("--mode", choices=["native", "rewrite", "inject"], default="native")
    parser.add_argument("--resume")
    parser.add_argument("--fork", action="store_true")
    parser.add_argument("--prompt", default=PROMPT)
    parser.add_argument("--label", default="tools")
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--proxy-url", default="http://127.0.0.1:18765", help="loopback relay base URL")
    parser.add_argument("--work-root", type=Path, required=True, help="private run directory outside this checkout")
    parser.add_argument("--client-executable", help="client executable path; default resolves the selected client on PATH")
    parser.add_argument("--provider-timeout-ms", type=positive_int)
    parser.add_argument("--compact-threshold", type=int)
    parser.add_argument("--codex-read-only", action="store_true")
    parser.add_argument("--codex-legacy-landlock", action="store_true")
    parser.add_argument("--codex-v1-profile", action="store_true",
                        help="use the tested V1 feature profile (disable multi_agent_v2)")
    parser.add_argument("--fixture-mcp", action="store_true")
    parser.add_argument("--fixture-command", default="appa-stateful-fixtures", help="installed fixture-tools entry point")
    parser.add_argument("--fixture-output-dir", type=Path, help="private fixture output directory")
    parser.add_argument("--opencode-title", help="explicit OpenCode session title; avoids automatic title generation")
    parser.add_argument("--allow-builtin-commands", action="store_true")
    args = parser.parse_args()
    if args.mode == "inject" and args.client != "claude":
        parser.error("The child-prompt injection experiment supports Claude only")
    os.umask(0o077)
    executable = args.client_executable or shutil.which(args.client)
    if executable is None or not os.access(executable, os.X_OK):
        parser.error(f"required client executable is unavailable: {args.client_executable or args.client}")
    fixture_command = None
    if args.fixture_mcp:
        fixture_command = shutil.which(args.fixture_command)
        if fixture_command is None or not os.access(fixture_command, os.X_OK):
            parser.error(f"installed fixture entry point is unavailable: {args.fixture_command}")
    args.work_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    args.work_root.chmod(0o700)
    run = args.work_root / f"{args.client}-{args.mode}"
    work = run / "work"
    home = run / "home"
    work.mkdir(parents=True, exist_ok=True)
    home.mkdir(mode=0o700, exist_ok=True)
    home.chmod(0o700)
    (home / ".codex").mkdir(mode=0o700, exist_ok=True)
    for name, content in [("alpha.txt", "ALPHA-17\n"), ("beta.txt", "BETA-29\n")]:
        (work / name).write_text(content)
    env = {k: os.environ[k] for k in ["LANG", "NODE_USE_SYSTEM_CA"] if k in os.environ}
    env["PATH"] = os.environ.get("PATH", "")
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"),
               XDG_DATA_HOME=str(home / ".local/share"), XDG_CACHE_HOME=str(home / ".cache"),
               TERM="dumb")
    base = args.proxy_url.rstrip("/") + "/" + (args.mode + "/" if args.mode != "native" else "")
    fixture_args = ["--output-dir", str(args.fixture_output_dir or (run / "fixture-output"))]
    if args.client == "claude":
        env.update(ANTHROPIC_API_KEY="local-proxy-placeholder", ANTHROPIC_BASE_URL=base + "anthropic",
                   CLAUDE_CONFIG_DIR=str(home / ".claude"), CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1")
        if args.provider_timeout_ms is not None:
            env["API_TIMEOUT_MS"] = str(args.provider_timeout_ms)
        mcp_config = '{"mcpServers":{}}'
        allowed_tools = "Read,Write,Edit,Agent,Bash(pwd)"
        cmd = [str(executable)]
        if args.fixture_mcp:
            # --safe-mode disables explicit MCP configuration, so retain isolation explicitly instead.
            mcp_config = json.dumps({"mcpServers": {FIXTURE_MCP_NAME: {
                "command": fixture_command, "args": fixture_args}}})
            allowed_tools += ",mcp__appa_fixture__*"
            if not args.allow_builtin_commands:
                cmd += ["--disable-slash-commands"]
        else:
            cmd += ["--safe-mode"]
        cmd += ["--strict-mcp-config", "--mcp-config", mcp_config,
               "--setting-sources", "", "--model", "claude-haiku-4-5",
               "--max-budget-usd", "2", "--tools", "Read,Write,Edit,Bash,Agent",
               "--allowedTools", allowed_tools,
               "--permission-mode", "dontAsk", "--output-format", "stream-json", "--verbose"]
        if args.resume:
            cmd += ["--resume", args.resume]
        if args.fork:
            cmd += ["--fork-session"]
        cmd += ["-p", args.prompt]
    elif args.client == "codex":
        env.update(OPENAI_API_KEY="local-proxy-placeholder", CODEX_HOME=str(home / ".codex"))
        cmd = [str(executable), "exec", "--ignore-user-config", "--ignore-rules", "--skip-git-repo-check",
               "--json", "--sandbox", "read-only" if args.codex_read_only else "workspace-write", "-m", "gpt-5.4",
               "-c", 'model_provider="probe"', "-c", 'model_providers.probe.name="Probe"',
               "-c", f'model_providers.probe.base_url="{base}openai/v1"',
               "-c", 'model_providers.probe.env_key="OPENAI_API_KEY"',
                "-c", 'model_providers.probe.wire_api="responses"',
                "-c", 'model_providers.probe.supports_websockets=false',
                "-c", 'model_reasoning_effort="low"', "-c", 'features.multi_agent=true']
        if args.codex_legacy_landlock:
            cmd += ["-c", "features.use_legacy_landlock=true"]
        if args.codex_v1_profile:
            cmd += ["-c", "features.multi_agent_v2=false"]
        if args.fixture_mcp:
            cmd += ["-c", f'mcp_servers.{FIXTURE_MCP_NAME}.command="{fixture_command}"',
                    "-c", f"mcp_servers.{FIXTURE_MCP_NAME}.args={json.dumps(fixture_args)}"]
            # Codex defaults MCP tools to prompt; approve only the known fixture tool names.
            cmd += ["-c", f'mcp_servers.{FIXTURE_MCP_NAME}.tools.read_source.approval_mode="approve"',
                    "-c", f'mcp_servers.{FIXTURE_MCP_NAME}.tools.publish.approval_mode="approve"',
                    "-c", f'mcp_servers.{FIXTURE_MCP_NAME}.tools.protected_publish.approval_mode="approve"']
        if args.compact_threshold is not None:
            cmd += ["-c", f"model_auto_compact_token_limit={args.compact_threshold}"]
        if args.resume:
            cmd += ["fork" if args.fork else "resume", args.resume]
        cmd += [args.prompt]
    else:
        config = {"$schema": "https://opencode.ai/config.json", "share": "disabled",
                  "enabled_providers": ["kimi-probe"], "model": "kimi-probe/kimi-for-coding",
                  "small_model": "kimi-probe/kimi-for-coding", "permission": {
                      "*": "deny", "read": "allow", "edit": "allow", "task": "allow",
                      "bash": {"*": "deny", "pwd": "allow"}},
                  "provider": {"kimi-probe": {"npm": "@ai-sdk/openai-compatible", "name": "Kimi Probe",
                      "models": {"kimi-for-coding": {"name": "Kimi for Coding", "limit": {"context": 262144, "output": 16384}}},
                      "options": {"baseURL": base + "kimi/v1", "apiKey": "local-proxy-placeholder"}}}}
        if args.fixture_mcp:
            config["mcp"] = {FIXTURE_MCP_NAME: {"type": "local",
                "command": [fixture_command, *fixture_args], "enabled": True}}
            config["permission"].update({
                f"{FIXTURE_MCP_NAME}_*": "deny",
                f"{FIXTURE_MCP_NAME}_read_source": "allow",
                f"{FIXTURE_MCP_NAME}_publish": "allow",
                f"{FIXTURE_MCP_NAME}_protected_publish": "allow",
            })
        cfg = run / "opencode.json"
        cfg.write_text(json.dumps(config))
        env.update(OPENCODE_CONFIG=str(cfg), OPENCODE_DISABLE_DEFAULT_PLUGINS="true",
                   OPENCODE_DISABLE_CLAUDE_CODE="true", OPENCODE_DISABLE_SHARE="true")
        cmd = [str(executable), "run", "--pure", "--format", "json", "--model", "kimi-probe/kimi-for-coding"]
        if args.opencode_title:
            cmd += ["--title", args.opencode_title]
        if args.resume:
            cmd += ["--session", args.resume]
        if args.fork:
            cmd += ["--fork"]
        cmd += [args.prompt]
    started = time.time()
    with (run / f"{args.label}.stdout.jsonl").open("w") as out, (run / f"{args.label}.stderr.log").open("w") as err:
        process = subprocess.Popen(cmd, cwd=work, env=env, stdout=out, stderr=err, start_new_session=True)
        try:
            status = process.wait(timeout=args.timeout)
        except subprocess.TimeoutExpired:
            # Kill the complete client process group, including helper children.
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
            status = "timeout"
    record = {"client": args.client, "mode": args.mode, "label": args.label, "command": cmd,
              "exit": status, "elapsed_s": round(time.time() - started, 2), "started": started,
              "work": str(work), "credentials": "dummy client key; real key only in proxy"}
    (run / f"{args.label}.run.json").write_text(json.dumps(record, indent=2))
    print(json.dumps(record))
    return 124 if status == "timeout" else status


if __name__ == "__main__":
    raise SystemExit(main())
