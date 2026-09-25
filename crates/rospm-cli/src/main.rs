mod commands;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgAction, Parser, Subcommand};
use rospm_core::manifest::RosFlavor;
use rospm_core::mirror::Mirror;
use rospm_core::platform::Platform;
use rospm_core::provider::Shell;

use commands::Global;

#[derive(Parser)]
#[command(
    name = "rospm",
    version,
    about = "Describe, lock and replay ROS 2 project environments",
    long_about = "rospm wraps rosdep, vcstool, colcon, apt, pixi and docker behind one \
                  manifest (rospm.toml) and one lock file (rospm.lock).\n\n\
                  Commands that modify the system (apply, uninstall, build, export, import, \
                  deps install) always print their plan first and accept --dry-run."
)]
struct Cli {
    /// Path to rospm.toml (default: search upwards from the current directory)
    #[arg(long, global = true, value_name = "PATH")]
    manifest: Option<PathBuf>,

    /// Machine-readable JSON output where supported
    #[arg(long, global = true)]
    json: bool,

    /// Increase log verbosity (-v, -vv)
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a rospm.toml template in the current directory
    Init {
        /// Draft the manifest from this host and directory (best effort;
        /// undetected fields are marked TODO)
        #[arg(long)]
        from_env: bool,
        /// Which ROS metapackage to install: `ros-base` (bare bones, default)
        /// or `desktop` (adds RViz, demos, tutorials)
        #[arg(long, value_name = "FLAVOR", value_parser = ["ros-base", "desktop"])]
        base: Option<String>,
        /// Overwrite an existing rospm.toml
        #[arg(long)]
        force: bool,
    },

    /// Validate the manifest and compare it with this host (read-only)
    Check {
        /// Evaluate platform overrides for this target instead of the host
        #[arg(long, value_name = "PLATFORM")]
        target: Option<Platform>,
        /// Exit non-zero when the host does not match the manifest
        #[arg(long)]
        strict: bool,
    },

    /// Resolve and write rospm.lock (roadmap P2)
    Lock {
        #[arg(long, value_name = "PLATFORM")]
        platform: Option<Platform>,
    },

    /// Compare rospm.lock with the current environment (roadmap P2)
    Verify,

    /// Set up the ROS apt source, install the ROS base, import sources and
    /// resolve dependencies [modifies system]
    Apply {
        /// Honour rospm.lock (snapshot repositories, pinned revisions)
        #[arg(long)]
        locked: bool,
        /// Use only bundled payloads, never the network
        #[arg(long)]
        offline: bool,
        /// Download ROS from this source set instead of probing and asking:
        /// official | tuna | ustc | aliyun (also `ROSPM_MIRROR`)
        #[arg(long, value_name = "NAME", value_parser = clap::builder::PossibleValuesParser::new(rospm_core::mirror::NAMES))]
        mirror: Option<String>,
        /// Print the plan and exit
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove the ROS installation described by the manifest; project files
    /// and workspaces are kept [modifies system]
    Uninstall {
        /// Leave the ROS apt source and keyring in place
        #[arg(long)]
        keep_source: bool,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
        /// Print the plan and exit
        #[arg(long)]
        dry_run: bool,
    },

    /// Build workspaces with colcon in overlay order [produces artifacts]
    Build {
        /// Build only this workspace (its underlays must already be built)
        workspace: Option<String>,
        /// Print the plan and exit
        #[arg(long)]
        dry_run: bool,
    },

    /// Print a script that enters the project environment (`source <(rospm env)`)
    Env {
        #[arg(long, default_value = "bash")]
        shell: Shell,
    },

    /// Start a subshell with the project environment loaded
    Shell,

    /// Inspect or complete dependencies from package.xml files
    Deps {
        #[command(subcommand)]
        command: DepsCmd,
    },

    /// Produce an offline bundle (roadmap P6) [produces artifacts]
    Export {
        #[arg(long, value_name = "PLATFORM")]
        target: Option<Platform>,
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },

    /// Verify and install an offline bundle (roadmap P6) [modifies system]
    Import {
        bundle: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },

    /// Generate editor configuration (roadmap P7)
    Editor {
        /// vscode | clion | devcontainer (default: all enabled in the manifest)
        kind: Option<String>,
    },

    /// Check that the wrapped tools are available on this machine and
    /// whether the ROS sources (official and mirrors) are reachable
    Doctor {
        /// Skip the network probe
        #[arg(long)]
        no_network: bool,
    },
}

#[derive(Subcommand)]
enum DepsCmd {
    /// Report missing dependencies without installing (roadmap P2)
    Check,
    /// Install missing dependencies through rosdep (roadmap P2) [modifies system]
    Install {
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let global = Global {
        manifest: cli.manifest,
        json: cli.json,
    };

    let result = match cli.command {
        Cmd::Init {
            from_env,
            base,
            force,
        } => {
            let base = base
                .as_deref()
                .map(|b| b.parse::<RosFlavor>().expect("validated by clap"));
            commands::init(&global, from_env, base, force)
        }
        Cmd::Check { target, strict } => commands::check(&global, target, strict),
        Cmd::Lock { .. } => commands::not_implemented("lock", "P2"),
        Cmd::Verify => commands::not_implemented("verify", "P2"),
        Cmd::Apply {
            locked,
            offline,
            mirror,
            dry_run,
        } => {
            let mirror = mirror
                .as_deref()
                .map(|m| m.parse::<Mirror>().expect("validated by clap"));
            commands::apply(&global, locked, offline, dry_run, mirror)
        }
        Cmd::Uninstall {
            keep_source,
            yes,
            dry_run,
        } => commands::uninstall(&global, keep_source, yes, dry_run),
        Cmd::Build { workspace, dry_run } => {
            commands::build(&global, workspace.as_deref(), dry_run)
        }
        Cmd::Env { shell } => commands::env(&global, shell),
        Cmd::Shell => commands::shell(&global),
        Cmd::Deps { command } => match command {
            DepsCmd::Check => commands::not_implemented("deps check", "P2"),
            DepsCmd::Install { .. } => commands::not_implemented("deps install", "P2"),
        },
        Cmd::Export { .. } => commands::not_implemented("export", "P6"),
        Cmd::Import { .. } => commands::not_implemented("import", "P6"),
        Cmd::Editor { .. } => commands::not_implemented("editor", "P7"),
        Cmd::Doctor { no_network } => commands::doctor(&global, no_network),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if global.json {
                let payload = serde_json::json!({ "ok": false, "error": format!("{err:#}") });
                println!("{payload}");
            } else {
                eprintln!("error: {err:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn init_logging(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}
