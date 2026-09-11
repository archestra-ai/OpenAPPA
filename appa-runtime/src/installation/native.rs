//! Executables are selected with their batteries archive, never independently.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use appa_package::generation::{ArtifactDigest, Artifacts, Generation, Platform};
use serde::{Deserialize, Serialize};

use super::{InstallError, Installation, acquisition, io, open_regular, require_directory_or_absent, sync_directory};

const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::NativeChild;
#[cfg(all(test, windows))]
mod windows_tests;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryIdentity {
    schema: u32,
    protocol: u32,
    commit: Option<String>,
    release: Option<String>,
    batteries_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batteries_tree_sha256: Option<String>,
}

pub fn build_info() -> ExitCode {
    let identity = BinaryIdentity {
        schema: 1,
        protocol: appa_package::PROTOCOL,
        commit: option_env!("APPA_BUILD_COMMIT").map(str::to_owned),
        release: option_env!("APPA_RELEASE_REF").map(str::to_owned),
        batteries_sha256: option_env!("APPA_BATTERIES_SHA256").map(str::to_owned),
        batteries_tree_sha256: option_env!("APPA_BATTERIES_TREE_SHA256").map(str::to_owned),
    };
    let mut output = std::io::stdout().lock();
    match serde_json::to_writer(&mut output, &identity)
        .map_err(std::io::Error::other)
        .and_then(|()| writeln!(output))
        .and_then(|()| output.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

pub struct ClaudeArtifacts {
    binary: PathBuf,
}

impl ClaudeArtifacts {
    /// Uses only retained, rehashed archives. It does not write the Claude
    /// profile, start a runtime, or change the active config.
    pub fn prepare(
        installation: &Installation,
        generation: &Generation,
        platform: Platform,
    ) -> Result<Self, InstallError> {
        if Platform::current() != Some(platform) {
            return Err(InstallError::Invalid(
                "the selected Claude binary does not target this machine".into(),
            ));
        }
        let archives = generation.archives();
        let binary_digest = archives
            .get(platform.archive())
            .ok_or_else(|| InstallError::Invalid("the installed version has no executable for this platform".into()))?;
        let batteries_digest = &archives[&generation.batteries_archive()];
        let binary_archive = installation.state.join("artifacts").join(binary_digest.hex());
        let batteries_archive = installation.state.join("artifacts").join(batteries_digest.hex());
        acquisition::verify_artifact(&binary_archive, binary_digest)?;
        acquisition::verify_artifact(&batteries_archive, batteries_digest)?;
        let directory = installation.state.join("native");
        require_directory_or_absent(&directory)?;
        fs::create_dir_all(&directory).map_err(|error| io("create native cache", &directory, error))?;
        let stage = tempfile::tempdir_in(&directory).map_err(|error| io("stage native binary", &directory, error))?;
        let binary_name = if matches!(platform, Platform::WindowsAmd64 | Platform::WindowsArm64) {
            "appa.exe"
        } else {
            "appa"
        };
        if binary_name == "appa.exe" {
            extract_windows_binary(&binary_archive, &stage.path().join(binary_name))?;
        } else {
            super::archive::extract_bundle_archive(&binary_archive, stage.path())
                .map_err(|error| InstallError::Invalid(error.to_string()))?;
        }
        let entries: Vec<_> = fs::read_dir(stage.path())
            .map_err(|error| io("inspect binary archive", stage.path(), error))?
            .collect::<Result<_, _>>()
            .map_err(|error| io("inspect binary archive", stage.path(), error))?;
        if entries.len() != 1 || entries[0].file_name() != binary_name {
            return Err(InstallError::Invalid(
                "runtime archive must contain exactly its APPA executable".into(),
            ));
        }
        let staged_binary = stage.path().join(binary_name);
        let expected = ArtifactDigest::of_reader(open_regular(&staged_binary)?, MAX_BINARY_BYTES)
            .map_err(|error| io("hash selected binary", &staged_binary, error))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&staged_binary, fs::Permissions::from_mode(0o755))
                .map_err(|error| io("make selected binary executable", &staged_binary, error))?;
        }
        let identity_output = invoke(&staged_binary, &["build-info".as_ref()], Duration::from_secs(15))?;
        let identity: BinaryIdentity = serde_json::from_slice(&identity_output).map_err(|error| {
            InstallError::Invalid(format!("selected binary returned invalid build identity: {error}"))
        })?;
        let same_artifacts = match generation.artifacts() {
            Artifacts::Published(published) => {
                identity.release.as_deref() == Some(published.release())
                    && identity.batteries_sha256.as_deref() == Some(batteries_digest.hex())
            }
            Artifacts::Build(build) => {
                identity.release.is_none()
                    && identity.batteries_sha256.is_none()
                    && identity.batteries_tree_sha256.as_deref() == Some(build.batteries_tree())
            }
        };
        if identity.schema != 1
            || identity.protocol != appa_package::PROTOCOL
            || identity.commit.as_deref() != Some(generation.commit().as_str())
            || !same_artifacts
        {
            return Err(InstallError::Invalid(
                "the selected runtime and batteries archive do not belong to the same version".into(),
            ));
        }
        let destination = directory.join(binary_digest.hex());
        require_directory_or_absent(&destination)?;
        if destination.exists() {
            acquisition::verify_artifact(&destination.join(binary_name), &expected)?;
        } else {
            File::open(&staged_binary)
                .and_then(|file| file.sync_all())
                .map_err(|error| io("sync binary", &staged_binary, error))?;
            sync_directory(stage.path())?;
            fs::rename(stage.path(), &destination)
                .map_err(|error| io("publish selected binary", &destination, error))?;
            sync_directory(&directory)?;
        }
        Ok(Self {
            binary: destination.join(binary_name),
        })
    }

    /// Caller holds the installation lock and durable activation journal.
    pub fn activate(&self, config: &Path, previous: Option<&Self>) -> Result<(), InstallError> {
        let mut arguments = vec!["activate-claude".as_ref(), "--config".as_ref(), config.as_os_str()];
        if let Some(previous) = previous {
            arguments.extend(["--previous-binary".as_ref(), previous.binary.as_os_str()]);
        }
        invoke(&self.binary, &arguments, Duration::from_secs(120))?;
        Ok(())
    }

    pub fn remove(&self, config: &Path) -> Result<(), InstallError> {
        invoke(
            &self.binary,
            &["remove-claude".as_ref(), "--config".as_ref(), config.as_os_str()],
            Duration::from_secs(120),
        )?;
        Ok(())
    }
}

