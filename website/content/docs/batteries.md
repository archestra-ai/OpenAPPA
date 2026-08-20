---
title: Our vision for batteries
category: Integration
order: 7
description: Our vision for reusable security coverage, and the deliberately small first implementation.
---

Security rules should travel with the tools they protect. A team should not have to rebuild the same Claude Code or Grain policy for every deployment.

That is our vision for APPA batteries. A battery will package ready-made OpenAPPA coverage for one agent harness or toolset. It will own the contracts and default judgments that are common to every deployment. Local settings will keep deployment-specific trust decisions under the deployer's control.

> [!NOTE]
> Batteries are an approved design, not a feature in the current release. This page describes what we want batteries to become and how we will build the first version.

This page starts with the product and security principles behind batteries. It then walks through policy composition, local resolver processes, customization, failure behavior, and installation. It ends with the boundaries we are deliberately keeping around the first release. If OpenAPPA's flow model is new to you, read [How it works](/how-it-works) first. If you want to see today's harness integration, start with [Claude Code](/claude-code).

## Our vision: coverage should travel with the integration

Today, a deployment that covers several toolsets must collect their declarations in one policy file. That works, but it puts the wrong person in charge of repeated details.

The maintainer of a Claude Code integration knows its tool names and argument schemas. The maintainer of a meeting-notes integration knows which calls read transcripts and which calls can publish them. A deployer should review those contracts, not reconstruct them line by line.

We think a security feature that begins with copying hundreds of lines into `appa.toml` will not survive contact with real tools. Batteries will move reusable declarations back to the people who understand the integration, without moving local trust decisions out of the deployment.

Four principles shape the design:

1. **Integration maintainers define reusable coverage once.** Tool names, argument schemas, source restrictions, and outbound walls belong beside the integration.
2. **Deployers keep control of trust.** Private paths, workspaces, and trusted destinations remain local settings.
3. **Review stays explicit.** APPA will show the complete effective policy and reject ambiguous conflicts. Includes will not create hidden override precedence.
4. **Failure never becomes permission.** A broken include, resolver, or settings file will refuse activation or supply no answer. It will not silently weaken the serving policy.

This is not a vision for a policy marketplace or a general package manager. The first version will add only enough composition and process management to make reusable coverage practical.

## What a battery will contain

Each battery has two parts:

1. A policy document declares tools, input schemas, source restrictions, outbound requirements, resolver names, and review hints.
2. Resolver functions make the dynamic judgments that static policy cannot make, such as whether a file path is private or a destination is trusted.

The policy remains declarative. Resolver code supplies bounded answers to declarations; it cannot add a tool or change a tool contract.

This division is important. Contracts define what a tool can read or release. Resolver functions answer only the questions those contracts registered. A battery author cannot hide a new tool contract inside executable resolver code.

## How policy composition will work

The first version will add one-level policy includes. A deployment will select batteries beside its local policy:

```toml
[policy]
include = [
  "batteries/claude-code.toml",
  "batteries/grain.toml",
  "./my-policy.toml",
]

[externals]
timeout_ms = 5000
max_body_bytes = 65536

[externals.dynamic]
command = ["node", "./resolvers.js"]
```

An include is deliberately less powerful than a package system:

- Paths are local. APPA does not fetch policy from a URL.
- An included document cannot include another document.
- Every document must use the same policy version.
- Declarations merge additively. A duplicate tool, Authority, Transformer, cast, or resolver name refuses the complete load.
- A singleton setting, such as the trust chain or deployment profile, can appear in only one source.
- Include order controls display and diagnostic order. It does not create override precedence.

APPA will combine the source documents and validate one effective policy. A trajectory will record that effective policy, so replay will never depend on an included file that later changes or disappears.

Reload will follow the same rule. APPA will read and validate every source before swapping deployments. A missing file, syntax error, version mismatch, or name conflict will leave the currently serving deployment unchanged.

The included files will remain the human review surface. The effective policy will be deterministic and available for diagnostics, but nobody will have to maintain the generated result.

## How dynamic judgment will run

A tool contract can depend on a proposed call's actual value. A file reader may need to know whether `/work/public/readme.md` or `~/.ssh/config` is being opened. A messaging tool may need to know whether a channel is internal.

OpenAPPA already supports these judgments through an HTTP dynamic resolver. Batteries will add a second binding: one child process managed by the runtime.

```toml
[externals.dynamic]
command = ["node", "./resolvers.js"]
```

`command` will contain an executable followed by its arguments. APPA will not invoke a shell or embed a language runtime. The process can be JavaScript, Python, or a compiled program. It will start in the directory containing `appa.toml`, so relative paths will have one predictable base.

The deployment will choose which battery functions that process serves:

```javascript
import { claudeCode, grain, run } from "@appa/batteries"

run({
  ...claudeCode(),
  ...grain(),
})
```

