---
name: appa-local-reset
description: Prepare a local test of the current OpenAPPA checkout — build the `appa` binary into ~/.local/bin, refresh the `appa-guide-test` Claude skill from the checkout's guide sources, and purge the local deployment. Use when the user wants to try local APPA or appa-guide changes before release.
---

# appa-local-reset

Put the current checkout's `appa` binary and `appa-guide` skill where a local
test can use them. Do not run the install itself; the user does that.

## 1. Build and place the binary

Build the release `appa` binary from this checkout. Take the package name and
required features from the `[[bin]]` entry in `appa-runtime/Cargo.toml`; do
not assume them.

Copy the built binary to `~/.local/bin/appa` through a temporary file and a
`mv`, so a running `appa` is not overwritten in place. Run
`~/.local/bin/appa --version` and keep its output for the report.

If the build fails, stop and report the error. Do not place an old binary.

## 2. Refresh the `appa-guide-test` skill

The installed `appa-guide` skill is composed by
`appa-runtime/src/init/skill.rs`. Read that file to learn which source files
make up the skill text, in which order and with which separator, and which
file it installs as the reference beside it. Compose the same content from the
checkout.

Write it to `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills/appa-guide-test/`,
with two changes so it does not collide with the installed `appa-guide`:

- the frontmatter `name:` is `appa-guide-test`;
- every path to the skill's own reference file under `skills/appa-guide/`
  points under `skills/appa-guide-test/` instead.

Leave other paths unchanged. Replace only the files this skill writes; do not
touch `skills/appa-guide/`.

Check the result: the frontmatter names `appa-guide-test`, and no path to the
reference file still points under `skills/appa-guide/`.

## 3. Reset the local deployment

Start the next test from nothing with the installer's own start-over:

```
~/.local/bin/appa plugin remove claude-code --purge --json
```

It stops the runtime and deletes the default deployment's config and data
directories: policy, databases, logs, retained versions, and install state.
Deleting only the config leaves the install state, so the next install is not
a first install. Invoking this skill authorizes this purge, nothing else.

Then check `ps ax -o command | grep '[a]ppa runtime'`. If a runtime still
serves the purged deployment, report it.

## 4. Report

Say in a few short lines:

- the binary version from step 1 and the branch or commit it was built from;
- that `/appa-guide-test` now carries this checkout's guide;
- the directories step 3 purged;
- the next step to test the local installation, run from this checkout:

  ```
  ! ~/.local/bin/appa plugin install claude-code --revision <full HEAD commit>
  ```

  then start a new Claude Code session. Without `--revision`, the install
  takes a published release, not this checkout. The revision exports
  committed state only; uncommitted changes are not in it.