fn extract_windows_binary(archive: &Path, target: &Path) -> Result<(), InstallError> {
    let mut archive = zip::ZipArchive::new(open_regular(archive)?)
        .map_err(|error| InstallError::Invalid(format!("invalid runtime ZIP: {error}")))?;
    if archive.len() != 1 {
        return Err(InstallError::Invalid(
            "runtime ZIP must contain exactly appa.exe".into(),
        ));
    }
    let mut entry = archive
        .by_index(0)
        .map_err(|error| InstallError::Invalid(error.to_string()))?;
    if entry.name() != "appa.exe"
        || !entry.is_file()
        || entry.size() > MAX_BINARY_BYTES
        || entry
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000))
    {
        return Err(InstallError::Invalid(
            "runtime ZIP contains an unsafe or oversized executable".into(),
        ));
    }
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(target)
        .map_err(|error| io("create selected binary", target, error))?;
    let written = std::io::copy(&mut entry.by_ref().take(MAX_BINARY_BYTES + 1), &mut output)
        .map_err(|error| io("extract selected binary", target, error))?;
    if written > MAX_BINARY_BYTES {
        return Err(InstallError::Invalid("runtime ZIP exceeds its byte limit".into()));
    }
    Ok(())
}

#[cfg(not(windows))]
struct NativeChild {
    child: std::process::Child,
    completed: bool,
}

#[cfg(not(windows))]
impl NativeChild {
    fn terminate(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            // Only this invocation and its descendants belong to this group.
            let result = unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(std::io::Error::last_os_error());
            }
        }
        self.child.wait()?;
        self.completed = true;
        Ok(())
    }
}

#[cfg(not(windows))]
impl Drop for NativeChild {
    fn drop(&mut self) {
        if !self.completed
            && let Err(error) = self.terminate()
        {
            tracing::error!(%error, "native activation process cleanup failed");
        }
    }
}

#[cfg(not(windows))]
fn spawn(
    binary: &Path,
    arguments: &[&std::ffi::OsStr],
    stdout: &File,
    stderr: &File,
) -> Result<NativeChild, InstallError> {
    let mut command = Command::new(binary);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(
            stdout
                .try_clone()
                .map_err(|error| io("capture native result", binary, error))?,
        )
        .stderr(
            stderr
                .try_clone()
                .map_err(|error| io("capture native diagnostics", binary, error))?,
        );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Ok(NativeChild {
        child: command
            .spawn()
            .map_err(|error| io("run selected binary", binary, error))?,
        completed: false,
    })
}

/// The one line of a failed activation's stderr that says why: its last
/// diagnostic, with the `appa: ` prefix and the progress lines (which end in
/// `...`) left out. A child that said nothing is reported by its exit status.
fn failure_cause(stderr: &str, status: std::process::ExitStatus) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty() && !line.ends_with("..."))
        .map(|line| line.strip_prefix("appa: ").unwrap_or(line).to_owned())
        .map(|line| if line.ends_with('.') { line } else { format!("{line}.") })
        .unwrap_or_else(|| format!("the selected binary exited with {status}."))
}

