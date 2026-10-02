# OpenAPPA release automation

## Release Please PR authentication

`Release OpenAPPA` creates and updates release PRs as `archestra-ci[bot]`, the
same organization-owned **Archestra CI** App used by Archestra's Release Please
workflow. Its installation already includes `archestra-ai/OpenAPPA`.

The `release-pr` job requests a token for **OpenAPPA only**, with Contents,
Issues, and Pull requests write permissions. Both Release Please and the
lockfile/marketplace synchronization push use that token. App-authenticated PR
events start CI without the workflow approval required for PR events generated
with `GITHUB_TOKEN`. The job's `GITHUB_TOKEN` has Contents read access, checkout
does not persist credentials, and the token action revokes its token on completion.
Release creation and asset publication keep their separate credentials.

An organization owner configures these **OpenAPPA Actions repository secrets**:

| Secret | Value |
| --- | --- |
| `ARCHESTRA_RELEASER_GITHUB_APP_ID` | `1667068` |
| `ARCHESTRA_RELEASER_GITHUB_APP_PRIVATE_KEY` | The complete PEM key for Archestra CI |

App settings: `https://github.com/organizations/archestra-ai/settings/apps/archestra-ci`.
Keep the existing selected-repository installation and App permissions. The
workflow does not request Actions or Workflows write access, or any ruleset bypass.

An App private key can mint tokens for every repository in its installation;
the workflow's token restriction does not restrict the key itself. The owner must
approve making this credential available to OpenAPPA and enter it directly in
GitHub's secure secret UI. GitHub cannot reveal Archestra's existing secret value.
Do not paste private keys into chat, files, logs, or PRs.

After configuration and merge, verify the next release PR update starts CI as
`archestra-ci[bot]`. Existing runs awaiting approval still need maintainer
approval or a subsequent App-authenticated PR update. This change does not
approve or merge release PRs.

## Runtime update automation

After `Release OpenAPPA` publishes a complete GitHub release, it calls
`Update OpenAPPA in Archestra`. The `release: published` event also handles
releases published manually. The explicit call is needed because GitHub does
not start another release workflow for releases created with `GITHUB_TOKEN`.

Only canonical stable tags such as `v1.2.3` are supported. Drafts and prereleases
do not produce updates. The updater verifies the published release, resolves
lightweight or annotated tags to a full commit, checks that commit belongs to
OpenAPPA main, and checks the Cargo workspace version. The release workflow
also passes its source commit, which must match the tag.

Archestra embeds the Rust runtime.
The updater changes the four OpenAPPA Git revisions in
`platform/archestra-rs/openappa-rs/Cargo.toml`, regenerates
`platform/archestra-rs/Cargo.lock` with Cargo, and compiles `openappa_rs` with the
locked dependency graph. Release authenticity and lockfile validation failures
stop before opening a PR or closing older updates. A bridge compilation failure
appears in the draft PR with a link to the logs, and the updater workflow fails
after creating it. Humans can repair compatibility on that reproducible update.
Build jobs receive read-only access. A separate
publishing job receives the write token and runs no dependency build scripts.

Each release has one `chore/openappa-vX.Y.Z` branch and a draft PR identifying
its GitHub release ID, source commit, and previous revision. Retries preserve
reviewer edits and never reopen closed or merged PRs. Older releases cannot
downgrade a newer installed revision or overtake a newer open update. Runs are
serialized. If the manifest or lockfile changes during compilation, rerun the
workflow against the new main rather than overwriting that dependency work.

After opening a newer draft, the updater closes older open PRs created by the
same GitHub App with this updater's branch and release marker. Unrelated PRs
remain open, and superseded branches are retained. For example, `v1.2.4` replaces
a pending `v1.2.3` update. Closure failures can be repaired by retrying `v1.2.4`.

Archestra runs **OpenAPPA Native Tests** on these PRs, including the real native
addon and PostgreSQL ledger. A human reviews compatibility and dependency
changes, marks the draft ready, and merges through the normal repository process.
The updater never approves, enables auto-merge, queues, merges, or pushes to main.
Archestra currently requires zero approving reviews in its main ruleset, so
human review is a process expectation, not an enforced reviewer count. The App
has no ruleset or branch-protection bypass privileges.

## GitHub App setup

First inspect the organization's existing Apps for a suitable dedicated updater.
The registered configuration is:

| Setting | Value |
| --- | --- |
| Name | OpenAPPA Archestra Updater |
| App settings | `https://github.com/organizations/archestra-ai/settings/apps/openappa-archestra-updater` |
| App ID | `5165953` |
| Owner | `archestra-ai` organization |
| Homepage | `https://github.com/archestra-ai/OpenAPPA` |
| Installation availability | Only this account (private App) |
| Selected repositories | Only `archestra-ai/archestra` |
| Repository permissions | Contents: read/write; Pull requests: read/write; Metadata: read |
| Organization/account permissions | None |
| Webhooks/OAuth/callbacks | Disabled; no user authorization required |
| Ruleset/branch protection bypass | None |

The workflow requests an installation token for `archestra` only, with just
Contents and Pull requests write permissions. The pinned GitHub App token action
revokes it at job completion. No Actions, Workflows, Issues, Administration,
release publishing, or organization permission is required.

After approving registration and installation, an organization owner uses
GitHub's secure UI to generate/download the App private key and enter it directly
as an Actions repository secret in **OpenAPPA**:

- Actions variable: `APPA_ARCHESTRA_UPDATER_APP_ID` (the registered numeric App ID).
- Actions secret: `APPA_ARCHESTRA_UPDATER_PRIVATE_KEY` (the complete PEM key).

Do not paste the key into chat, source files, logs, or a PR. Do not share the
secret with other repositories. App IDs are public configuration; private keys
are credentials. Key generation and secure secret entry are owner handoff steps.

## Retry or backfill

After the workflow is merged into OpenAPPA main and the App is configured,
choose **Update OpenAPPA in Archestra → Run workflow → main** and enter the exact
published stable tag. For example, `v0.30.0` backfills the current released runtime
if Archestra still has an older revision. Non-main manual runs are ignored.

Check the release's **Update OpenAPPA in Archestra** jobs and the linked draft PR.
When compilation fails because a runtime API changed, maintainers adapt the
Archestra bridge on the draft PR and use Archestra's native checks to verify it.
Retries preserve those edits. A rerun does not merge or publish either project.
