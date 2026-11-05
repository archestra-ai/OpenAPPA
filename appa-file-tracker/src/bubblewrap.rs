//! SafeBash's offline backend on Linux: one command under bubblewrap, for a call that declares
//! no host. The command runs in fresh namespaces with no network, no capabilities and an empty
//! environment. It sees the system directories read-only, a private `/tmp`, a read-only copy of
//! each input at the input's own path, and an empty file at each output's path. The user's
//! home, working directory and temporary files are not mounted, and a recorded file under a
//! system directory reads as empty. Only when the command exits 0 are its output files
//! written back, so a failed command leaves every declared output as it was.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::paths::Named;
use crate::safebash::supervise;
use crate::store::Locked;

/// Directories mounted read-only in every sandbox, where they exist.
const SYSTEM: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/etc"];

/// The largest output file written back.
const MAX_OUTPUT_BYTES: u64 = 64 << 20;

pub fn run(bwrap: &Path, config: &Config, command: &str, named: &Named) -> Result<String, String> {
    let job = std::env::temp_dir().join(format!("appa-safebash-{}-{}", std::process::id(), crate::store::now()));
    fs::create_dir(&job).map_err(|error| format!("{}: {error}", job.display()))?;
    let outcome = run_in(&job, bwrap, config, command, named);
    let _ = fs::remove_dir_all(&job);
    outcome
}

fn run_in(job: &Path, bwrap: &Path, config: &Config, command: &str, named: &Named) -> Result<String, String> {
    let mut mounted: Vec<PathBuf> = SYSTEM.iter().map(PathBuf::from).collect();
    mounted.extend(config.sandbox.readable.iter().cloned());
    let mut sandbox = Command::new(bwrap);
    sandbox.args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
    ]);
    sandbox.args([
        "--setenv",
        "PATH",
        &std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    ]);
    sandbox.args([
        "--setenv", "HOME", "/tmp", "--setenv", "TMPDIR", "/tmp", "--setenv", "LANG", "C.UTF-8",
    ]);
    for path in &mounted {
        sandbox.arg("--ro-bind-try").arg(path).arg(path);
    }
    sandbox.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);

    // A file the store holds a label for, or the store itself, is empty wherever a mount
    // above would otherwise show it, unless the call declares it.
    let recorded: Vec<PathBuf> = {
        let locked = Locked::open(&config.store)?;
        locked.store.files.keys().cloned().collect()
    };
    for path in recorded.iter().chain([&config.store]) {
        let declared = named.reads.contains(path) || named.writes.contains(path);
        if !declared && path.is_file() && mounted.iter().any(|root| path.starts_with(root)) {
            sandbox.arg("--ro-bind").arg("/dev/null").arg(path);
        }
    }

    // The working directory exists, empty, unless a mount above already shows it; declared
    // files appear in it at their own paths, so relative paths to them still work.
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    if !mounted.iter().any(|root| cwd.starts_with(root)) {
        sandbox.arg("--dir").arg(&cwd);
    }
    for (at, input) in named.reads.iter().enumerate() {
        let copy = job.join(format!("input-{at}"));
        fs::copy(input, &copy).map_err(|error| format!("{}: {error}", input.display()))?;
        sandbox.arg("--ro-bind").arg(&copy).arg(input);
    }
    let mut staged = Vec::new();
    for (at, output) in named.writes.iter().enumerate() {
        let stage = job.join(format!("output-{at}"));
        fs::File::create(&stage).map_err(|error| format!("{}: {error}", stage.display()))?;
        sandbox.arg("--bind").arg(&stage).arg(output);
        staged.push((stage, output));
    }
    sandbox.arg("--chdir").arg(&cwd).args(["bash", "-c", command]);

    let timeout = config.sandbox.timeout_secs;
    let finished = supervise(sandbox, timeout)?;
    if !finished.succeeded() {
        let note = if staged.is_empty() {
            ""
        } else {
            "\n--- output files ---\nnot written: the command did not exit 0"
        };
        return Ok(finished.report(timeout, note));
    }
    for (stage, output) in &staged {
        write_back(stage, output).map_err(|error| format!("output {} not written: {error}", output.display()))?;
    }
    Ok(finished.report(timeout, ""))
}

/// Copy one staged output over its destination, in place, so the destination keeps its
/// permissions. Only a regular, singly linked file within the ceiling is written back.
fn write_back(stage: &Path, output: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(stage)?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("the command did not leave a regular file"));
    }
    if metadata.len() > MAX_OUTPUT_BYTES {
        return Err(std::io::Error::other("the output exceeds 64 MiB"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other("the command linked the output elsewhere"));
        }
    }
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;
    std::io::copy(&mut fs::File::open(stage)?, &mut destination)?;
    destination.flush()
}