The process will announce its resolver names when it starts. APPA will compare them with the effective policy before activating the deployment. A missing, duplicate, or unknown name will refuse activation instead of leaving part of the policy unwired.

Requests and responses will use versioned, newline-delimited JSON over standard input and output. The first version will serialize requests, so the protocol will need no correlation identifier. Standard error will remain available for bounded operational logs; standard output will contain protocol messages only.

## How deployments will customize a battery

Most customization should be data, not a fork of shipped code. A battery will read one obvious settings file:

```toml
[grain]
private_workspaces = ["customer-research"]

[claude_code]
private_paths = ["~/clients", "~/.ssh"]
trusted_domains = ["docs.internal.example"]
```

The resolver will validate fresh settings before it answers each request, or detect changes with the same next-request behavior. This matters during revocation. If someone replaces a valid file with malformed settings, the resolver will return no answer. It will never keep using the older, more permissive configuration.

Deployers will be able to replace one judgment without editing battery code:

```javascript
const defaultGrain = grain()

run({
  ...claudeCode(),
  ...defaultGrain,
  "grain.transcript": async (request) => {
    if (request.value.startsWith("public-demo/")) return []
    return defaultGrain["grain.transcript"](request)
  },
})
```

That escape hatch will be intentionally visible. Custom resolver code will join the deployer's trusted base and can weaken protection. Review will have to cover the entry file as well as the local policy and settings.

## How private-data walls will preserve public work

Blanket outbound blocks are easy to build and hard to live with. We want a battery to stop a private transcript from reaching an external channel without stopping an agent from posting ordinary public text to that same channel.

The design will compose two dynamic judgments:

- A source resolver will leave ordinary content unrestricted and assign restrictive readers to private content.
- An outbound resolver will require nothing for a trusted destination. For an untrusted destination, it will require a reader that only Public data can satisfy.

Suppose a Grain battery marks transcripts from `customer-research` as private. The agent reads one of those transcripts and then proposes a post to `#external-partners`. The source restriction and destination requirement will not match, so OpenAPPA will refuse the call before the transcript leaves the harness.

The refusal should explain the next move in ordinary language:

> This Grain transcript is private, so APPA did not post it to `#external-partners`. Remove the private content, choose a trusted destination, or add the destination to `battery-settings.toml`.

A clean trajectory that contains only Public data will still be able to post to the same destination. Installing the wall will not make the tool useless.

## How resolver failure will stay closed

The resolver process is part of the decision path, so failure behavior matters more than convenience.

Startup and every judgment will share the deployment's external timeout. If the process times out, exits, emits malformed output, or exceeds the response-size limit, APPA will treat that as no answer. It will not turn an operational failure into permission or an Engine fact.

The runtime will terminate the failed process and start a fresh one before the next judgment. Calls already waiting on the failed process will receive no answer rather than waiting behind a restart. Feedback will name the command, failure class, and repair action without exposing the value being resolved:

> APPA could not check the Grain destination because `resolvers.js` exited. The call was not run. Fix the resolver and reload the deployment.

Reload will be equally conservative. APPA will start the replacement process, check its readiness and resolver names, and only then activate the new deployment. The old process will stay live if replacement validation fails.

## Where the battery boundary will remain

A battery will judge the values that reach its declared tools. It will not see every fact about the operating system.

Globs can conceal paths. Shell commands can invoke interpreters, process launchers, or command substitution. One tool can stage content on disk for another tool to send later. The first batteries will take the restrictive path when text is ambiguous, and their review material will state these limits.

Operating-system sandboxing will remain the access-prevention boundary. It must also stop the agent from changing APPA policy, resolver code, or live settings. Batteries and sandboxing will solve different parts of the same problem; neither will make the other optional.

## How installation will work

Installation should still be one action. The installer will place trusted battery files, update the short include list, create default resolver and settings files when they are absent, bind the managed command, and validate the complete deployment. It will not overwrite local customization.

The completion summary will name the installed batteries, covered tools, resolver names, settings location, and known limits. An unknown harness tool will remain blocked until its battery is updated or a local policy declares it.

The installer will perform mechanical setup only. It will not decide which local paths are private or which destinations deserve trust. Those decisions stay visible in the settings and policy that the deployer reviews.

## What we are deliberately not building yet

The first implementation will not add nested includes, remote packages, dependency resolution, inheritance, policy overrides, automatic updates, concurrent process requests, or in-process scripts.

We want evidence from real battery use before adding any of those mechanisms. More composition power would also create more precedence rules, supply-chain questions, and failure modes. None is necessary to prove the first idea.

Our goal is simple: installing coverage for a known integration should be one action, reviewing it should remain straightforward, and customizing local trust should not require editing shipped code. We can reach that goal with local policy includes and one managed resolver process.

That is the first version we intend to build: reusable coverage without hidden precedence, local judgment without another service to operate, and failure that never quietly becomes permission.
