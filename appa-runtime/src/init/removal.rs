//! Remove only what an activation wrote to this Claude profile.

use std::fs;
use std::path::Path;

use super::{CLAPPA, InitError, appa_filename, deployment_paths, file_before, mcp, settings, skill, write_state};
use crate::installation::archive::VerifiedArchive;

#[cfg(unix)]
pub(super) const REMOVING: &str = "#!/bin/sh\nprintf 'APPA plugin removal is incomplete; rerun appa plugin remove claude-code with the same config.\\n' >&2\nexit 1\n";
#[cfg(windows)]
pub(super) const REMOVING: &str = "@echo off\r\necho APPA plugin removal is incomplete; rerun appa plugin remove claude-code with the same config. 1>&2\r\nexit /b 1\r\n";

/// The caller holds a durable removal journal. A partial failure is replayable:
/// absent registrations and files are accepted, changed user state is not
/// removed. The runtime, configuration, retained artifacts and trajectory data
/// stay put. The `--config` flag of the bridge stays because the installed
/// binary invokes it; the profile carries everything removal needs.
pub fn claude_code_remove(_config: &Path, archive: &Path) -> Result<(), InitError> {
    let paths = deployment_paths()?;
    let _profile_lock = super::lock_claude_profile(&paths.claude_dir)?;
    VerifiedArchive::of(archive)?;
    let deployed = paths.data_dir.join("bin").join(appa_filename());
    let launcher = paths.install_dir.join(CLAPPA.0);
    let launcher_before = file_before(&launcher)?;
    verify_launcher(launcher_before.as_deref(), &launcher)?;
    // Everything foreign is refused before anything is removed.
    let registered = mcp::current()?;
    skill::verify(&paths.claude_dir)?;
    settings::verify(&paths)?;

    // Disable the protected entrypoint before unregistering its hooks. A crash
    // must not leave a working-looking clappa that starts unprotected Claude.
    if launcher_before.is_some() {
        write_state(&launcher, REMOVING.as_bytes())?;
    }
    settings::remove_hooks(&paths, &deployed)?;
    if let mcp::Registered::Ours { .. } = registered {
        mcp::remove()?;
    }
    skill::remove(&paths.claude_dir)?;
    settings::remove_statusline(&paths, &deployed)?;
    if launcher_before.is_some() {
        if file_before(&launcher)?.as_deref() != Some(REMOVING.as_bytes()) {
            return Err(conflict(
                &launcher,
                "launcher changed during removal; leaving it unchanged",
            ));
        }
        fs::remove_file(&launcher).map_err(|source| InitError::WriteFile { path: launcher, source })?;
    }
    Ok(())
}

fn verify_launcher(bytes: Option<&[u8]>, path: &Path) -> Result<(), InitError> {
    if bytes.is_some_and(|bytes| !super::launcher_is_owned(bytes)) {
        return Err(conflict(
            path,
            "launcher was edited; resolve it before removing the plugin",
        ));
    }
    Ok(())
}

fn conflict(path: &Path, message: &str) -> InitError {
    InitError::NativeState {
        path: path.to_owned(),
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_accepts_only_owned_launcher_states() {
        let launcher = Path::new("clappa");
        for bytes in [None, Some(CLAPPA.1.as_bytes()), Some(REMOVING.as_bytes())] {
            verify_launcher(bytes, launcher).unwrap();
        }
        assert!(verify_launcher(Some(b"custom launcher"), launcher).is_err());
    }
}
