---
title: Create your own battery
category: Batteries
order: 6.7
description: Build the smallest useful security contract for a tool interface.
---

A battery should be **as lean as possible, but not leaner**. Start with TOML
contracts. Add machinery when a useful, correct decision needs facts or reasoning
that those contracts cannot supply.

A battery describes a specific tool interface: what data calls send, who may see
results, how much to trust those results, what changes, and what requires review.
Identify the MCP implementation and supported tool sets, not just the provider's
brand. Two servers for the same service can expose different interfaces.

> **Ask your coding agent**
>
> ```text
> Read https://openappa.com/write-a-battery and inspect this server's implementation.
> Build the smallest useful battery for its supported tool interface.
> Use TOML for known contracts and expressible argument-dependent decisions.
> Add a resolver or annotator only for a concrete decision TOML cannot supply.
> Explain which provider facts it needs and what happens when they are unavailable.
> Keep deployment choices in the root policy. Do not invent resource permissions.
> Add focused behavioral evidence, documentation, and the catalogue entry.
> Run the checks relevant to the change. Report coverage, assumptions, and limits.
> ```

## Choose the implementation from the decision

| What determines the contract? | Start with | Example |
| --- | --- | --- |
| Known tool behavior | Static TOML | A tool always publishes its input publicly. |
| Arguments already present in the call | TOML selectors, parameter constraints, and argument references | A destination reader is supplied in `to`; a configured resource has a known audience. |
| Current provider state | A deterministic annotator or resolver | A GitHub repository's visibility and permitted readers determine a push's audience requirement. |
| Behavior that cannot be classified by those rules | An existing bounded interpretation mechanism | The Claude Code Bash annotator interprets a proposed shell command. |

Argument dependence alone does not require Python. Conversely, a static rule that
forces operators to maintain every changing repository ACL manually may be too
limited to provide the integration you intend to support.

### Example: pushing to GitHub

The same push tool can target a public or private repository. The call identifies
the repository, but does not establish who can read it.

- A public destination requires input that may be shared publicly.
- A private destination requires input that may be shared with its permitted
  readers. `private` does not mean that every organization member has access.
- A failed lookup must never be interpreted as public visibility. Refuse unless
  the complete contract has an explicitly justified conservative fallback.
  Requiring public input can protect outbound confidentiality; it does not make
  the returned content public or establish that the action itself is acceptable.

A useful mixed public/private integration therefore needs provider lookup or an
explicit, maintained source of resource permissions. That machinery earns its
place. Keep the provider lookup and interpretation focused; the runtime already
owns contract enforcement and the review mechanism.

