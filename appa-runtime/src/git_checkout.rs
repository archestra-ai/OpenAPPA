//! Git run in one checkout, shared by the build script, which stamps the
//! checkout's commit, and the development install, which exports it, so
//! both read the same repository.

use std::path::Path;
use std::process::Command;

/// The variables of `git rev-parse --local-env-vars` that point git at a
/// repository, or at objects, other than its working directory's; `-C` does
/// not override them. Process configuration (`GIT_CONFIG_*`, e.g. a
/// `safe.directory` the caller trusts the checkout with) and the
/// replacement-ref settings stay.
const REPOSITORY_LOCAL_GIT_VARIABLES: [&str; 10] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Git run in `root` and nowhere else, whatever repository the caller's
/// environment names.
pub(crate) fn git_in(root: &Path) -> Command {
    let mut command = Command::new("git");
    for variable in REPOSITORY_LOCAL_GIT_VARIABLES {
        command.env_remove(variable);
    }
    command.arg("-C").arg(root);
    command
}
