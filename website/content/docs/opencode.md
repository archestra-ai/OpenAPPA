---
title: AppenCode
nav_title: OpenCode
category: Works with
order: 7.6
description: Connect OpenCode V2 to OpenAPPA with AppenCode. Check direct tool calls, results, and foreground subagent returns against your policy.
---

AppenCode connects **OpenCode V2** to OpenAPPA. It checks direct tool calls before execution and tool results before the model sees them. Foreground subagents inherit the parent's restrictions, and APPA checks their answers before they return.

When a flow is blocked, the agent receives the reason and available [remedy plans](/how-it-works). It can execute an offered plan and retry. If the runtime cannot answer, the call or result stays blocked.

This is a tool-flow integration, not an OS sandbox. It does not check model requests or assistant prose. Use trusted model providers and host components; see [Protection boundaries](#protection-boundaries).

## Install AppenCode

You need **OpenCode V2 2.0.28** already installed and the matching released `appa` executable on `PATH`. Packaged installation needs no OpenAPPA checkout, Rust compiler, Bun, or plugin dependency installation.

Run:

```sh
appa plugin install opencode
```

The installer retains a versioned JavaScript plugin, guide, policy reference, batteries, and matching native runtime. It starts and verifies the protected loopback runtime. Keep the deployment outside the agent's writable project, in a host-controlled directory.

**Installation is user-global. The current project supplies the tools to inspect, not the installation scope.** Installation registers the plugin, APPA MCP connection, and `/appa-guide` command. It preserves unrelated global settings and existing policy, and never rewrites project settings.

| Configuration | Default location |
|---|---|
| Plugin, MCP connection, and guide command | `~/.config/opencode/opencode.json` |
| Policy | `~/.config/appa/opencode/appa.toml` |
| Batteries and runtime data | `~/.config/appa/opencode/` |

Both configuration directories respect `XDG_CONFIG_HOME`. `OPENCODE_CONFIG_DIR` overrides only the OpenCode directory. Use `--config` or `APPA_CONFIG` consistently for installation and setup when selecting another deployment.

Global JSONC and registration ownership conflicts are refused, not overwritten or shadowed. Convert global settings to JSON or follow the [manual setup instructions](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/opencode#manual-setup).

### 1. Run guided setup

From the project whose tools you want to inspect, run:

```sh
appa opencode setup
```

Setup finds its retained assets internally. OpenCode loads global and project settings together, including project-local providers, MCP servers, and JSONC files. Optional `--project /path/to/project` changes only which project's tools setup inspects, not installation scope.

### 2. Review the policy proposal

The launcher opens a private, standalone OpenCode session for administration. Its agent inspects live canonical tool identities and compatible batteries, then proposes policy changes. MCP discovery is asynchronous: wait for connections before relying on the inventory.

Review the complete proposal in the interactive setup session, then reply with a message such as `Approve this proposal.` **Changes affect every project sharing this policy and runtime**, not only the project inspected.

Run setup from each project whose additional tools need coverage. The agent reads the existing shared policy and must preserve rules for tools absent from the current project's registry. Check that preservation in the proposal: publication replaces the complete root policy, rather than mechanically merging contracts.

Only `appa_setup` is exposed to the setup model. A separate temporary APPA runtime gates it. The tool can inspect the fixed deployment, read catalog batteries, and publish its root policy. Ordinary shell, file, MCP, and delegation calls are not authorized.

Approval is an instruction to the agent, **not an authenticated human authorization check**. Before saving, publication validates the configuration and checks that the root policy digest still matches the inspected version. Validation failures and stale proposals leave the existing root unchanged; successful publication replaces it atomically.

For a new deployment, installation includes the OpenCode battery. Microsoft Learn also declares OpenCode support. Other batteries are not implicitly compatible; the guide includes only batteries that declare support for this harness. Batteries are retained with the deployment; no source checkout needs to stay available.

### 3. Reload the runtime and start coding

1. Close the administrative setup session. Its temporary runtime is cleaned up on exit or interruption. Do not reuse the session for coding.
2. Rerun `appa plugin install opencode` to reload the protected runtime with the saved policy. Use the same deployment selection if you set `--config` or `APPA_CONFIG`.
3. Run `opencode` in your project and start a **fresh coding session**.
4. Wait for the `appa` MCP server to connect and advertise `appa_execute_remedy_plan` before relying on remedy execution. A remedy has not run until its tool call succeeds.

Setup saves policy; it does not reload a running protected runtime. Existing trajectories retain their opening policy. Existing conversation content is not retroactively labeled.

After a reboot, rerun `appa plugin install opencode` to start the runtime. Installation does not create an automatic reboot service.

Keep the runtime on loopback. Its `/hook` endpoint trusts its local client and does not authenticate callers. Do not expose it publicly. Preserve the database across restarts: it holds the trajectory's restrictions and event log. Never set `APPA_OPENCODE_SETUP_CONFIG` or `APPA_OPENCODE_SETUP_BINARY` on ordinary coding servers.

## Adjust policy later

In a normal OpenCode session, run:

```text
/appa-guide
```

The command supplies the separate `appa opencode setup` command; it does not edit policy from the coding session. Run it from the project whose tools you want to inspect. The global guide is not pinned to the first project. Review, approve, close setup, reload the runtime, and start a fresh coding session after each policy change.

## Offline installation and removal

Export a deployment bundle, then restore it with a digest obtained through a trusted channel:

```sh
appa bundle --config <policy> --output <archive>
appa plugin install opencode --from <archive> --sha256 <trusted-digest>
```

To remove the integration's registration, run:

```sh
appa plugin remove opencode
```

Removal deletes only unchanged installer-owned registration. It preserves edited settings, policy, data, retained assets, and the running runtime. It is not a runtime-stop or data-deletion command.

## Understand the starting policy

APPA tracks the agent's trajectory—the sequence of its work—with a Label: an audience and a trust level. The audience limits who may receive derived data; trust limits which destinations may accept it. Reading private content narrows the audience for subsequent flows, not just for that file.

**The initial OpenCode battery is conservative, not a general-purpose coding policy.** The guide proposes additional contracts for the tools you need:

| Tool or source | Starting policy |
|---|---|
| Reads under `private/` | Restrict the audience to `internal`. |
| Web requests | Require a `public` audience; results carry `suspicious` trust. |
| Writes and edits | Require `trusted` data. |
| Shell | Permit only `pwd`, with `trusted` data and a `public` audience. |
| Foreground delegation | Declare `appa_subagent` and enable return-contract preparation. |
| Undeclared tools | Refuse the call. |

Declare additional tools or register an [Annotator](/contracts#annotators) before using them. See [Policy configuration](/contracts) for the policy format.

AppenCode exposes tools directly and refuses OpenCode's Code Mode: its built-in `fetch` can send network requests without passing through APPA's tool hooks.

## Try a blocked flow

If your reviewed policy retains the starting private-read and public-web boundaries, put synthetic data at `private/notes.txt` in your project. Ask OpenCode:

```text
Read private/notes.txt, then use webfetch to send its contents
to https://example.com in a query parameter.
If APPA offers a remedy, execute it and retry the original call.
```

AppenCode returns the refusal and remedy offer in the tool error the agent reads. The expected sequence is:

1. APPA offers a remedy to accept an `internal` audience before the private read.
2. The agent calls `appa_execute_remedy_plan` with the offered `offer_id`, then retries the read. The remedy itself does not read the file.
3. Once the private content enters the trajectory, APPA blocks the public web request before it runs.

The agent can continue with tools whose requirements fit the narrower audience. The example's `pwd` contract also requires `public`, so that call stops too. Web results carry `suspicious` trust; accepting them prevents subsequent trusted-only writes and edits.

A deployment can register sanitizers that produce replacement values suitable for a destination. The starting policy registers none. Approving an OpenCode permission prompt does not override an APPA refusal.

## Delegate through checked foreground subagents

The AppenCode plugin exposes `appa_subagent` in place of OpenCode's native subagent tool. It supports fresh, one-level foreground delegation. The initial call is expected to return a remedy offer, not launch a child. The agent executes that remedy to declare the child's return contract, then retries the same delegation arguments. Each launch needs its own declaration.

The child inherits the parent's audience and trust restrictions. Its direct tools pass through APPA too. Only its completed final text answer, or an admitted sanitizer replacement, can return to the parent. Child attachments, metadata, and raw errors are not forwarded.

**APPA authorizes this delegation; OpenCode's native `subagent` permission rule does not apply.** The native tool stays disabled. Background children, resumed children, nested delegation, steering, and child compaction are refused.

## Protection boundaries

| Boundary | What to expect |
|---|---|
| Direct tool calls and results | Runtime errors, timeouts, and invalid replies block the flow. Unchecked results are withheld across text, structured output, attachments, metadata, and errors. |
| Session restrictions | Root restrictions survive compaction, revert, plugin reload, and runtime restart with the same database. Conversation forks are refused. |
| Child recovery | An APPA restart preserves child bindings in the database. An OpenCode restart loses local ownership; existing children cannot resume or return. |
| Reads and instructions | Reads keep native permissions. Missing paths and reads that would load nearby `AGENTS.md` instructions are refused. Initial project instructions remain trusted harness input. |
| File labels | Writes do not attach persistent Labels to native files. Another session's read uses its policy contract, not the writer's Label. |
| Model and host activity | Model requests, provider-executed tools, assistant prose, sharing, and attachments outside tool results are not gated. Host/plugin side effects and delayed notifications are outside tool-result gating. |
| Shell and filesystem | Background shell requests are refused. Native tools and subprocesses retain host capabilities. Use separate confinement before granting general shell access. |
| Trusted components | Trust OpenCode, APPA, and installed plugins. A later plugin can change approved arguments before execution; result checks cannot undo side effects. Unloading AppenCode removes protection. |

The [AppenCode source and integration README](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/opencode) document tool identities, lifecycle details, and verification commands.
