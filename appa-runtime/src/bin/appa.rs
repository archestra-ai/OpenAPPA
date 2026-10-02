use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
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
    /// Ask for one battery's token on a local page; exit when the battery is ready.
    Ui(appa_runtime::ui::Args),
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
    },
    /// Internal journalled removal using the selected release executable.
    #[command(hide = true)]
    RemoveClaude {
        #[arg(long)]
        config: PathBuf,
    },
    /// Inspect host plugin packages for a deployment.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect policy battery packages for a deployment.
    #[command(alias = "batteries")]
    Battery {
        #[command(subcommand)]
        command: PackageCommand,
    },
    /// Manage tracked files and workspace state.
    Files {
        #[command(subcommand)]
        command: FilesCommand,
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

        /// Tool names the calling session sees, comma-separated, in the host's spelling or as
        /// canonical ids. Each is reported as covered by a rule, annotated call by call, or refused.
        #[arg(long = "session-tools", value_delimiter = ',')]
        session_tools: Vec<String>,
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
        #[arg(long, env = "APPA_RUNTIME_URL", default_value = appa_runtime::runtime_url::DEFAULT_RUNTIME_URL)]
        url: String,

        /// Answer both questions with yes: pseudonymize the report, and send it.
        #[arg(short = 'y', long = "yes")]
        yes: bool,

        /// Select the intended recently active family on this machine by its root ID. For
        /// Claude Code, use `cc:<session-id>`. Confirm the ID belongs to the intended session.
        #[arg(long)]
        trajectory: Option<String>,

        /// What went wrong. Read from stdin when absent.
        message: Vec<String>,
    },

    /// Render the protected session's label for Claude Code's status line.
    #[command(hide = true)]
    Statusline {
        #[command(flatten)]
        target: appa_runtime::runtime_url::RuntimeUrl,
    },

    /// Print the advice a protected session starts with.
    #[command(hide = true)]
    SessionContext {
        /// Print it in the shape a SubagentStart hook is heard through.
        #[arg(long)]
        subagent: bool,
    },

    /// Internal host for a protected Claude Code process.
    #[command(hide = true)]
    ProtectedLaunch {
        #[arg(long)]
        settings: PathBuf,
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(last = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },

    /// Internal SessionStart and SessionEnd launch recorder.
    #[command(hide = true)]
    RecordLaunch {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(value_enum)]
        event: appa_runtime::protected_launch::Event,
    },

    /// Post one harness hook event to the running runtime.
    #[command(hide = true)]
    Hook {
        #[command(flatten)]
        target: appa_runtime::runtime_url::RuntimeUrl,

        /// Report a finished turn, whose answer never blocks the harness.
        #[arg(long)]
        turn_end: bool,

        /// Bring the deployed runtime up before posting, as the SessionStart entry does.
        #[arg(long, conflicts_with = "turn_end")]
        ensure_runtime: bool,

        /// The config the started runtime serves; the installed one when absent.
        #[arg(long, requires = "ensure_runtime")]
        config: Option<PathBuf>,

        /// Where the started runtime keeps its database and logs; the installed data directory when absent.
        #[arg(long, requires = "ensure_runtime")]
        data_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PackageCommand {
    /// Inspect credentials, dependencies, and optional read-only provider checks.
    Status(appa_runtime::ui::StatusArgs),
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

#[derive(Subcommand)]
enum FilesCommand {
    /// Repair a pending file operation from its recorded request and disk state.
    #[command(
        after_help = "Caution: Stop all processes writing to the workspace before running this command.\n\nCompares the disk state with the pending operation. If the disk reflects an expected result, the command records the file version under the request's bound Label without marking the operation successful. If the disk is incompatible with the recorded operation, the command fails and keeps the workspace reservation."
    )]
    Repair {
        /// Path to the tracked workspace.
        #[arg(long)]
        workspace: PathBuf,

        /// Installed deployment data directory; uses the platform default when absent.
        #[arg(long, env = "APPA_DATA_DIR")]
        data_dir: Option<PathBuf>,
    },

    /// Assign the configured initial Label to selected tracked files.
    #[command(
        after_help = "Caution: Relabelling overrides security state. This command records the current contents of each selected file under the configured initial Label.\n\nPrecondition: All pending file operations must be resolved before running this command."
    )]
    Relabel {
        /// Path to the tracked workspace.
        #[arg(long)]
        workspace: PathBuf,

        /// Installed deployment data directory; uses the platform default when absent.
        #[arg(long, env = "APPA_DATA_DIR")]
        data_dir: Option<PathBuf>,

        /// Paths to tracked files to record under the initial Label.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
}

fn file_store(data_dir: Option<PathBuf>) -> Result<Arc<appa_eventlog::LogStore>, String> {
    appa_runtime::runtime_start::Deployment::installed(None, data_dir)
        .map_err(|error| error.to_string())
        .and_then(|deployment| {
            appa_eventlog::LogStore::open(appa_eventlog::Backend::Sqlite {
                path: deployment.data_dir.join("appa.db"),
            })
            .map(Arc::new)
            .map_err(|error| error.to_string())
        })
}

