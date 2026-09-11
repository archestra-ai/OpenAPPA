use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, ffi::OsString, iter};

use appa_runtime_api::AdapterName;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "appa", version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run headless Claude with runtime-owned file tools and native tools removed.
    ClaudeFiles(appa_runtime::claude_files::Args),
    /// Internal trajectory-bound MCP server launched by claude-files.
    #[command(hide = true)]
    FileMcp(appa_runtime::claude_files::ServeArgs),
    /// Internal release identity used when activating a selected binary.
    #[command(hide = true)]
    BuildInfo,
    /// Internal noninteractive native activation after package verification.
    #[command(hide = true)]
    ActivateClaude {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        archive: PathBuf,
        #[arg(long)]
        previous_binary: Option<PathBuf>,
    },
    /// Internal journalled removal using the selected release executable.
    #[command(hide = true)]
    RemoveClaude {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        archive: PathBuf,
    },
    /// Inspect host plugin packages for a deployment.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect policy battery packages for a deployment.
    Battery {
        #[command(subcommand)]
        command: PackageCommand,
    },
    /// Export a locked deployment and its artifacts for offline installation.
    Bundle(appa_runtime::installation::cli::Bundle),
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

        #[arg(long, default_value_t = AdapterName::ClaudeCode)]
        adapter: AdapterName,

        /// Exit unsuccessfully unless the complete configuration loads.
        #[arg(long)]
        check: bool,
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

    /// Tell the OpenAPPA team that this deployment is broken, confusing, or in the way.
    #[command(long_about = "Tell the OpenAPPA team that this deployment is broken, confusing, or \
                            in the way.\n\n\
                            The report carries your message and what APPA decided — its rulings, \
                            remedies, label changes and the policy they were made under. It never \
                            carries a prompt, a tool argument, a tool output, or a path.\n\n\
                            Two questions, in this order: whether to replace the names your policy \
                            chose with report-local tokens such as `tool-1`, and whether to send \
                            the finished file, which is named before you answer. The file is kept \
                            either way.")]
    Yell {
        /// The runtime that builds the report. Loopback only.
        #[arg(long, env = "APPA_RUNTIME_URL", default_value = "http://127.0.0.1:8787")]
        url: String,

        /// Answer both questions with yes: pseudonymize the report, and send it.
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// What went wrong. Read from stdin when absent.
        message: Vec<String>,
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

#[derive(Subcommand)]
enum PackageCommand {
    /// List selected packages offline, or explicitly fetch the available catalog.
    List(appa_runtime::installation::cli::List),
    /// Add a battery's policy to this deployment in one operation.
    Install(appa_runtime::installation::cli::BatteryInstall),
    /// Remove only unchanged installer-owned battery configuration.
    Remove(appa_runtime::installation::cli::BatteryRemove),
}

#[derive(Subcommand)]
enum PluginCommand {
    /// List selected plugins offline, or explicitly fetch the available catalog.
    List(appa_runtime::installation::cli::List),
    /// Install a verified host plugin and its matching runtime.
    Install(appa_runtime::installation::cli::Install),
    /// Remove only installer-owned host support; preserve policy and data.
    Remove(appa_runtime::installation::cli::PluginRemove),
}

fn main() -> ExitCode {
    if env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("runtime")) {
        let args = iter::once(OsString::from("appa runtime")).chain(env::args_os().skip(2));
        return appa_runtime::runtime_cli::run_from(args);
    }

    let parsed = match Args::try_parse() {
        Ok(parsed) => parsed,
        Err(error) => {
            let help = matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            let marketplace = env::args_os()
                .nth(1)
                .is_some_and(|command| ["plugin", "battery", "bundle"].iter().any(|name| command == *name));
            if !help && marketplace && env::args_os().any(|argument| argument == "--json") {
                return appa_runtime::installation::cli::usage_error(&error);
            }
            let code = error.exit_code();
            if error.print().is_err() {
                return ExitCode::FAILURE;
            }
            return ExitCode::from(code as u8);
        }
    };
    match parsed.command {
        Command::BuildInfo => appa_runtime::installation::native::build_info(),
        Command::ActivateClaude {
            config,
            archive,
            previous_binary,
        } => match appa_runtime::init::activate_claude_code(&config, &archive, previous_binary.as_deref()) {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("appa: {error}");
                if matches!(error, appa_runtime::init::InitError::PluginRecovery { .. }) {
                    ExitCode::from(3)
                } else {
                    ExitCode::FAILURE
                }
            }
        },
        Command::Plugin {
            command: PluginCommand::List(args),
        } => appa_runtime::installation::cli::list(appa_package::PackageKind::Plugin, args),
        Command::Plugin {
            command: PluginCommand::Install(args),
        } => appa_runtime::installation::cli::install(args),
        Command::Plugin {
            command: PluginCommand::Remove(args),
        } => appa_runtime::installation::cli::remove_plugin(args),
        Command::RemoveClaude { config, archive } => match appa_runtime::init::claude_code_remove(&config, &archive) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("appa: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Battery {
            command: PackageCommand::List(args),
        } => appa_runtime::installation::cli::list(appa_package::PackageKind::Battery, args),
        Command::Battery {
            command: PackageCommand::Install(args),
        } => appa_runtime::installation::cli::install_battery(args),
        Command::Battery {
            command: PackageCommand::Remove(args),
        } => appa_runtime::installation::cli::remove_battery(args),
        Command::Bundle(args) => appa_runtime::installation::cli::bundle(args),
        Command::ClaudeFiles(args) => appa_runtime::claude_files::run(args),
        Command::FileMcp(args) => appa_runtime::claude_files::serve(args),
        Command::Hook { url, turn_end } => appa_runtime::hook_client::run(&url, turn_end),
        Command::Yell { url, yes, message } => appa_runtime::yell::cli::run(&url, yes, message),
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
            print!("{}", description.text);
            if check && !description.valid {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}
