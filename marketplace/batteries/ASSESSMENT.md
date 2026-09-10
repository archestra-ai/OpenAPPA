# Battery assessment

Reviewed on 2026-09-10 against `ba6966dc`; Linear is assessed separately at PR
[#272](https://github.com/archestra-ai/OpenAPPA/pull/272), commit `b310e8e6`.
This is a review of repository contracts and implementations, not a new live
verification of provider APIs. The recommendations below are follow-up work;
this assessment does not change the shipped policies.

Standard: **as lean as possible, but not leaner**. Retain code that supplies
necessary provider facts or interpretation. Prefer TOML for decisions it can
express. Judge usefulness and security behavior before file count.

| Battery | Assessment | Next action |
| --- | --- | --- |
| GitHub | Static rules are a public-repository baseline; a useful mixed public/private integration needs additional resource resolution. | Prioritize repository visibility and permitted-reader resolution for reads and writes, including push. |
| Slack | Directory discovery is justified; the tool policy's broad `internal` assumption needs review for private/shared conversations. | Establish conversation audiences and search-result scope before promising mixed-permission coverage. |
| Grain | Static rules are useful for a uniform audience, but meeting and collection permissions may require finer resolution. | Verify resource visibility and outward-sharing behavior against provider evidence. |
| Google Workspace | Executable directory integration is the product today; there are no tool contracts. | Keep it and describe its audience/identity role accurately. |
| Claude Code | Static path rules plus an existing Bash interpreter address different problems. | Keep interpretation; assess path and indirect-command limits separately. |
| Linear, PR #272 | Plain TOML supports explicitly configured scope; its shared read/write audience needs a stronger deployment invariant. | Keep the simplification; add resource resolution if the intended scope expands to changing, heterogeneous ACLs. |

## GitHub: add necessary capability

[The policy](github/appa.toml) explicitly assumes public repositories. Private
repositories require root overrides. [The audience source](github/audience-source.py)
discovers the viewer, organization members and teams; those collections do not
establish the ACL of each repository.

A push to a public repository needs public input. A push to a private repository
needs input shareable with that repository's permitted readers. A useful
integration spanning both should resolve the destination and required audience,
rather than make operators duplicate rules for every changing repository.

Keep the directory source. Add a deterministic resource annotator only with a
clear account of visibility, access grants, identity mapping, API failure and
freshness. A configured read-result audience may include only authorized readers;
a write requirement must cover every destination reader. A shared subset is not a
safe substitute for the write audience. If the required facts are unavailable,
refuse unless the complete contract has a justified conservative fallback. Do not equate `private` with all organization
members, or the current token's access with everybody else's access.

The [visibility example](../../examples/test-github-battery/repository-visibility.py)
proves the provider-call pattern. It labels a read with a configured private
audience; it is not a complete ACL resolver or push integration. Review mutation
responses as well as outbound input. Focus evidence on public/private targets,
unknown permissions and cross-repository information flow.

## Slack: retain directory logic, examine resource scope

[The helper](slack/audience-source.py) performs provider requests, pagination,
membership filtering and identity-claim extraction. Those are legitimate provider
responsibilities. They should not be replaced with static lists merely to remove
Python.

[The tool policy](slack/appa.toml) assigns `internal` to
`slack_search_public_and_private`. [The example deployment](../../examples/claude-code-battery/appa.toml)
feeds `internal` from full workspace membership. The repository does not establish
that all those members may read every private-search result. Treat this as a
permission-model concern, not a request to rearrange files.

Determine which conversation identifiers, channel permissions and shared-channel
participants can be established before the call. Broad search across heterogeneous
ACLs needs a justified common audience, restricted query scope, or another
supported classification mechanism. A pre-call annotator cannot inspect results
that have not been returned. If the necessary audience cannot be established,
keep the unsupported case explicit rather than assigning a broad default.

## Grain: establish the supported permission model

[The policy](grain/appa.toml) labels meeting content and directory/settings results
`internal`. It permits trusted internal writes and requires review for outward
sharing and administration. Those are readable static defaults.

The next investigation should establish whether the intended connection exposes
meetings or collections with different readers, including external participants.
Uniform access can justify static contracts. Heterogeneous or changing access can
justify resource resolution. A common subset may safely restrict returned content,
but write requirements still need to cover every destination reader. The current files alone do not answer that provider
question.

Also verify what sharing calls expose and what mutation responses contain.
Requiring a human review mark does not itself establish that existing private
content may be disclosed. Add only the resolver and behavioral cases that those
findings justify.

## Google Workspace: keep the integration, clarify the category

[Its manifest](google-workspace/appa-package.toml) already says that it carries no
tool rules. [Its implementation](google-workspace/audience-source.py) supplies
viewer/directory/group facts and identity claims. This machinery is necessary for
that capability, rather than accidental scaffolding around static annotations.

Keep the implementation and provider-response tests. Describe it consistently as
an audience-source package in the catalogue. Audience selection and identity
policy remain deployment-owned. No separate package format is required merely to
make that distinction. If Workspace tool contracts are added, assess their resource
permissions independently of directory membership.

## Claude Code: retain interpretation and state its limits

[The battery](claude-code/appa.toml) uses static credential-path rules and the
existing model annotator for other Bash calls. Arbitrary shell behavior cannot be
covered usefully by a short list of static tool annotations. That is a justified
interpretation boundary; it does not need a new provider-specific Python engine.

The README already notes that path rules match the written path, not its resolved
target. Symlinks, aliases and indirect shell behavior are limits to investigate,
not evidence of complete secret-path protection. Review ordinary file reads'
trust treatment as well: `Read` currently preserves trust outside its audience
rules. Keep host/path enforcement work separate from this documentation change.

## Linear: preserve the baseline, allow justified growth

PR #272 now contains TOML, a manifest and a README. Its internal defaults require
the deployment's audience sources; resource overrides can use verified literal
readers. Writes require review, and the policy records mutation effects and
special handling for external image fetching and signed upload URLs.

This does not provide automatic issue, project or team ACL discovery. The shared
`internal` label is used in both read results and write requirements: it must be
contained within the authorized readership of returned content while covering
all readers of write destinations. A common subset alone only addresses the
read side. Tighten the deployment assumptions and resource rules accordingly;
review marks do not replace this audience requirement.

For a uniform, explicitly configured scope, static contracts can be sufficient.
For many differently shared resources, a focused resolver may be necessary. Do
not restore the removed generic rule language, copied schemas, generated profiles
or repeated runtime tests to obtain that capability.

## Recommended order

1. Adopt the revised authoring guide and corrected catalogue descriptions.
2. Design and verify GitHub's public/private resource contract using push and a
   read as the concrete slice. This is a capability gap, not a code-removal task.
3. Verify Slack and Grain's resource-audience assumptions before expanding their
   promised coverage. Add provider machinery where the findings require it.
4. Retain the directory sources and Bash interpretation. Share recurring plumbing
   only when a concrete repeated implementation justifies it.

Use existing replay and package checks throughout. Add direct tests for new
provider logic and its failure modes; avoid a fresh host or package-lifecycle
suite for each battery.
