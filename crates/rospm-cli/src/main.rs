mod build_summary;
mod commands;
mod i18n;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgAction, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use rospm_core::manifest::RosFlavor;
use rospm_core::mirror::Mirror;
use rospm_core::platform::{Platform, RosDistro};
use rospm_core::provider::Shell;

use commands::Global;
use i18n::tr;

// Translation resources, compiled in (spec §4.2). `en` is the fallback.
rust_i18n::i18n!("locales", fallback = "en");

// Descriptions are looked up at build time of the clap command, i.e. after
// `i18n::install`, so `--help` follows the selected language. The auto
// help/version flags are replaced so their descriptions are translated too.
#[derive(Parser)]
#[command(
    name = "rospm",
    version,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    about = tr("cli.about"),
    long_about = tr("cli.long_about")
)]
struct Cli {
    #[arg(long, global = true, value_name = "PATH", help = tr("cli.manifest"))]
    manifest: Option<PathBuf>,

    #[arg(long, global = true, help = tr("cli.json"))]
    json: bool,

    #[arg(short, long, global = true, action = ArgAction::Count, help = tr("cli.verbose"))]
    verbose: u8,

    // Read before parsing (see `i18n::detect`); declared so clap accepts it.
    #[arg(long, global = true, value_name = "LANG", value_parser = ["zh", "en"], hide_possible_values = true, help = tr("cli.lang"))]
    lang: Option<String>,

    #[arg(short = 'h', long, global = true, action = ArgAction::Help, help = tr("cli.help_flag"))]
    help: Option<bool>,

    #[arg(short = 'V', long, action = ArgAction::Version, help = tr("cli.version_flag"))]
    version: Option<bool>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = tr("cli.init.about"))]
    Init {
        #[arg(long, help = tr("cli.init.from_env"))]
        from_env: bool,
        #[arg(long, value_name = "NAME", conflicts_with = "from_env", help = tr("cli.init.distro"))]
        distro: Option<RosDistro>,
        #[arg(long, value_name = "FLAVOR", value_parser = ["ros-base", "desktop"], hide_possible_values = true, help = tr("cli.init.base"))]
        base: Option<String>,
        #[arg(long, help = tr("cli.init.force"))]
        force: bool,
    },

    #[command(about = tr("cli.check.about"))]
    Check {
        #[arg(long, value_name = "PLATFORM", help = tr("cli.check.target"))]
        target: Option<Platform>,
        #[arg(long, help = tr("cli.check.strict"))]
        strict: bool,
    },

    #[command(about = tr("cli.lock.about"))]
    Lock {
        #[arg(long, value_name = "PLATFORM", help = tr("cli.check.target"))]
        platform: Option<Platform>,
    },

    #[command(about = tr("cli.verify.about"))]
    Verify,

    #[command(about = tr("cli.apply.about"))]
    Apply {
        #[arg(long, value_name = "NAME", help = tr("cli.apply.distro"))]
        distro: Option<RosDistro>,
        #[arg(long, help = tr("cli.apply.locked"))]
        locked: bool,
        #[arg(long, help = tr("cli.apply.build"))]
        build: bool,
        #[arg(long, help = tr("cli.no_env_check"))]
        no_env_check: bool,
        #[arg(long, help = tr("cli.apply.offline"))]
        offline: bool,
        #[arg(long, value_name = "NAME", help = tr("cli.apply.mirror"), hide_possible_values = true, value_parser = clap::builder::PossibleValuesParser::new(rospm_core::mirror::NAMES))]
        mirror: Option<String>,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },

    #[command(about = tr("cli.uninstall.about"))]
    Uninstall {
        #[arg(long, help = tr("cli.uninstall.keep_source"))]
        keep_source: bool,
        #[arg(short, long, help = tr("cli.uninstall.yes"))]
        yes: bool,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },

    #[command(about = tr("cli.build.about"))]
    Build {
        #[arg(help = tr("cli.build.workspace"))]
        workspace: Option<String>,
        #[arg(long, value_name = "NAME", conflicts_with = "conda_prefix", help = tr("cli.build.conda_env"))]
        conda_env: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with = "conda_env", help = tr("cli.build.conda_prefix"))]
        conda_prefix: Option<PathBuf>,
        #[arg(long, help = tr("cli.no_env_check"))]
        no_env_check: bool,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
        #[arg(last = true, value_name = "COLCON_BUILD_ARGS", help = tr("cli.build.colcon_args"))]
        colcon_args: Vec<String>,
    },

    #[command(about = tr("cli.run.about"))]
    Run {
        #[arg(long, help = tr("cli.run.list"))]
        list: bool,
        #[arg(long, help = tr("cli.run.interactive"))]
        interactive: bool,
        #[arg(help = tr("cli.run.target"))]
        target: Option<String>,
        #[arg(last = true, value_name = "ARGS", help = tr("cli.run.args"))]
        args: Vec<String>,
    },

    #[command(about = tr("cli.env.about"))]
    Env {
        #[arg(long, default_value = "bash", hide_default_value = true, help = tr("cli.env.shell"))]
        shell: Shell,
    },

    #[command(about = tr("cli.shell.about"))]
    Shell,

    #[command(about = tr("cli.tui.about"))]
    Tui,

    #[command(about = tr("cli.deps.about"))]
    Deps {
        #[command(subcommand)]
        command: DepsCmd,
    },

    #[command(about = tr("cli.export.about"))]
    Export {
        #[arg(long, value_name = "PLATFORM", help = tr("cli.export.target"))]
        target: Option<Platform>,
        #[arg(short, long, value_name = "PATH", help = tr("cli.export.output"))]
        output: Option<PathBuf>,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },

    #[command(about = tr("cli.import.about"))]
    Import {
        #[arg(help = tr("cli.import.bundle"))]
        bundle: PathBuf,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },

    #[command(about = tr("cli.editor.about"))]
    Editor {
        #[arg(help = tr("cli.editor.kind"))]
        kind: Option<String>,
    },

    #[command(about = tr("cli.doctor.about"))]
    Doctor {
        #[arg(long, help = tr("cli.doctor.no_network"))]
        no_network: bool,
    },

    // Replaces clap's built-in `help` subcommand, whose text is not translatable.
    #[command(about = tr("cli.help.about"))]
    Help {
        #[arg(help = tr("cli.help.command"))]
        command: Vec<String>,
    },
}

