---
title: Coding agents
category: Deep Dive
order: 4
description: Tool-flow enforcement, file labels, SafeBash, shell rules, and checked delegation for coding agents.
---

OpenAPPA monitors and restricts coding agent tool calls and subagent execution via harness hooks and an in-process MCP server. For [Claude Code](/claude-code), it enforces three core boundaries: protected sessions, validated subagent returns, and policy contracts for native filesystem and shell tools.

> **Scope Note:** OpenAPPA tracks information flow and validates tool invocations; it does not isolate processes. Pair it with operating-system sandboxing (such as containers or namespaces) for execution confinement.

## Protected sessions

Running `appa plugin install claude-code` installs lifecycle hooks, the `appa` MCP server, the `appa-guide` skill, and the `clappa` launcher into your Claude Code setup.

### Activation and environment invariants

Enforcement hooks evaluate calls only when `APPA_GATE=1` is set in the process environment:

- **Unprotected (`claude`):** Standard execution runs without policy checks, flow labeling, or tool call blocking. The `appa` MCP tools remain exposed to the model.
- **Protected (`clappa`):** The supported launcher automatically sets `APPA_GATE=1`.
- **Custom Launchers:** When invoking Claude Code directly, export `APPA_GATE=1` and set `CLAUDE_CODE_DISABLE_AGENT_VIEW=1` (backgrounded sessions are unsupported).

`clappa` also manages state tokens required for session integrity:
- **Launch token:** Binds session state. Without it, each new session ID initializes with an unlinked, fresh security label.
- **Parent lineage:** Binds forked sessions to the parent trajectory's taint context. Without `clappa`, a forked session inherits parent conversation history but resets to a clean label (avoid forking unprotected sessions).
- **Messaging address:** Required for inter-session peer communication.
- **Confined shell:** Without it, Claude Code's own `Bash` declares no file lists, and the file-tracker refuses every `Bash` call. Register SafeBash instead: an MCP server named `file-tracker` that runs `appa file-tracker mcp`. Also list `Bash`, `PowerShell`, `Monitor`, `Grep`, and `NotebookEdit` in `permissions.deny`.

### Boundary limitations and failure modes

- **Client-Side Enforcement:** Because hooks execute in userspace inside the Claude Code client process, local configuration files can disable protection via `APPA_GATE` or `disableAllHooks`.
- **Fail-Closed Behavior:** If the OpenAPPA runtime disconnects or becomes unavailable, all hooked actions block by default.
- **Telemetry:** The terminal status line displays the active trajectory's trust level and audience scope. See the [Claude Code guide](/claude-code) for setup and policy enforcement walk-throughs.

## Subagent return checks

When a parent agent delegates through Claude Code's native `Agent` tool, OpenAPPA intercepts execution at two points:

1. **Frontmatter Validation:** Claude Code bypasses return hooks if a subagent definition declares `maxTurns`. To prevent hook evasion, OpenAPPA scans the first 64 KiB of user, project, and plugin frontmatter. If `maxTurns` is present, or if frontmatter is unclosed or unparseable, OpenAPPA rejects the prompt (fail-closed).
2. **Contract Declaration:** OpenAPPA suspends subagent spawning until the parent agent registers a return contract. The parent registers this contract immediately before the `Agent` call using `execute_remedy_plan` (omitting `offer_id`). The contract can:
   - Accept the child's final response unmodified.
   - Constrain the response to a specified security label.
   - Route the output through a designated sanitizer.

Human review is required only if the spawn request itself violates policy or requires explicit authority approval.

