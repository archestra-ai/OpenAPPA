#![cfg(unix)]
//! Claude Code's own plugin semantics, captured against the real CLI.
//!
//! These are the facts init's sequence depends on, and none of them is stated
//! anywhere in this repository: they were established by running `claude`
//! against an isolated `CLAUDE_CONFIG_DIR`. They are pinned here so a change in
//! Claude's behaviour surfaces as a failing test rather than as a broken
//! install.
//!
//! Ignored by default: they need a real `claude` on PATH and mutate its plugin
//! registry, in an isolated config directory. Run with
//! `cargo test -- --ignored real_claude`.

use std::fs;
use std::path::Path;
use std::process::Command;

mod common;
use common::stage_bundle;

fn claude(config: &Path, arguments: &[&str]) -> std::process::Output {
    Command::new("claude")
        .args(arguments)
        .env("CLAUDE_CONFIG_DIR", config)
        .output()
        .expect("the claude CLI runs")
}

fn registry(config: &Path) -> serde_json::Value {
    let path = config.join("plugins/installed_plugins.json");
    serde_json::from_slice(&fs::read(path).expect("the plugin registry is readable"))
        .expect("the plugin registry parses")
}

#[test]
#[ignore = "needs a real claude CLI and mutates its plugin registry"]
fn real_claude_keeps_the_marketplace_directory_and_copies_the_plugin() {
    let directory = tempfile::tempdir().expect("temporary directory");
    // A path with a space: marketplace registration and plugin installation
    // must both survive one.
    let root = directory.path().join("a space");
    fs::create_dir_all(&root).expect("the working root");
    let config = root.join("claude config");
    fs::create_dir_all(&config).expect("the isolated Claude config directory");
    let source = stage_bundle(&root);

    let added = claude(
        &config,
        &["plugin", "marketplace", "add", source.to_str().expect("UTF-8 path")],
    );
    assert!(
        added.status.success(),
        "marketplace add failed: {}",
        String::from_utf8_lossy(&added.stderr),
    );

    let installed = claude(&config, &["plugin", "install", "appa-runtime@appa", "--scope", "user"]);
    assert!(
        installed.status.success(),
        "plugin install failed: {}",
        String::from_utf8_lossy(&installed.stderr),
    );

    // A local marketplace directory is recorded, not cloned: init can therefore
    // register an immutable deployment directory and rely on Claude reading it
    // from there.
    let marketplaces: serde_json::Value = serde_json::from_slice(
        &fs::read(config.join("plugins/marketplaces.json")).expect("the marketplace registry is readable"),
    )
    .expect("the marketplace registry parses");
    let recorded = marketplaces["marketplaces"]["appa"]["source"]["path"]
        .as_str()
        .expect("a local marketplace records its directory");
    assert_eq!(
        Path::new(recorded),
        source,
        "Claude no longer keeps a local marketplace's own directory",
    );

    // The plugin itself is copied to an installPath under Claude's cache, so
    // editing the source afterwards does not reach an installed session.
    let entry = &registry(&config)["plugins"]["appa-runtime@appa"][0];
    let install_path = entry["installPath"].as_str().expect("an installed plugin has a path");
    assert_ne!(
        Path::new(install_path),
        source.join("plugin"),
        "Claude no longer copies an installed plugin out of its marketplace",
    );
    assert!(
        Path::new(install_path).join("hooks/hooks.json").is_file(),
        "the copied plugin is missing its hook map",
    );

    // Re-installing the same version is a no-op that does not refresh the copy,
    // which is why init removes the marketplace and re-adds it rather than
    // installing over the top.
    //
    // Marking the installed copy rather than comparing timestamps: a refresh
    // that copies unchanged bytes can preserve or coarsely round the
    // modification time, and then a timestamp comparison reports a no-op it
    // never observed. Edited content survives only if nothing was copied over
    // it, whether the refresh would replace the tree or write through it.
    let marker = Path::new(install_path).join("hooks/hooks.json");
    let marked = "{\"appa-reinstall-probe\": true}\n";
    fs::write(&marker, marked).expect("the installed hook map is marked");
    let reinstalled = claude(&config, &["plugin", "install", "appa-runtime@appa", "--scope", "user"]);
    assert!(reinstalled.status.success());
    assert_eq!(
        fs::read_to_string(&marker).expect("hook map contents"),
        marked,
        "a same-version reinstall now refreshes the copy; init's remove-then-add may be unnecessary",
    );

    // Uninstall removes the registry entry, which is what init's rollback
    // relies on being able to undo.
    let uninstalled = claude(
        &config,
        &["plugin", "uninstall", "appa-runtime@appa", "--scope", "user", "--yes"],
    );
    assert!(
        uninstalled.status.success(),
        "plugin uninstall failed: {}",
        String::from_utf8_lossy(&uninstalled.stderr),
    );
    let remaining = registry(&config)["plugins"]["appa-runtime@appa"]
        .as_array()
        .map_or(0, Vec::len);
    assert_eq!(remaining, 0, "uninstall left the plugin registered");
}
