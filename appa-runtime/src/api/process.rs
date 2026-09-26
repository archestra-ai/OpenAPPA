//! Declared-input subprocess execution. The host installs the pinned agentsh backend.
//!
//! The ledger reserves inputs and destination before this module runs. Only input snapshots
//! enter the child workspace. The runner closes inherited descriptors, clears the environment,
//! and uses bubblewrap namespaces plus agentsh Landlock and seccomp restrictions. The host's
//! system toolchain is trusted, public input. The live workspace, ledger and credentials are
//! never mounted. Every result, including failures, carries all declared input Labels.
//!
//! Publication waits for namespace PID 1 to exit and its descendants to be torn down. Import
//! accepts only a regular, singly linked output file, never child-supplied paths or metadata.
//! Native Claude tools, inference traffic, resource exhaustion and timing flows remain outside
//! this subprocess contract. No sanitizer or declassification occurs here.

use super::{FileTracking, ProcessArgs, ProposedCall, existing};
use crate::file_ledger::beneath::Entry;
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(serde::Deserialize, serde::Serialize)]
struct ProcessResult {
    exit_code: i32,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
}

#[derive(serde::Deserialize)]
struct Response {
    result: ProcessResult,
}

pub(super) fn perform(
    files: &FileTracking,
    workspace: &Path,
    call: &ProposedCall,
    pin: &crate::file_ledger::FilePin,
) -> Result<String, String> {
    let crate::file_ledger::PinnedBasis::Process { inputs, .. } = &pin.basis else {
        return Err("the file reservation is not a process".into());
    };
    let backend = files
        .process_backend
        .as_ref()
        .ok_or("isolated processing is not enabled")?;
    let args: ProcessArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
    let job = tempfile::tempdir().map_err(|error| error.to_string())?;
    let prepare = || -> std::io::Result<()> {
        fs::create_dir(job.path().join("inputs"))?;
        fs::create_dir(job.path().join("output"))?;
        for input in inputs {
            let target = job.path().join("inputs").join(&input.path);
            fs::create_dir_all(target.parent().expect("input paths have the staging parent"))?;
            std::io::copy(&mut existing(workspace, &input.path)?, &mut fs::File::create(target)?)?;
        }
        fs::write(
            job.path().join("request.json"),
            serde_json::to_vec(&serde_json::json!({"command": args.command}))?,
        )?;
        Ok(())
    };
    prepare().map_err(|error| error.to_string())?;
    let output = Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(backend.join("run.py"))
        .arg(backend)
        .arg(job.path())
        .env_clear()
        .output()
        .map_err(|_| "isolated process launcher failed")?;
    if !output.status.success() {
        // The launcher's own diagnostics: the operator's, not the model's. The message the
        // model sees stays generic, because a launcher failure is not about its content.
        tracing::warn!(
            status = ?output.status.code(),
            stderr = %String::from_utf8_lossy(&output.stderr).trim(),
            "the isolated process launcher failed"
        );
        return Err("isolated process failed before verified completion".into());
    }
    let response: Response =
        serde_json::from_slice(&output.stdout).map_err(|_| "isolated process returned an invalid result")?;
    let text = serde_json::to_string(&response.result).map_err(|error| error.to_string())?;
    if response.result.exit_code != 0 {
        return Err(text);
    }
    Entry::create(workspace, &pin.path)
        .and_then(|destination| publish(&job.path().join("output/result"), &destination, MAX_OUTPUT_BYTES))
        .map_err(|error| format!("isolated output not published: {error}"))?;
    Ok(text)
}

/// The largest isolated output the runtime imports. The runner caps one file at 64 MiB through
/// RLIMIT_FSIZE, which is the same ceiling; this refuses the bytes before they are copied.
const MAX_OUTPUT_BYTES: u64 = 64 << 20;

fn publish(source: &Path, destination: &Entry, limit: u64) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("output must be a regular file"));
    }
    if metadata.len() > limit {
        return Err(std::io::Error::other("output exceeds the import limit"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other("linked output is not supported"));
        }
    }
    destination.publish(&mut fs::File::open(source)?)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn process_import_rejects_links_and_preserves_replaced_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input");
        let output = dir.path().join("output");
        let destination = dir.path().join("destination");
        fs::write(&input, b"private bytes").unwrap();
        fs::write(&destination, b"old bytes").unwrap();
        let target = Entry::create(dir.path(), "destination").unwrap();
        std::os::unix::fs::symlink(&input, &output).unwrap();
        assert!(publish(&output, &target, MAX_OUTPUT_BYTES).is_err());
        fs::remove_file(&output).unwrap();
        fs::hard_link(&input, &output).unwrap();
        assert!(publish(&output, &target, MAX_OUTPUT_BYTES).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old bytes");
        fs::remove_file(&output).unwrap();
        // The ceiling is refused before any byte is copied, and the destination is untouched.
        fs::write(&output, b"four").unwrap();
        assert!(publish(&output, &target, 3).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old bytes");
        assert!(publish(&output, &target, 4).is_ok());
        assert_eq!(fs::read(&destination).unwrap(), b"four");
        fs::write(&output, [0, 255, 17]).unwrap();
        publish(&output, &target, MAX_OUTPUT_BYTES).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), [0, 255, 17]);
    }
}