During execution, OpenAPPA monitors child tool calls under standard policy. Once the child finishes, OpenAPPA intercepts and validates the final response payload before releasing it to the parent context. If validation fails, the child remains active until it produces an admissible response. This mechanism allows subagents to inspect sensitive files (e.g. credential logs or private tickets) without leaking prohibited data back into the parent trajectory. See [Subagent reads](/how-it-works#subagent-reads) and [Subagent Returns](/contracts#subagent-returns).

## Shell and native file tools

Every Claude Code install includes the [Claude Code tools battery](/battery-claude-code). It routes `Read`, `Write`, and `Edit` through the [file-tracker](#files-that-keep-their-labels), confines every shell command to the files and hosts it declares ([SafeBash](#safebash)), and blocks `PowerShell`, `Monitor`, `Grep`, and `NotebookEdit`.

The `coding` battery gives the canonical shell and file tools of other harnesses these contracts:

- **Reading credentials narrows the trajectory.** Read, search, or shell calls that contain configured credential-path spellings narrow the audience to `self`.
- **Writing where another process looks requires trust.** Writes to credential and control paths require a `trusted` trajectory. After suspicious input, the exact call requires human review.
- **Changing the controls requires review.** Writes to harness settings or OpenAPPA policy always ask the person running the session.
- **Shell commands are classified per call.** [Annotators](/how-it-works#annotators) determine the required trust and audience for each shell command. The GitHub battery can supply repository visibility and authorship context for `git push` and `gh` commands.

These contracts label what a command reads or returns; they do not sandbox the command. For confinement, see [SafeBash](#safebash).

## Files that keep their labels

A coding agent writes files that outlive its trajectory. The file-tracker plugin keeps a label on every file a mediated tool call writes, so a later read of that file carries the restriction the write carried. The read can happen in the same session, in a fresh session, or after a resume.

The plugin is a [recorder](/contracts#recorders) and an [annotator](/contracts#annotators) for the same tools:

- As a recorder, it receives the label each released call published: the trajectory's label at release, combined with the call's `forwards` and `delta`. It stores that label on each file the call wrote.
- As an annotator, it answers a `Read` with the stored label as `delta`. An `Edit` forwards the file's existing label, so the edited file keeps it. A `Write` replaces the file, so the new label replaces the old one.

The plugin's built-in path rules give credential paths such as `.env*` and `~/.ssh/*` the audience `self` before any call touches them. They also set the trust a write to a control file such as `CLAUDE.md` requires. A deployment adds rules in `~/.config/appa/file-tracker.toml`. The plugin never denies a call: the engine checks the annotation's `requires`.

### SafeBash

A shell command can read and write any file the process can reach, so a label on a file does not cover a native shell. Every shell command therefore declares three lists beside its `command`: `input_files`, `output_files`, and `network_hosts`. The command can read only its inputs, the system directories (`/usr`, `/etc`, `/lib` and similar), the directories on its `PATH`, the installed toolchain each `bin` directory on `PATH` belongs to, the built-in toolchains, and the download caches of Go, cargo and npm. It can write only its outputs and connect only to its hosts. Every other top-level directory, such as `/home`, `/opt` or `/srv`, is hidden from it, and so are the working directory and the temporary directories.

An input is a file, a directory or a Unix socket. An input that does not exist is read as nothing. A directory input carries the labels of the files in it that the command can see. A file in it that a path rule gives a restricted audience, such as `.env`, is hidden from the command unless the call names it, so `git status` over the project leaves the session's audience unchanged and `cat .env` narrows it to `self`. The person's own Claude Code settings, `~/.claude/settings*` and a project's `.claude/settings.local.json`, have the audience `self` too, because their `env` block can hold credentials. A project's committed `.claude/settings.json` is not labeled. A file labeled by a recorded write stays in sight and its label counts, so a session reads back what it wrote. A file that the git repository tracks, or a directory that holds one, also stays in sight and its label counts: the repository's history holds its content, and git cannot work in a tree where a tracked file is hidden. A git directory, such as `.git`, holds every file its index or history reached, so an input that holds one or lies in one carries the labels the path rules give those paths, including a credential that was committed and then removed. A git directory that git cannot read carries the label of every path rule. A socket input, such as `/var/run/docker.sock` or the SSH agent's `$SSH_AUTH_SOCK`, lets the command reach the service behind it; every socket the call does not declare stays hidden. A socket carries the label its path rules give it. A container daemon can mount any file into a container, so the built-in rules give the Docker, Podman, and containerd sockets the audience `self`: a call that declares one narrows its session to `self`. An output may lie in a directory that does not exist yet; the directory is created. An output file that does not exist is an empty file while the command runs, because the sandbox can only allow writes to a file that exists. If the command leaves it empty and unchanged, it is removed again. A tool that refuses an empty file, such as `uv` with `uv.lock`, needs the file's directory declared instead. An output written with a trailing `/` is a directory, such as a build directory, `node_modules/` or a compiler cache: the command reads what it holds and writes under it, and each file it creates or changes there takes the call's label. A file it leaves as it was keeps its own label. A file in it that a path rule guards for writing stays as it is unless the call names it. The built-in rules guard `CLAUDE.md` and a repository's hooks and config: in `.git/`, in a submodule's `.git/modules/<name>/`, in `config.worktree`, in a bare repository named `<name>.git`, and in husky's `.husky/`. A bare repository with another name is not guarded. Each call also gets a temporary directory of its own as `$TMPDIR`, removed when the call ends; the call does not declare it.

The built-in toolchain list holds the usual install locations outside the system, such as `/opt/homebrew`, `~/.cargo/bin`, `~/.local/bin`, `~/.nix-profile` and the `mise`, `nvm` and `pyenv` directories. A deployment adds more under `[sandbox] readable` in `~/.config/appa/file-tracker.toml`. A toolchain directory never changes the label of a call that declares it: only recorded writes label a file there. No path rule applies under a toolchain, so a test certificate in a toolchain is not a credential. A directory directly under the home, such as `~/.cargo`, is not opened as a toolchain, because it holds the person's own configuration. A file in `/etc` or `/run` that a path rule labels, such as an SSH host key or a token, is hidden too. `~/.gitconfig` stays hidden; the command gets the person's git name and email as `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables instead. The command starts without the environment variables whose names hold credentials: a word such as `TOKEN`, `SECRET`, `PASSWORD`, `KEY` or `AUTH`, and every `ANTHROPIC_` and `APPA_` variable. A write to an undeclared path does not reach the disk. An output file that exists is read as well, and its label counts as an input's does, so a command such as `npm install --save-dev` may rewrite `package.json`.

The download caches of Go (`$GOMODCACHE`, by default `~/go/pkg/mod`), cargo (`$CARGO_HOME/registry`) and npm (`~/.npm/_cacache`) are toolchains too, so a build that uses a downloaded dependency needs no host. A call that names a host of a cache's registry (`golang.org`, `crates.io` or `registry.npmjs.org`, or a subdomain) also writes that cache without declaring it. Each file the call adds or changes there takes the call's label. A command that does not declare a toolchain or a cache does not see a file recorded there with a label that restricts the audience or lowers trust, because its call does not carry that label. A call that declares the directory sees the file and carries its label. A cargo call that reaches the registry also rewrites the index file `~/.cargo/registry/index/*/config.json`, which every later cargo command reads. After one such call in a session whose label restricts the audience or lowers trust, a later call that does not declare `~/.cargo/registry` cannot read the index, and `cargo build` fails, even with `--offline`. Declare the cache in `input_files`, which carries its label, or check the labels with `appa file-tracker labels ~/.cargo/registry` and remove them with `appa file-tracker reset <path>` (see [Resetting labels](#resetting-labels)).

APPA builds no sandbox of its own. It confines each command in the best sandbox the harness has:

| Where | Tool | Sandbox |
|---|---|---|
| Claude Code started by `clappa`, on a tested release (2.1.266 and 2.1.319) | Claude Code's own `Bash`, with the three lists added | Claude Code's built-in sandbox (bubblewrap on Linux, Seatbelt on macOS) and its proxy, configured for each call |
| Claude Code otherwise | `mcp/file-tracker/SafeBash`, served by the file-tracker | Anthropic's sandbox runtime `srt`, the same runtime, started for each call |

Before each session, `clappa` loads a preload into Claude Code and runs its self-test. The self-test checks that the lists survive Claude Code's input handling, that an undeclared file is hidden and an undeclared write does not reach the disk, and that each command reaches only its own hosts. If the release is not on the tested list or any check fails, `clappa` disables `Bash`, registers SafeBash, and prints why. When `srt` is not installed, `clappa` and `appa plugin install claude-code` say so and name the command that installs it. `clappa` keeps any `BUN_OPTIONS` already set, after its own preload, so another tool's preload loads too. Under `clappa`, a model's `Bash` call that declares none of the lists runs with the working directory as its only input, no output and no host, and its result says so. A `Bash` call that declares some of the lists, or no lists in Claude Code started without `clappa`, is refused. A command the person types at the prompt with `!` reaches no hook, so nothing labels its result: under `clappa` it reads and writes the working directory, or the git repository that holds it, reaches no host, and sees no file whose label restricts its audience. A file whose label only lowers trust stays in sight, because the person who types the command vouches for it. Both tools return the exit code, standard output and standard error as a successful result, so the call's label applies whatever the exit code.

The annotator forwards the combined label of the input files, and the call's result carries it. The recorder applies the published label to every output file.

The label of a copied secret therefore survives across sessions. If the agent reads `.env` and writes its content into `/tmp/notes.txt`, that file now carries `self`. A later shell command that reads `/tmp/notes.txt` narrows its session to `self`, in this session or in any other, so a later public sink such as `WebFetch` is refused.

A call that declares a host sends what the session holds to it. The Claude Code battery therefore requires public data for every networked call, and labels its result `suspicious`, because strangers write what a host returns. The file-tracker still labels the call's files, so a command that reads `.env` and declares a host is refused. A host rule in the root policy is checked first and can give a known host its own contract:

```toml
[[policy.tool]]
name = "host/claude-code/Bash(network_hosts:*.corp.example)"
annotator = "file-tracker"
recorder = "file-tracker"
requires = { audience = { contains = ["internal"] } }
```

The battery gives the npm, Yarn, Go module, crates.io and PyPI registries rules of their own: a call whose hosts all match one registry's pattern runs as a call without hosts does. PyPI's rule names both of its hosts, `pypi.org` and `files.pythonhosted.org`. A registry learns which packages the command fetches.

A host rule matches a call only when every host the call declares matches its pattern. A call that also declares another host falls through to the battery's rule. `*` matches dots too, so write `example.com` and `*.example.com` as two rules rather than `*example.com`.

### Resetting labels

A recorded label stays on a file until the person running the sessions resets it. A session that read `.env` records `self` on every file it writes, including source files, lock files and `.git` objects that hold no credential. A fresh session that reads those files narrows to `self`. Only the person can decide that a file no longer holds what its label says, so no tool call resets a label.

Run these commands in your own terminal, outside the agent's session:

```sh
appa file-tracker labels [<path>...]   # the labels recorded under each path; default: the current directory
appa file-tracker reset <path>...      # remove the labels recorded at or under each path
```

`reset` removes recorded labels only. A path rule still labels each file it matches, so `.env` keeps `self`. A label recorded on a directory above the path still covers it; `reset` names that directory. A session already running keeps its own label, and the files it writes next are labeled again.

`clappa` says before a session starts when files under the working directory carry recorded labels. An agent cannot reset a label itself: the label store is outside every command's declared outputs, so the sandbox keeps it read-only. A call that declares the store as an output asks the person running the session first.

### Per harness

| Harness | File tools | Shell |
|---|---|---|
| Claude Code | `Read`, `Write`, `Edit`, `Bash`, and `mcp/file-tracker/SafeBash` use the file-tracker annotator and recorder. | `clappa` confines `Bash` per call in Claude Code's sandbox, or serves SafeBash where it cannot. It starts Claude Code without `PowerShell`, `Monitor`, `Grep`, and `NotebookEdit`, and the battery blocks them. |
| OpenCode | No file-tracker yet: the battery gives `read`, `write` and `edit` static rules, and a read under `*/private/*` narrows the session to `internal`. | The shell permits only `pwd`. |
| Amp in an orb | The orb's file system has the trajectory's lifetime, so it counts as part of the trajectory. No file-tracker runs. | A proxy in the orb posts each outbound connection as `host/amp/appa_network`. Sending a file to another thread is an outbound flow that `requires` checks. |

The `appa` executable serves the file-tracker as `appa file-tracker`. Claude Code's default policy binds it, and the Claude Code battery routes the tools. The SafeBash fallback needs `srt` on `PATH`.

### Limits

- A host learns what the command sends, and a public host is public. A session that holds `self` or `internal` data cannot reach the network until a host rule names a host for those readers.
- Credential variables are withheld by name. A variable whose name does not say it holds a credential reaches the command.
- The system directories other than `/etc` and `/run`, such as `/usr` and `/lib`, the directories on `PATH`, the toolchains they belong to and the readable toolchains are assumed to hold no credentials. A labeled file there is readable by a command that does not declare it.
- A package's install script runs with what the command can read and reaches the registry the command declares.
- A service reached through a declared socket is outside the sandbox. What the command sends it, and what the service does with it, carries no label.
- A directory input or output is walked on every call for its labels and version. A directory with more than 250,000 entries is refused; declare a narrower one.
- A file that changes outside a mediated call keeps its recorded label. A process outside the sandbox can still copy a file without its label.
- A read waits while a write of the same file has not yet reached the recorder. If the write does not settle within the wait, the read fails and the agent can propose it again.
- The stored audience must be one clause of audiences the policy declares. A file that two unrelated restricted audiences wrote fails its next annotation.
- Model-provider requests and assistant messages are not mediated.
