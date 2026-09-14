//! `appa file-ledger` — reading, and giving back, the durable state an operator reconciles.
//!
//! A tracked workspace stops accepting file calls in two states, and both are durable on
//! purpose: a reservation is held, or a tracked path no longer holds the bytes the ledger
//! recorded for it. The runtime refuses in both rather than guess, so this command exists to
//! say which state a deployment is in. It reads the ledger directly — no runtime, no policy
//! file, no workspace argument: the ledger records the workspace it is bound to.
//!
//! `--release` gives back a reservation whose call the harness never ran, and only while the
//! workspace still shows the pinned state. A workspace that moved is never released here: the
//! runtime cannot tell an unrun call from one whose report was lost, so those bytes are an
//! operator's decision, not a command's.

use std::path::PathBuf;
use std::process::ExitCode;

use appa_eventlog::files::{AbandonOutcome, FileStore};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The file ledger to inspect: the runtime's `--file-ledger` path.
    #[arg(long, env = "APPA_FILE_LEDGER")]
    ledger: PathBuf,
    /// Give back the reservation when the workspace still shows the pinned state.
    #[arg(long)]
    release: bool,
}

pub fn run(args: Args) -> ExitCode {
    let store = match FileStore::inspect(&args.ledger) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("appa file-ledger: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("workspace: {}", store.workspace().display());

    match store.reservation() {
        Ok(None) => println!("reservation: none"),
        Ok(Some(reservation)) => {
            let pin = &reservation.pin;
            println!(
                "reservation: {} {:?} {}",
                reservation.actor, pin.operation, pin.path
            );
            if let Some(dispatch) = &reservation.bound_dispatch {
                println!("  bound dispatch: {dispatch}");
            }
            println!(
                "  pinned version: {}",
                pin.predecessor_version
                    .map_or_else(|| "absent".to_string(), |version| version.to_string())
            );
            let intact = match store.pin_matches_workspace(pin) {
                Ok(intact) => intact,
                Err(error) => {
                    eprintln!("appa file-ledger: {error}");
                    return ExitCode::FAILURE;
                }
            };
            if !intact {
                println!("  the workspace moved away from this pin; the reservation stands");
                return drifted(&store, true);
            }
            if !args.release {
                println!("  the workspace still shows the pinned state; --release gives it back");
                return drifted(&store, false);
            }
            match store.abandon(&reservation.actor, &reservation.call_key) {
                Ok(AbandonOutcome::Released | AbandonOutcome::Absent) => {
                    println!("  released: the harness never ran this call");
                }
                Ok(AbandonOutcome::Quarantined) => {
                    println!("  the workspace moved between reading and releasing; the reservation stands");
                    return drifted(&store, true);
                }
                Err(error) => {
                    eprintln!("appa file-ledger: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Err(error) => {
            eprintln!("appa file-ledger: {error}");
            return ExitCode::FAILURE;
        }
    }
    drifted(&store, false)
}

/// Report the tracked paths that no longer hold their recorded bytes. A drifted path is what
/// refuses a later call on it, and restoring those bytes is the operator's repair; a path that
/// is simply gone counts as drifted, because the ledger cannot tell a deletion from a loss.
fn drifted(store: &FileStore, failure: bool) -> ExitCode {
    let versions = match store.drifted() {
        Ok(versions) => versions,
        Err(error) => {
            eprintln!("appa file-ledger: {error}");
            return ExitCode::FAILURE;
        }
    };
    if versions.is_empty() {
        println!("drifted paths: none");
        return if failure {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    println!("drifted paths: {}", versions.len());
    for version in &versions {
        println!(
            "  {} (version {}, recorded {})",
            version.path,
            version.id,
            &version.digest[..version.digest.len().min(16)]
        );
    }
    ExitCode::FAILURE
}