#[derive(Subcommand)]
enum DepsCmd {
    #[command(about = tr("cli.deps.check"))]
    Check,
    #[command(about = tr("cli.deps.install"))]
    Install {
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },
}

/// clap hard-codes its section headings ("Usage:", "Commands:", …); a
/// per-command template supplies translated ones instead. Applied
/// recursively because templates are not inherited by subcommands.
fn localize_help(cmd: Command) -> Command {
    let has_subcommands = cmd.has_subcommands();
    let has_positionals = cmd.get_positionals().next().is_some();
    let mut t = String::from("{before-help}{about-with-newline}\n");
    t.push_str(&tr("help.usage"));
    t.push_str(" {usage}\n");
    if has_subcommands {
        t.push('\n');
        t.push_str(&tr("help.commands"));
        t.push_str("\n{subcommands}\n");
    }
    if has_positionals {
        t.push('\n');
        t.push_str(&tr("help.arguments"));
        t.push_str("\n{positionals}\n");
    }
    t.push('\n');
    t.push_str(&tr("help.options"));
    t.push_str("\n{options}{after-help}");
    cmd.help_template(t).mut_subcommands(localize_help)
}

fn parse_cli() -> Cli {
    let cmd = localize_help(Cli::command());
    let matches = cmd.get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

/// `rospm help [COMMAND…]`: the long help of the named (sub)command.
fn print_help(path: &[String]) -> anyhow::Result<()> {
    let mut cmd = localize_help(Cli::command());
    // Build first so global flags and `rospm …` usage prefixes reach subcommands.
    cmd.build();
    let mut cur = &mut cmd;
    for name in path {
        cur = match cur.find_subcommand_mut(name) {
            Some(sc) => sc,
            None => {
                anyhow::bail!("{}", rospm_core::msg!("cli.help.unknown", command = name))
            }
        };
    }
    match cur.print_long_help() {
        Ok(()) => Ok(()),
        // `rospm help | head`: nothing to report.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn main() -> ExitCode {
    // Language first: everything below, including `--help`, is rendered with it.
    i18n::install(i18n::from_process());
    let cli = parse_cli();
    init_logging(cli.verbose);
    let global = Global {
        manifest: cli.manifest,
        json: cli.json,
    };

    let build_json = global.json && matches!(&cli.command, Cmd::Build { .. } | Cmd::Apply { .. });
    let redirect = if build_json {
        match build_summary::JsonOutput::redirect() {
            Ok(guard) => Some(guard),
            Err(err) => {
                eprintln!("{err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let result = match cli.command {
        Cmd::Init {
            from_env,
            distro,
            base,
            force,
        } => {
            let base = base
                .as_deref()
                .map(|b| b.parse::<RosFlavor>().expect("validated by clap"));
            commands::init(&global, from_env, distro, base, force)
        }
        Cmd::Check { target, strict } => commands::check(&global, target, strict),
        Cmd::Lock { platform } => commands::lock(&global, platform),
        Cmd::Verify => commands::verify(&global),
        Cmd::Apply {
            distro,
            locked,
            build,
            no_env_check,
            offline,
            mirror,
            dry_run,
        } => {
            let mirror = mirror
                .as_deref()
                .map(|m| m.parse::<Mirror>().expect("validated by clap"));
            commands::apply(
                &global,
                locked,
                build,
                no_env_check,
                offline,
                dry_run,
                mirror,
                distro,
            )
        }
        Cmd::Uninstall {
            keep_source,
            yes,
            dry_run,
        } => commands::uninstall(&global, keep_source, yes, dry_run),
        Cmd::Build {
            workspace,
            conda_env,
            conda_prefix,
            no_env_check,
            dry_run,
            colcon_args,
        } => commands::build(
            &global,
            workspace.as_deref(),
            conda_env,
            conda_prefix,
            colcon_args,
            no_env_check,
            dry_run,
        ),
        Cmd::Run {
            list,
            interactive,
            target,
            args,
        } => commands::run(&global, list, interactive, target.as_deref(), args),
        Cmd::Env { shell } => commands::env(&global, shell),
        Cmd::Shell => commands::shell(&global),
        Cmd::Tui => rospm_tui::run(
            global.manifest.as_deref(),
            rospm_tui::Text {
                no_logs: tr("cli.tui.no_logs"),
                log_error: tr("cli.tui.log_error"),
                open_logs: tr("cli.tui.open_logs"),
                log_help: tr("cli.tui.log_help"),

                build_order: tr("cli.tui.build_order"),
                running: tr("cli.tui.running"),
                not_installed: tr("cli.tui.not_installed"),
                manifest_label: tr("cli.tui.manifest_label"),
                provider_label: tr("cli.tui.provider_label"),
                root_label: tr("cli.tui.root_label"),
                refresh: tr("cli.tui.refresh"),
                missing: tr("cli.tui.missing"),
                item: tr("cli.tui.item"),
                actual: tr("cli.tui.actual"),
                expected: tr("cli.tui.expected"),
                not_connected: tr("cli.tui.not_connected"),
                components: tr("cli.tui.components"),

                tabs: [
                    "start", "install", "build", "run", "debug", "diagnose", "help", "settings",
                ]
                .map(|tab| tr(&format!("cli.tui.tab_{tab}"))),
                title: tr("cli.tui.title"),
                project: tr("cli.tui.project"),
                overview: tr("cli.tui.overview"),
                environment: tr("cli.tui.environment"),
                workspace: tr("cli.tui.workspace"),
                status: tr("cli.tui.status"),
                ready: tr("cli.tui.ready"),
                details: tr("cli.tui.details"),
                manifest_search: tr("cli.tui.manifest_search"),
                quit: tr("cli.tui.quit"),
                non_terminal: tr("cli.tui.non_terminal"),
                actions: tr("cli.tui.actions"),
                check: tr("cli.tui.check"),
                verify: tr("cli.tui.verify"),
                deps: tr("cli.tui.deps"),
                apply_plan: tr("cli.tui.apply_plan"),
                uninstall_plan: tr("cli.tui.uninstall_plan"),
                plan: tr("cli.tui.plan"),
                build_args: tr("cli.tui.build_args"),
                conda: tr("cli.tui.conda"),
                check_result: tr("cli.tui.check_result"),
                verify_result: tr("cli.tui.verify_result"),
                deps_result: tr("cli.tui.deps_result"),
                task_status: tr("cli.tui.task_status"),
                logs: tr("cli.tui.logs"),
                diagnostics: tr("cli.tui.diagnostics"),
                status_ok: tr("cli.status.ok"),
                status_missing: tr("cli.status.missing"),
                status_mismatch: tr("cli.status.mismatch"),
                status_unknown: tr("cli.status.unknown"),
                status_info: tr("cli.status.info"),
            },
        ),
        Cmd::Deps { command } => match command {
            DepsCmd::Check => commands::deps_check(&global),
            DepsCmd::Install { dry_run } => commands::deps_install(&global, dry_run),
        },
        Cmd::Export { .. } => commands::not_implemented("export", "P6"),
        Cmd::Import { .. } => commands::not_implemented("import", "P6"),
        Cmd::Editor { .. } => commands::not_implemented("editor", "P7"),
        Cmd::Doctor { no_network } => commands::doctor(&global, no_network),
        Cmd::Help { command } => print_help(&command),
    };

    drop(redirect);
    match result {
        Ok(()) => {
            if build_json {
                println!("{}", serde_json::json!({"ok": true}));
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            let text = commands::describe(&err);
            if global.json {
                let mut payload = serde_json::json!({ "ok": false, "error": text });
                if let Some(summary) = err.downcast_ref::<build_summary::Summary>() {
                    payload["build_summary"] =
                        serde_json::to_value(summary).expect("serializable summary");
                }
                println!("{payload}");
            } else {
                if let Some(summary) = err.downcast_ref::<build_summary::Summary>() {
                    build_summary::print(summary);
                }
                eprintln!("{}", rospm_core::msg!("cli.err.prefix", text = text));
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
