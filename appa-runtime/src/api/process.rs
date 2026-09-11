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

use super::{FileTracking, ProcessArgs, ProposedCall};
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

pub(super) fn perform(files: &FileTracking, call: &ProposedCall) -> Result<String, String> {
    let backend = files
        .process_backend
        .as_ref()
        .ok_or("isolated processing is not enabled")?;
    let args: ProcessArgs = serde_json::from_str(call.arguments.get()).map_err(|error| error.to_string())?;
    let job = tempfile::tempdir().map_err(|error| error.to_string())?;
    let prepare = || -> std::io::Result<()> {
        fs::create_dir(job.path().join("inputs"))?;
        fs::create_dir(job.path().join("output"))?;
        for path in &args.input_paths {
            // Paths were validated and pinned before dispatch. There are no outside writers.
            let source = files.workspace.join(path);
            let relative = source
                .strip_prefix(&files.workspace)
                .map_err(|_| std::io::Error::other("input is outside the workspace"))?;
            let target = job.path().join("inputs").join(relative);
            fs::create_dir_all(target.parent().expect("input paths have the staging parent"))?;
            fs::copy(source, target)?;
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
        return Err("isolated process failed before verified completion".into());
    }
    let response: Response =
        serde_json::from_slice(&output.stdout).map_err(|_| "isolated process returned an invalid result")?;
    let text = serde_json::to_string(&response.result).map_err(|error| error.to_string())?;
    if response.result.exit_code != 0 {
        return Err(text);
    }
    publish(
        &job.path().join("output/result"),
        &files.workspace.join(args.output_path),
    )
    .map_err(|error| format!("isolated output not published: {error}"))?;
    Ok(text)
}

fn publish(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("output must be a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other("linked output is not supported"));
        }
    }
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("missing output parent"))?;
    fs::create_dir_all(parent)?;
    let mut input = fs::File::open(source)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut input, &mut staged)?;
    staged.as_file().sync_all()?;
    staged.persist(destination).map_err(|error| error.error)?;
    Ok(())
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
        std::os::unix::fs::symlink(&input, &output).unwrap();
        assert!(publish(&output, &destination).is_err());
        fs::remove_file(&output).unwrap();
        fs::hard_link(&input, &output).unwrap();
        assert!(publish(&output, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old bytes");
        fs::remove_file(&output).unwrap();
        fs::write(&output, [0, 255, 17]).unwrap();
        publish(&output, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), [0, 255, 17]);
    }
}
