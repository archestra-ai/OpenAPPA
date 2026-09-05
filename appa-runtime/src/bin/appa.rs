use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, ffi::OsString, iter};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "appa", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Describe the configuration facts available to a human or configuring agent.
    Describe {
        #[arg(long, env = "APPA_CONFIG")]
        config: Option<PathBuf>,

        #[arg(
            long = "batteries-dir",
            env = "APPA_BATTERIES_DIR",
            value_delimiter = ':',
            action = clap::ArgAction::Append
        )]
        batteries_dir: Vec<PathBuf>,

        #[arg(long, value_enum, default_value_t = Adapter::ClaudeCode)]
        adapter: Adapter,

        /// Exit unsuccessfully unless the complete configuration loads.
        #[arg(long)]
        check: bool,
    },

    /// Initialize OpenAPPA for an agent harness.
    Init {
        #[command(subcommand)]
        harness: Harness,
    },

    /// Replay trace files against a policy and check every expectation.
    Replay {
        #[arg(long, env = "APPA_CONFIG")]
        config: Option<PathBuf>,

        #[arg(long, env = "APPA_MODULES_DIR")]
        modules_dir: Option<PathBuf>,

        /// Print every step, not only the ones that failed.
        #[arg(short, long)]
        verbose: bool,

        /// Trace files, or directories holding `.appa` files.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },

    /// Post one harness hook event to the running runtime.
    #[command(hide = true)]
    Hook {
        #[arg(long, env = "APPA_RUNTIME_URL", default_value = "http://127.0.0.1:8787")]
        url: String,

        /// Report a finished turn, whose answer never blocks the harness.
        #[arg(long)]
        turn_end: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Adapter {
    ClaudeCode,
    Kagent,
}

impl Adapter {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Kagent => "kagent",
        }
    }
}

#[derive(Subcommand)]
enum Harness {
    /// Install this build's Claude Code plugin and initialize its local deployment.
    ClaudeCode {
        /// Developer override: a staged marketplace root to deploy.
        #[arg(long, hide = true)]
        plugin_source: Option<String>,
    },
}

fn main() -> ExitCode {
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("runtime")) {
        let args = iter::once(OsString::from("appa runtime")).chain(env::args_os().skip(2));
        return appa_runtime::runtime_cli::run_from(args);
    }

    match Args::parse().command {
        Command::Hook { url, turn_end } => appa_runtime::hook_client::run(&url, turn_end),
        Command::Replay {
            config,
            modules_dir,
            verbose,
            paths,
        } => {
            let config = config.unwrap_or_else(appa_runtime::init::installed_config_path);
            appa_runtime::replay::main(&config, modules_dir, verbose, &paths)
        }
        Command::Describe {
            config,
            batteries_dir,
            adapter,
            check,
        } => {
            let config = config.unwrap_or_else(appa_runtime::init::installed_config_path);
            let description = appa_runtime::describe::render(&config, &batteries_dir, adapter.as_str());
            print!("{description}");
            if check && !appa_runtime::describe::is_loadable(&config, &batteries_dir) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::Init {
            harness: Harness::ClaudeCode { plugin_source },
        } => match appa_runtime::init::claude_code(plugin_source.as_deref()) {
            Ok(description) => {
                print!("{description}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("appa: {error}");
                ExitCode::FAILURE
            }
        },
    }
}