fn repair_files(workspace: PathBuf, data_dir: Option<PathBuf>) -> ExitCode {
    let result = file_store(data_dir).and_then(|store| {
        appa_eventlog::files::FileStore::repair_workspace(store, &workspace).map_err(|error| error.to_string())
    });
    match result {
        Ok(appa_eventlog::files::RepairOutcome::Released) => {
            println!(
                "Released unchanged file reservation for workspace: {}",
                workspace.display()
            );
            ExitCode::SUCCESS
        }
        Ok(appa_eventlog::files::RepairOutcome::Repaired) => {
            println!("Repaired pending file operation in workspace: {}", workspace.display());
            ExitCode::SUCCESS
        }
        Ok(appa_eventlog::files::RepairOutcome::Quarantined) => {
            eprintln!("appa files repair: disk state matches neither the pre-execution state nor an expected result");
            ExitCode::FAILURE
        }
        Ok(appa_eventlog::files::RepairOutcome::Absent) => unreachable!("workspace repair requires a reservation"),
        Err(error) => {
            eprintln!("appa files repair: {error}");
            ExitCode::FAILURE
        }
    }
}

fn relabel_files(workspace: PathBuf, data_dir: Option<PathBuf>, paths: Vec<PathBuf>) -> ExitCode {
    let result = file_store(data_dir).and_then(|store| {
        appa_eventlog::files::FileStore::relabel_workspace(store, &workspace, &paths).map_err(|error| error.to_string())
    });
    match result {
        Ok(()) => {
            println!(
                "Relabelled selected files with the initial Label in workspace: {}",
                workspace.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("appa files relabel: {error}");
            ExitCode::FAILURE
        }
    }
}

fn claude_exit<T>(outcome: Result<T, appa_runtime::init::InitError>) -> ExitCode {
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("appa: {error}");
            match error {
                appa_runtime::init::InitError::Recovery { .. } => ExitCode::from(3),
                _ => ExitCode::FAILURE,
            }
        }
    }
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
        Command::Ui(args) => appa_runtime::ui::run(args),
        Command::Battery {
            command: PackageCommand::Status(args),
        } => appa_runtime::ui::status(args),
        Command::BuildInfo => appa_runtime::installation::native::build_info(),
        Command::ActivateClaude { config } => claude_exit(appa_runtime::init::activate_claude_code(&config)),
        Command::Plugin {
            command: PluginCommand::List(args),
        } => appa_runtime::installation::cli::list(appa_package::PackageKind::Plugin, args),
        Command::Plugin {
            command: PluginCommand::Install(args),
        } => appa_runtime::installation::cli::install(args),
        Command::Plugin {
            command: PluginCommand::Remove(args),
        } => appa_runtime::installation::cli::remove_plugin(args),
        Command::RemoveClaude { config: _ } => claude_exit(appa_runtime::init::claude_code_remove()),
        Command::Battery {
            command: PackageCommand::List(args),
        } => appa_runtime::installation::cli::list(appa_package::PackageKind::Battery, args),
        Command::Battery {
            command: PackageCommand::Install(args),
        } => appa_runtime::installation::cli::install_battery(args),
        Command::Battery {
            command: PackageCommand::Remove(args),
        } => appa_runtime::installation::cli::remove_battery(args),
        Command::Files {
            command: FilesCommand::Repair { workspace, data_dir },
        } => repair_files(workspace, data_dir),
        Command::Files {
            command:
                FilesCommand::Relabel {
                    workspace,
                    data_dir,
                    paths,
                },
        } => relabel_files(workspace, data_dir, paths),
        Command::Bundle(args) => appa_runtime::installation::cli::bundle(args),
        Command::ClaudeFiles(args) => appa_runtime::claude_files::run(args),
        Command::FileMcp(args) => appa_runtime::claude_files::serve(args),
        Command::Hook {
            target,
            turn_end,
            ensure_runtime,
            config,
            data_dir,
        } => {
            let deployment = match ensure_runtime {
                true => match appa_runtime::runtime_start::Deployment::installed(config, data_dir) {
                    Ok(deployment) => Some(deployment),
                    Err(error) => {
                        eprintln!("OpenAPPA hook blocked: {error}");
                        return ExitCode::from(2);
                    }
                },
                false => None,
            };
            appa_runtime::hook_client::run(&target.resolve(), turn_end, deployment.as_ref())
        }
        Command::Statusline { target } => appa_runtime::statusline::run(&target.resolve()),
        Command::SessionContext { subagent } => appa_runtime::session_context::run(if subagent {
            appa_runtime::session_context::Delivery::SubagentContext
        } else {
            appa_runtime::session_context::Delivery::SessionStdout
        }),
        Command::ProtectedLaunch {
            settings,
            data_dir,
            arguments,
        } => appa_runtime::protected_launch::launch(&settings, &data_dir, &arguments),
        Command::RecordLaunch { data_dir, event } => appa_runtime::protected_launch::record(&data_dir, event),
        Command::Yell {
            url,
            yes,
            trajectory,
            message,
        } => appa_runtime::yell::cli::run(&url, yes, trajectory, message),
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
            session_tools,
        } => {
            let config = config.unwrap_or_else(appa_runtime::init::installed_config_path);
            let batteries_dir = if batteries_dir.is_empty() {
                appa_runtime::batteries::default_search_path(&config)
            } else {
                batteries_dir
            };
            let description = appa_runtime::describe::render(&config, &batteries_dir, adapter, &session_tools);
            print!("{}", description.text);
            match appa_runtime::ui::describe_readiness(&config, &batteries_dir) {
                Ok(readiness) => print!("{readiness}"),
                Err(_) => eprintln!("Battery readiness: checks unavailable"),
            }
            if check && !description.valid {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}