fn invoke(binary: &Path, arguments: &[&std::ffi::OsStr], timeout: Duration) -> Result<Vec<u8>, InstallError> {
    let mut stdout = tempfile::tempfile().map_err(|error| io("capture native result", binary, error))?;
    let mut stderr = tempfile::tempfile().map_err(|error| io("capture native diagnostics", binary, error))?;
    #[cfg(not(windows))]
    let mut process = spawn(binary, arguments, &stdout, &stderr)?;
    #[cfg(windows)]
    let mut process = NativeChild::spawn(binary, arguments, &stdout, &stderr)
        .map_err(|error| io("run selected binary", binary, error))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if stdout
            .metadata()
            .map_err(|error| io("inspect native output", binary, error))?
            .len()
            > 65536
            || stderr
                .metadata()
                .map_err(|error| io("inspect native diagnostics", binary, error))?
                .len()
                > 65536
        {
            process
                .terminate()
                .map_err(|error| io("stop noisy native command", binary, error))?;
            return Err(InstallError::Recovery {
                path: binary.to_owned(),
                reason: "selected binary output exceeded its byte limit; owned subprocesses were stopped".into(),
            });
        }
        #[cfg(not(windows))]
        let status = process.child.try_wait();
        #[cfg(windows)]
        let status = process.try_wait();
        if let Some(status) = status.map_err(|error| io("wait for selected binary", binary, error))? {
            if !status.success() {
                process
                    .terminate()
                    .map_err(|error| io("stop failed native command", binary, error))?;
            }
            #[cfg(not(windows))]
            {
                process.completed = true;
            }
            break status;
        }
        if Instant::now() >= deadline {
            let killed = process.terminate();
            return Err(InstallError::Recovery {
                path: binary.to_owned(),
                reason: format!(
                    "selected binary timed out; activation state must be checked (child cleanup: {killed:?})"
                ),
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    use std::io::{Seek, SeekFrom};
    stdout
        .seek(SeekFrom::Start(0))
        .map_err(|error| io("read native result", binary, error))?;
    stderr
        .seek(SeekFrom::Start(0))
        .map_err(|error| io("read native diagnostics", binary, error))?;
    let mut output = Vec::new();
    stdout
        .take(65537)
        .read_to_end(&mut output)
        .map_err(|error| io("read native result", binary, error))?;
    if output.len() > 65536 {
        return Err(InstallError::Invalid(
            "selected binary result exceeds its byte limit".into(),
        ));
    }
    if !status.success() {
        let mut message = String::new();
        stderr
            .take(65536)
            .read_to_string(&mut message)
            .map_err(|error| io("read native diagnostics", binary, error))?;
        return Err(InstallError::Recovery {
            path: binary.to_owned(),
            reason: format!("activation failed: {}", failure_cause(&message, status)),
        });
    }
    #[cfg(windows)]
    {
        // Descendants may include the runtime. Release them only after the
        // successful command's captured result passes validation.
        if stderr
            .metadata()
            .map_err(|error| io("inspect native diagnostics", binary, error))?
            .len()
            > 65536
        {
            return Err(InstallError::Recovery {
                path: binary.to_owned(),
                reason: "selected binary diagnostics exceeded their byte limit".into(),
            });
        }
        process.complete().map_err(|error| InstallError::Recovery {
            path: binary.to_owned(),
            reason: format!("could not release the successful native command's runtime: {error}"),
        })?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_binary_archive_accepts_one_executable_and_refuses_other_paths() {
        for name in ["appa.exe", "../appa.exe", "nested/appa.exe", "other.exe"] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("binary.zip");
            let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(b"verified executable bytes").unwrap();
            zip.finish().unwrap();
            let target = root.path().join("appa.exe");
            let result = extract_windows_binary(&archive, &target);
            assert_eq!(result.is_ok(), name == "appa.exe");
            if result.is_ok() {
                assert_eq!(fs::read(target).unwrap(), b"verified executable bytes");
            } else {
                assert!(!target.exists());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn native_subprocess_timeout_and_output_limit_are_bounded() {
        let start = Instant::now();
        let timeout = invoke(Path::new("/bin/sleep"), &["30".as_ref()], Duration::from_millis(50));
        assert!(matches!(timeout, Err(InstallError::Recovery { .. })));
        assert!(start.elapsed() < Duration::from_secs(2));
        let start = Instant::now();
        let noisy = invoke(Path::new("/usr/bin/yes"), &[], Duration::from_secs(5));
        assert!(matches!(noisy, Err(InstallError::Recovery { .. })));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
