---
name: appa-local-reset
description: Prepare a local test of the current OpenAPPA checkout — build the `appa` binary into ~/.local/bin, refresh the `appa-guide-test` Claude skill from the checkout's guide sources, and reset the local deployment (policy config, credential database, runtime database). Use when the user wants to try local APPA or appa-guide changes before release.
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

Start the next test from nothing: remove the policy config, the credential
database, and the runtime database. Invoking this skill authorizes deleting
exactly these files, nothing else.

1. Find the paths; do not assume them. The config and data directories come
   from `installed_config_dir` and `installed_data_dir` in
   `appa-runtime/src/init/paths.rs`, which honour `APPA_CONFIG_DIR` and
   `APPA_DATA_DIR`. The credential database is the file
   `CredentialStore::for_config` in `appa-runtime/src/credentials.rs` names
   beside the config. The runtime database is the `--db` path
   `appa-runtime/src/runtime_start.rs` gives the runtime it starts.
2. Stop the runtime first, so no process holds the database open: run
   `~/.local/bin/appa runtime stop`. Then check
   `ps ax -o command | grep '[a]ppa runtime'`. If a runtime still serves
   one of these files, stop and report it; do not delete under it.
3. Delete the config file, the credential database, and the runtime database
   with its SQLite side files (`-wal`, `-shm`). A missing file is fine. Leave
   everything else in those directories: installed batteries, binaries,
   logs, and settings.

List the paths you deleted in the report.

## 4. Report

Say in a few short lines:

- the binary version from step 1 and the branch or commit it was built from;
- that `/appa-guide-test` now carries this checkout's guide;
- the files step 3 deleted;
- the next step to test the local installation:

  ```
  ! ~/.local/bin/appa plugin install claude-code
  ```

  then start a new Claude Code session.
