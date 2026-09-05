#[path = "src/plugin_layout.rs"]
mod plugin_layout;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=APPA_PLUGIN_SHA256");
    println!("cargo:rerun-if-env-changed=APPA_RELEASE_REF");

    let crate_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let repository = crate_root.parent().expect("appa-runtime is inside the repository");
    for (source, _) in plugin_layout::REPOSITORY_MAPPINGS {
        println!("cargo:rerun-if-changed={}", repository.join(source).display());
    }
    watch_git_identity(repository);

    let release = env::var("APPA_RELEASE_REF").ok();
    let commit = git(repository, &["rev-parse", "HEAD"]);
    let dirty = plugin_is_dirty(repository);
    if release.is_some() {
        assert!(
            commit.is_some() && !dirty,
            "a release build requires a clean Git checkout"
        );
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let staged = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("plugin-build-source");
    if staged.exists() {
        fs::remove_dir_all(&staged).expect("remove the previous staged plugin identity");
    }
    let committed = out_dir.join("plugin-build-repository");
    if committed.exists() {
        fs::remove_dir_all(&committed).expect("remove the previous committed plugin source");
    }
    let identity_source = if commit.is_some() && !dirty {
        export_committed_repository(repository, &committed).expect("export the committed plugin source");
        committed.as_path()
    } else {
        repository
    };
    plugin_layout::stage_repository(identity_source, &staged).expect("stage the plugin source for build identity");
    let digest = appa_package::tree::canonical_tree_digest(&staged).expect("digest the staged plugin source");
    println!("cargo:rustc-env=APPA_PLUGIN_TREE_SHA256={}", hex(&digest));

    if let Some(reference) = release {
        assert!(!reference.trim().is_empty(), "APPA_RELEASE_REF must not be empty");
        let digest = release_plugin_digest();
        println!("cargo:rustc-env=APPA_RELEASE_REF={reference}");
        println!("cargo:rustc-env=APPA_PLUGIN_SHA256={digest}");
        return;
    }

    match (commit, dirty) {
        (Some(commit), false) => {
            println!("cargo:rustc-env=APPA_BUILD_COMMIT={commit}");
            println!("cargo:rustc-env=APPA_PLUGIN_SOURCE_KIND=commit");
        }
        _ => {
            println!("cargo:rustc-env=APPA_PLUGIN_SOURCE_KIND=local");
            println!("cargo:rustc-env=APPA_PLUGIN_SOURCE_ROOT={}", repository.display());
        }
    }
}

/// The SHA-256 of the release plugin archive this build accepts, as 64 hex
/// characters. A release binary resolves its plugin from nothing else, so a
/// release build without it is refused here rather than at someone's install.
fn release_plugin_digest() -> String {
    let Some(digest) = env::var("APPA_PLUGIN_SHA256").ok() else {
        panic!(
            "a release build (APPA_RELEASE_REF is set) requires APPA_PLUGIN_SHA256, \
             the SHA-256 of the release plugin archive"
        );
    };
    let digest = digest.trim().to_owned();
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "APPA_PLUGIN_SHA256 must be 64 hexadecimal characters, got {digest:?}"
    );
    digest
}

fn plugin_is_dirty(repository: &Path) -> bool {
    let mut arguments = vec!["status", "--porcelain=v1", "--untracked-files=all", "--"];
    arguments.extend(plugin_layout::REPOSITORY_MAPPINGS.iter().map(|(source, _)| *source));
    git(repository, &arguments).is_none_or(|output| !output.trim().is_empty())
}

fn export_committed_repository(repository: &Path, destination: &Path) -> std::io::Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .args(["archive", "--format=tar", "HEAD", "--"]);
    for (source, _) in plugin_layout::REPOSITORY_MAPPINGS {
        command.arg(source);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    fs::create_dir_all(destination)?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(output.stdout));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Git archived an unsafe path: {}", relative.display()),
            ));
        }
        let target = destination.join(&relative);
        match entry.header().entry_type() {
            // BSD tar (and therefore `git archive` on macOS) may prepend PAX
            // metadata records. They describe following entries and are not
            // files in the repository export.
            tar::EntryType::XHeader | tar::EntryType::XGlobalHeader => {}
            tar::EntryType::Directory => fs::create_dir_all(&target)?,
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = fs::File::create(target)?;
                std::io::copy(&mut entry, &mut file)?;
            }
            kind => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Git archived {} as unsupported {kind:?}", relative.display()),
                ));
            }
        }
    }
    Ok(())
}

fn git(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn watch_git_identity(repository: &Path) {
    let mut git_paths = vec!["HEAD".to_owned()];
    if let Some(reference) = git(repository, &["symbolic-ref", "-q", "HEAD"]) {
        git_paths.push(reference);
    }
    for git_path in git_paths {
        if let Some(path) = git(repository, &["rev-parse", "--git-path", &git_path]) {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                repository.join(path)
            };
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