The [repository visibility example](https://github.com/archestra-ai/OpenAPPA/tree/main/examples/test-github-battery)
illustrates an annotator making a provider request. It is a read example using a
configured private audience, **not a complete repository ACL resolver or push
integration**. Visibility alone cannot establish that configured audience.

Read and write audiences have different constraints. A result label may name a
subset of its authorized readers. A write's audience requirement must cover
**every reader of the destination**. A common subset can safely restrict reads
across several resources while being insufficient for writes to those resources.
An annotator must establish both outbound requirements and returned-content labels;
mutation responses can contain existing private data.

## Keep ownership explicit

The battery owns reviewed tool semantics and documented defaults. The deployment
owns credentials, physical server bindings, resource audience mappings, identity
policy, and the authority that can approve an action. A requirement for review
can be packaged; choosing who may grant that review belongs to the root policy.

An audience source discovers provider users or groups. An identity resolver
establishes which principals those provider claims represent. A resource ACL
resolver establishes who may read a particular resource. These are different
questions: workspace membership does not establish access to every private
channel, repository, meeting, or issue.

Optional provider implementations can ship alongside their contracts. Separate
ownership does not require a new package type. Activate and configure them from
the deployment where required by the existing configuration model.

Root tool rules take precedence over included rules and supply a complete
replacement annotation. Preserve the relevant trust, audience, review and effect
fields when overriding a resource. If the rule assumes a fixed argument shape,
constrain it with `parameters`; for example, `additionalProperties = false`
prevents an unexpected related-content or reassignment field from reusing that
rule. Use supported policy constraints rather than copying the provider's entire
API validator.

## Start with three files

```text
marketplace/batteries/your-server/
  appa-package.toml
  appa.toml
  README.md
```

`appa-package.toml` declares the package name, description, policy file, supported
hosts and namespaces. Declare helpers only when the package actually needs them.
`appa.toml` contains the contracts, using canonical tool IDs such as
`mcp/<server>/<tool>` or `host/claude-code/<tool>`.

The README records the supported server implementation/version, tool coverage,
source evidence, setup, assumptions and known limits. A deliberate subset is
acceptable: name the unsupported tools. A tool omitted from this battery receives
no permission from it; a deployment's other rules may still cover it.

Add an annotator, audience source, sanitizer or its tests only when its behavior
is part of the integration. A helper should solve a named provider problem. It
should not introduce another rule language, a general schema validator, a build
system or copies of runtime behavior without a demonstrated need.

For executable components:

- Use the existing consult protocol and constrain the declaration's mandate.
- Keep credentials out of files and arguments; use the supported environment binding.
- Never turn provider errors, partial membership or unresolved permissions into
  a successful empty answer or a broader audience.
- If results are cached, define freshness and invalidation; stale ACLs can change a decision.
- Keep reusable plumbing shared where an implementation already exists. Add a
  shared abstraction only when repeated work demonstrates the need.

## Keep maintenance evidence small

Review the upstream implementation or authoritative documentation. Tool names and
MCP annotations alone do not establish security behavior. Reads can send secrets
in queries; writes can return existing private content; apparently read-only
helpers can fetch arbitrary external URLs.

Record the reviewed revision or observed endpoint and capture date. When schema
drift tracking is useful, retain fingerprints and use maintenance tooling to fetch
current definitions for review. Full captures can remain local or CI artifacts.
A hash detects a changed definition when compared; it neither validates calls nor
proves that unchanged definitions imply unchanged server behavior. Do not add a
provider-specific generator just to maintain these records.

## Test the decisions the battery adds

Use the shared package/composition checks and [`appa replay`](/validation). A small
trace should demonstrate the relevant boundary: an allowed operation, a refused
operation, and a downstream consequence such as private content being unable to
reach a public sink. Exercise a changed resource or scope when the contract depends
on one. Prefer those cases over asserting a generated rule list against itself.

A static battery does not need a Python test suite or a Rust test file by default.
Use a focused runtime integration test when replay cannot exercise the required
boundary. Do not repeat generic host, approval, installation, or rollback tests
for each provider.

Executable provider logic does need direct tests. Use synthetic or sanitized
provider responses for its branches and failure behavior: for example, public and
private repositories, unknown permissions, pagination failures and identity
claims. Keep Python fixtures in Python or data files, rather than embedding a
second implementation in Rust strings.

Replay does not execute the proposed tools, but configured external annotators
can still make network calls. Use fixtures for deterministic checks and label any
live provider verification separately.

```sh
# After editing the package, regenerate and commit the catalogue with it:
bash scripts/appa-marketplace.sh
bash scripts/appa-marketplace.sh --check
# Shared package tests include verification of committed package digests:
cargo test --locked -p appa --test marketplace
# For a deployment example and its behavior trace:
appa replay --config examples/your-battery/appa.toml examples/your-battery/behavior.appa
# Only when the battery has Python implementation tests:
python3 -B -m unittest discover -s marketplace/batteries/your-server -p 'test_*.py'
```

Add a focused deployment example if setup needs explanation. There is no need to
append every provider to one growing Claude Code configuration; shared marketplace
checks compose each battery with its declared hosts.

## Submit a reviewable change

Include the package, updated `marketplace/marketplace.toml`, a documentation page
under `website/content/docs/`, and a card in `website/components/BatteryCatalog.tsx`.
Add only the helpers, examples and behavioral evidence the integration needs.

The PR should explain the supported interface, the decisions the battery makes,
why any machinery is necessary, its assumptions, and what was actually verified.
Run relevant checks and repository CI. An unresolved security question should
remain an explicit limit or refused case until evidence answers it.

:::battery-review-checklist:::
