#[path = "src/batteries_layout.rs"]
mod batteries_layout;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=APPA_BATTERIES_SHA256");
    println!("cargo:rerun-if-env-changed=APPA_RELEASE_REF");
    println!("cargo:rerun-if-env-changed=APPA_YELL_ENDPOINT");

    let crate_root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"));
    let repository = crate_root.parent().expect("appa-runtime is inside the repository");
    println!(
        "cargo:rerun-if-changed={}",
        repository.join(batteries_layout::SOURCE).display()
    );
    watch_git_identity(repository);

    let release = env::var("APPA_RELEASE_REF").ok();
    let commit = git(repository, &["rev-parse", "HEAD"]);
    let dirty = batteries_are_dirty(repository);
    if release.is_some() {
        // Release identity covers runtime code as well as batteries mappings.
        // Recheck on incremental release builds too, including newly added files.
        println!("cargo:rerun-if-changed={}", repository.display());
        assert!(
            commit.is_some()
                && git(repository, &["status", "--porcelain=v1", "--untracked-files=all"])
                    .is_some_and(|status| status.trim().is_empty()),
            "a release build requires a clean Git checkout"
        );
    }
    // A release tag locates artifacts; the commit is the marketplace generation.
    // Emit both on release builds rather than losing the commit at the return below.
    if !dirty && let Some(commit) = &commit {
        println!("cargo:rustc-env=APPA_BUILD_COMMIT={commit}");
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let staged = out_dir.join("batteries-build-source");
    if staged.exists() {
        fs::remove_dir_all(&staged).expect("remove the previous staged batteries identity");
    }
    let committed = out_dir.join("batteries-build-repository");
    if committed.exists() {
        fs::remove_dir_all(&committed).expect("remove the previous committed batteries source");
    }
    let identity_source = if commit.is_some() && !dirty {
        export_committed_repository(repository, &committed).expect("export the committed batteries source");
        committed.as_path()
    } else {
        repository
    };
    batteries_layout::stage_repository(identity_source, &staged)
        .expect("stage the batteries source for build identity");
    let digest = appa_package::tree::canonical_tree_digest(&staged).expect("digest the staged batteries source");
    println!("cargo:rustc-env=APPA_BATTERIES_TREE_SHA256={}", hex(&digest));

    if let Some(reference) = release {
        assert!(!reference.trim().is_empty(), "APPA_RELEASE_REF must not be empty");
        let digest = release_batteries_digest();
        let endpoint = release_yell_endpoint();
        println!("cargo:rustc-env=APPA_RELEASE_REF={reference}");
        println!("cargo:rustc-env=APPA_BATTERIES_SHA256={digest}");
        println!("cargo:rustc-env=APPA_YELL_COMPILED_ENDPOINT={endpoint}");
        return;
    }

    // Emitted rather than left unset, and under a name of its own. A
    // development build carries no endpoint because this says so, not because
    // the variable happened to be absent. The name differs from the one the
    // build reads because Cargo also exports a `rustc-env` to the process under
    // `cargo run`: emitted as APPA_YELL_ENDPOINT, this empty value would
    // overwrite the runtime override a developer set in the shell.
    println!("cargo:rustc-env=APPA_YELL_COMPILED_ENDPOINT=");

    // A development build can export its own commit from the checkout that
    // built it, without the network, for as long as that checkout has it.
    println!("cargo:rustc-env=APPA_BUILD_REPOSITORY={}", repository.display());
}

/// The SHA-256 of the release batteries archive this build accepts, as 64 hex
/// characters. A release binary resolves its batteries from nothing else, so a
/// release build without it is refused here rather than at someone's install.
fn release_batteries_digest() -> String {
    let Some(digest) = env::var("APPA_BATTERIES_SHA256").ok() else {
        panic!(
            "a release build (APPA_RELEASE_REF is set) requires APPA_BATTERIES_SHA256, \
             the SHA-256 of the release batteries archive"
        );
    };
    let digest = digest.trim().to_owned();
    assert!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "APPA_BATTERIES_SHA256 must be 64 hexadecimal characters, got {digest:?}"
    );
    digest
}

/// Where a release binary sends `appa yell` reports.
///
/// A build without one resolves an empty endpoint and refuses to send, which is
/// right for a development build and silent for a released one: the feature
/// would ship inert, and the person who learns that is someone whose report
/// went nowhere. So a release build is refused here instead.
///
/// The check is the client's own, not a cheaper approximation of it. Anything
/// `Receiver::parse` refuses would ship just as inert as no endpoint at all, so
/// a prefix test here would let the gate pass and the binary stay silent —
/// `https://user:secret@host/` is a URL that starts with `https://` and that the
/// client will not send to.
fn release_yell_endpoint() -> String {
    // An unset repository variable still reaches the build as an empty string,
    // so both spellings of "missing" have to answer with the same instruction.
    let present = env::var("APPA_YELL_ENDPOINT")
        .ok()
        .map(|endpoint| endpoint.trim().to_owned())
        .filter(|endpoint| !endpoint.is_empty());
    let Some(endpoint) = present else {
        panic!(
            "a release build (APPA_RELEASE_REF is set) requires APPA_YELL_ENDPOINT, \
             the URL of the receiver that `appa yell` posts to"
        );
    };

    let parsed = url::Url::parse(&endpoint)
        .unwrap_or_else(|error| panic!("APPA_YELL_ENDPOINT must be a URL, got {endpoint:?}: {error}"));
    assert!(
        parsed.scheme() == "https",
        "APPA_YELL_ENDPOINT must be https, got {endpoint:?}"
    );
    assert!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "APPA_YELL_ENDPOINT must carry no credentials; a receiver needs none, and the host shown \
         to a person approving a send would not be the host reached"
    );
    endpoint
}

fn batteries_are_dirty(repository: &Path) -> bool {
    let arguments = [
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--",
        batteries_layout::SOURCE,
    ];
    git(repository, &arguments).is_none_or(|output| !output.trim().is_empty())
}

fn export_committed_repository(repository: &Path, destination: &Path) -> std::io::Result<()> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .args(["archive", "--format=tar", "HEAD", "--", batteries_layout::SOURCE]);
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
