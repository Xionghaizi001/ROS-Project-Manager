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

    #[arg(long, hide = true)]
    internal_worker_runtime: Option<PathBuf>,
    #[arg(long, hide = true)]
    internal_worker_task: Option<String>,
    #[arg(long, hide = true)]
    internal_worker_interactive: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = tr("venv.about"))]
    Venv {
        #[command(subcommand)]
        command: VenvCmd,
    },
    #[command(about = tr("worker_cli.worker"))]
    Worker {
        #[arg(long, help = tr("worker_cli.runtime"))]
        runtime: PathBuf,
        #[arg(long, value_name = "SECONDS", default_value_t = commands::worker::DEFAULT_AUTO_EXIT_TIMEOUT_SECS, value_parser = clap::value_parser!(u64).range(1..), help = tr("worker_cli.auto_exit_timeout"))]
        auto_exit_timeout: u64,
        #[arg(long, hide = true)]
        run_project: Option<PathBuf>,
    },
    #[command(hide = true)]
    RunChild { request: String },
    #[command(hide = true)]
    ShellWorker,
    #[command(about = tr("worker_cli.task"))]
    Task {
        #[arg(long, help = tr("worker_cli.runtime"))]
        runtime: PathBuf,
        #[command(subcommand)]
        command: commands::worker::TaskCommand,
    },
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
        #[command(flatten)]
        venv: commands::VenvArgs,
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
    Verify {
        #[command(flatten)]
        venv: commands::VenvArgs,
    },

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
        #[command(flatten)]
        venv: commands::VenvArgs,
        #[arg(help = tr("cli.build.workspace"))]
        workspace: Option<String>,
        #[arg(long, value_name = "NAME", conflicts_with_all = ["conda_prefix", "venv", "venv_path"], help = tr("cli.build.conda_env"))]
        conda_env: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with_all = ["conda_env", "venv", "venv_path"], help = tr("cli.build.conda_prefix"))]
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
        #[command(flatten)]
        venv: commands::VenvArgs,
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
        #[command(flatten)]
        venv: commands::VenvArgs,
        #[arg(long, default_value = "bash", hide_default_value = true, help = tr("cli.env.shell"))]
        shell: Shell,
    },

    #[command(about = tr("cli.shell.about"))]
    Shell {
        #[command(flatten)]
        venv: commands::VenvArgs,
    },

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
        #[command(flatten)]
        venv: commands::VenvArgs,
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
enum VenvCmd {
    #[command(about = tr("venv.install_about"))]
    Install {
        #[command(flatten)]
        venv: commands::VenvArgs,
        #[arg(long, short = 'r', value_name = "FILE", help = tr("venv.requirements"))]
        requirements: PathBuf,
        #[arg(long, help = tr("venv.uv_install"))]
        uv: bool,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
    },
    #[command(about = tr("venv.create_about"))]
    Create {
        #[arg(long, help = tr("venv.uv"))]
        uv: bool,
        #[arg(long, help = tr("cli.dry_run"))]
        dry_run: bool,
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

impl Cmd {
    fn mutation(&self) -> Option<rospm_core::worker::Operation> {
        use rospm_core::worker::Operation;
        match self {
            Self::Build { dry_run: false, .. } => Some(Operation::Build),
            Self::Apply {
                dry_run: false,
                locked,
                ..
            } => Some(if *locked {
                Operation::ApplyLocked
            } else {
                Operation::Apply
            }),
            Self::Uninstall { dry_run: false, .. } => Some(Operation::Uninstall),
            Self::Deps {
                command: DepsCmd::Install { dry_run: false },
            } => Some(Operation::DepsInstall),
            Self::Venv {
                command: VenvCmd::Create { dry_run: false, .. },
            } => Some(Operation::VenvCreate),
            Self::Venv {
                command: VenvCmd::Install { dry_run: false, .. },
            } => Some(Operation::VenvInstall),
            Self::Lock { .. } => Some(Operation::Lock),
            _ => None,
        }
    }
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
    let json = cli.json;
    if cli.command.mutation().is_some() {
        let result = if cli.internal_worker_runtime.is_some() {
            commands::worker::synchronous::serve(cli)
        } else {
            commands::worker::synchronous::submit(&cli)
        };
        return match result {
            Ok(code) => code,
            Err(error) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"ok": false, "error": commands::describe(&error)})
                    );
                } else {
                    eprintln!("{}", commands::describe(&error));
                }
                ExitCode::FAILURE
            }
        };
    }
    if execute_cli(
        cli,
        None,
        None,
        std::io::IsTerminal::is_terminal(&std::io::stdin()),
    )
    .is_ok()
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn execute_cli(
    cli: Cli,
    interaction: Option<&dyn rospm_core::execution::Interaction>,
    task: Option<&str>,
    interactive: bool,
) -> rospm_core::Result<()> {
    let global = Global {
        verbose: cli.verbose > 0,
        manifest: cli.manifest,
        json: cli.json,
        interaction,
        task,
        interactive,
    };

    let build_json = global.json
        && matches!(
            &cli.command,
            Cmd::Build { .. } | Cmd::Apply { .. } | Cmd::Venv { .. }
        );
    let redirect = if build_json {
        match build_summary::JsonOutput::redirect() {
            Ok(guard) => Some(guard),
            Err(err) => {
                eprintln!("{err}");
                return Err(rospm_core::Error::io("<stdout>", err));
            }
        }
    } else {
        None
    };
    let result = match cli.command {
        Cmd::Venv {
            command:
                VenvCmd::Install {
                    venv,
                    requirements,
                    uv,
                    dry_run,
                },
        } => commands::venv::install(&global, venv, &requirements, uv, dry_run),
        Cmd::Venv {
            command: VenvCmd::Create { uv, dry_run },
        } => commands::venv::create(&global, uv, dry_run),
        Cmd::Worker {
            runtime,
            auto_exit_timeout,
            run_project,
        } => {
            let timeout = std::time::Duration::from_secs(auto_exit_timeout);
            match run_project {
                Some(root) => commands::worker::runs::serve(root, &runtime, timeout),
                None => commands::worker::serve(&global, &runtime, timeout),
            }
        }
        Cmd::RunChild { request } => commands::run_process::child(&request),
        Cmd::ShellWorker => commands::shell_worker(&global),
        Cmd::Task { runtime, command } => commands::worker::client(&global, &runtime, command),
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
        Cmd::Check {
            target,
            strict,
            venv,
        } => commands::check(&global, target, strict, venv),
        Cmd::Lock { platform } => commands::lock(&global, platform),
        Cmd::Verify { venv } => commands::verify(&global, venv),
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
            venv,
            workspace,
            conda_env,
            conda_prefix,
            no_env_check,
            dry_run,
            colcon_args,
        } => commands::build(
            &global,
            workspace.as_deref(),
            venv,
            conda_env,
            conda_prefix,
            colcon_args,
            no_env_check,
            dry_run,
        ),
        Cmd::Run {
            venv,
            list,
            interactive,
            target,
            args,
        } => commands::run(
            &global,
            venv,
            list,
            interactive,
            target.as_deref(),
            args,
            cli.verbose,
        ),
        Cmd::Env { shell, venv } => commands::env(&global, shell, venv),
        Cmd::Shell { venv } => commands::shell(&global, venv),
        Cmd::Tui => rospm_tui::run_with_runs(
            global.manifest.as_deref(),
            rospm_tui::Text {
                no_logs: tr("cli.tui.no_logs"),
                log_error: tr("cli.tui.log_error"),
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
                installing: tr("cli.tui.installing"),
                details: tr("cli.tui.details"),
                manifest_search: tr("cli.tui.manifest_search"),
                tui_usage: tr("cli.tui.tui_usage"),
                cli_help: tr("cli.tui.cli_help"),
                cli_usage: tr("cli.tui.cli_usage"),
                footer_select: tr("cli.tui.footer_select"),
                footer_open: tr("cli.tui.footer_open"),
                footer_tabs: tr("cli.tui.footer_tabs"),
                footer_scroll_up: tr("cli.tui.footer_scroll_up"),
                footer_scroll_down: tr("cli.tui.footer_scroll_down"),
                footer_refresh: tr("cli.tui.footer_refresh"),
                footer_install: tr("cli.tui.footer_install"),
                footer_back: tr("cli.tui.footer_back"),
                footer_quit: tr("cli.tui.footer_quit"),
                footer_build: tr("cli.tui.footer_build"),
                footer_rebuild: tr("cli.tui.footer_rebuild"),
                confirm_build: tr("cli.tui.confirm_build"),
                confirm_rebuild: tr("cli.tui.confirm_rebuild"),
                confirm_action: tr("cli.tui.confirm_action"),
                footer_apply: tr("cli.tui.footer_apply"),
                footer_uninstall: tr("cli.tui.footer_uninstall"),
                footer_check: tr("cli.tui.footer_check"),
                footer_verify: tr("cli.tui.footer_verify"),
                footer_deps_check: tr("cli.tui.footer_deps_check"),
                footer_deps_install: tr("cli.tui.footer_deps_install"),
                footer_create_project: tr("cli.tui.footer_create_project"),
                footer_scan_projects: tr("cli.tui.footer_scan_projects"),
                footer_rescan_projects: tr("cli.tui.footer_rescan_projects"),
                project_scan_title: tr("cli.tui.project_scan_title"),
                no_projects_found: tr("cli.tui.no_projects_found"),
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
                task_attach: tr("cli.tui.task_attach"),
                task_cancel: tr("cli.tui.task_cancel"),
                task_pending: tr("cli.tui.task_pending"),
                task_waiting_confirmation: tr("cli.tui.task_waiting_confirmation"),
                task_waiting_authorization: tr("cli.tui.task_waiting_authorization"),
                task_cancelling: tr("cli.tui.task_cancelling"),
                task_succeeded: tr("cli.tui.task_succeeded"),
                task_failed: tr("cli.tui.task_failed"),
                task_cancelled: tr("cli.tui.task_cancelled"),
                task_interrupted: tr("cli.tui.task_interrupted"),
                task_offline: tr("cli.tui.task_offline"),
                task_step: tr("cli.tui.task_step"),
                logs: tr("cli.tui.logs"),
                diagnostics: tr("cli.tui.diagnostics"),
                status_ok: tr("cli.status.ok"),
                status_missing: tr("cli.status.missing"),
                status_mismatch: tr("cli.status.mismatch"),
                status_unknown: tr("cli.status.unknown"),
                status_info: tr("cli.status.info"),
                run_not_built: tr("cli.run.not_built"),
                run_empty: tr("cli.run.none"),
                run_node: tr("cli.run.nodes"),
                run_launch: tr("cli.run.launch_files"),
                run_processes: tr("cli.tui.run_processes"),
                run_foreground: tr("cli.tui.run_foreground"),
                run_background: tr("cli.tui.run_background"),
                run_args: tr("cli.tui.run_args"),
                run_bad_args: tr("cli.tui.run_bad_args"),
                run_venv: tr("cli.tui.run_venv"),
                run_venv_help: tr("cli.tui.run_venv_help"),
                run_terminate: tr("cli.tui.run_terminate"),
                run_kill: tr("cli.tui.run_kill"),
                run_suspend: tr("cli.tui.run_suspend"),
                run_resume: tr("cli.tui.run_resume"),
                run_suspended: tr("cli.tui.run_suspended"),
                shell_detached: tr("cli.tui.shell_detached"),
                run_starting: tr("cli.tui.run_starting"),
                run_current: tr("cli.tui.run_current"),
                run_history: tr("cli.tui.run_history"),
                run_no_history: tr("cli.tui.run_no_history"),
                run_exited: tr("cli.tui.run_exited"),
                run_exit_code: tr("cli.tui.run_exit_code"),
                run_exit_signal: tr("cli.tui.run_exit_signal"),
                run_exit_unknown: tr("cli.tui.run_exit_unknown"),
                run_idle: tr("cli.tui.run_idle"),
                run_gone: tr("cli.tui.run_gone"),
                tui_already_running: tr("cli.tui.tui_already_running"),
            },
            || commands::apply(&global, false, false, false, false, false, None, None),
            || commands::components_install(&global, false),
            |rebuild| {
                if rebuild {
                    commands::rebuild(&global)
                } else {
                    commands::build(
                        &global,
                        None,
                        commands::VenvArgs::default(),
                        None,
                        None,
                        Vec::new(),
                        false,
                        false,
                    )
                }
            },
            |action| match action {
                rospm_tui::Action::Apply => {
                    commands::apply(&global, false, false, false, false, false, None, None)
                }
                rospm_tui::Action::Uninstall => commands::uninstall(&global, false, true, false),
                rospm_tui::Action::Check => {
                    commands::check(&global, None, false, commands::VenvArgs::default())
                }
                rospm_tui::Action::Verify => {
                    commands::verify(&global, commands::VenvArgs::default())
                }
                rospm_tui::Action::DepsCheck => commands::deps_check(&global),
                rospm_tui::Action::DepsInstall => commands::deps_install(&global, false),
                rospm_tui::Action::CreateProject => {
                    commands::init(&global, false, None, None, false)
                }
            },
            &mut commands::worker::runs::Runs::default(),
            cli.verbose,
        ),
        Cmd::Deps { command } => match command {
            DepsCmd::Check => commands::deps_check(&global),
            DepsCmd::Install { dry_run } => commands::deps_install(&global, dry_run),
        },
        Cmd::Export { .. } => commands::not_implemented("export", "P6"),
        Cmd::Import { .. } => commands::not_implemented("import", "P6"),
        Cmd::Editor { .. } => commands::not_implemented("editor", "P7"),
        Cmd::Doctor { no_network, venv } => commands::doctor(&global, no_network, venv),
        Cmd::Help { command } => print_help(&command),
    };

    drop(redirect);
    match result {
        Ok(()) => {
            if build_json {
                println!("{}", serde_json::json!({"ok": true}));
            }
            Ok(())
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
            match err.downcast::<rospm_core::Error>() {
                Ok(
                    error @ (rospm_core::Error::Cancelled
                    | rospm_core::Error::TerminalOutcomeUnknown),
                ) => Err(error),
                _ => Err(rospm_core::Error::Host(rospm_core::Message::text(text))),
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_routes_cover_all_entries_and_leave_previews_local() {
        for args in [
            vec!["build", "ws", "--venv-path", "../env", "--no-env-check"],
            vec![
                "apply",
                "--locked",
                "--build",
                "--offline",
                "--mirror",
                "tuna",
            ],
            vec!["uninstall", "--keep-source", "--yes"],
            vec!["deps", "install"],
            vec!["venv", "create", "--uv"],
            vec![
                "venv",
                "install",
                "--venv",
                "-r",
                "../requirements.txt",
                "--uv",
            ],
        ] {
            let mut argv = vec!["rospm", "--manifest", "../custom.toml", "--json"];
            argv.extend(args);
            assert!(Cli::try_parse_from(&argv)
                .unwrap()
                .command
                .mutation()
                .is_some());
            argv.push("--dry-run");
            assert!(Cli::try_parse_from(&argv)
                .unwrap()
                .command
                .mutation()
                .is_none());
        }
        assert!(
            Cli::try_parse_from(["rospm", "lock", "--platform", "linux-aarch64"])
                .unwrap()
                .command
                .mutation()
                .is_some()
        );
        for command in ["check", "verify", "run", "env", "doctor"] {
            assert!(Cli::try_parse_from(["rospm", command])
                .unwrap()
                .command
                .mutation()
                .is_none());
        }
    }

    #[test]
    fn worker_auto_exit_timeout_defaults_and_validates() {
        for (args, expected) in [
            (vec!["rospm", "worker", "--runtime", "/tmp/test"], 60),
            (
                vec![
                    "rospm",
                    "worker",
                    "--runtime",
                    "/tmp/test",
                    "--auto-exit-timeout",
                    "120",
                ],
                120,
            ),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(
                matches!(cli.command, Cmd::Worker { auto_exit_timeout, .. } if auto_exit_timeout == expected)
            );
        }
        for invalid in ["0", "-1", "abc", "18446744073709551616"] {
            assert!(Cli::try_parse_from([
                "rospm",
                "worker",
                "--runtime",
                "/tmp/test",
                "--auto-exit-timeout",
                invalid
            ])
            .is_err());
        }
    }
}
