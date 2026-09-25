//! Command implementations. Anything scheduled for a later roadmap phase
//! returns [`not_implemented`] with the phase so users know where it stands.
//!
//! All user-facing text goes through the i18n layer (spec §4.2): either a
//! `msg!` key rendered by [`crate::i18n::render`] via `Display`, or
//! [`tr`] for plain CLI strings. `--json` key names and enum values stay
//! English.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Serialize;

use rospm_core::introspect::{self, HostFacts};
use rospm_core::manifest::{Manifest, RosFlavor, MANIFEST_FILE};
use rospm_core::platform::{Platform, RosDistro};
use rospm_core::probe::{EnvReport, Host, Status, NATIVE_CORE_TOOLS, TOOLS};
use rospm_core::provider::{self, Context, Shell};
use rospm_core::{msg, Error};

use crate::i18n::{self, tr};

pub struct Global {
    pub manifest: Option<PathBuf>,
    pub json: bool,
}

struct Loaded {
    path: PathBuf,
    root: PathBuf,
    manifest: Manifest,
}

fn locate(global: &Global) -> Result<PathBuf> {
    if let Some(p) = &global.manifest {
        return Ok(p.clone());
    }
    let cwd = std::env::current_dir().context(tr("cli.err.no_cwd"))?;
    Manifest::find(&cwd).ok_or_else(|| {
        anyhow!(
            "{}",
            msg!(
                "cli.err.manifest_not_found",
                file = MANIFEST_FILE,
                dir = cwd.display()
            )
        )
    })
}

fn load(global: &Global) -> Result<Loaded> {
    let path = locate(global)?;
    let manifest = Manifest::load(&path)?;
    let root = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .with_context(|| msg!("cli.err.cannot_resolve", path = root.display()).to_string())?;
    Ok(Loaded {
        path,
        root,
        manifest,
    })
}

fn host_platform() -> Result<Platform> {
    Platform::current().ok_or_else(|| anyhow!("{}", tr("cli.err.unsupported_host")))
}

pub fn not_implemented(feature: &'static str, phase: &'static str) -> Result<()> {
    Err(Error::NotImplemented { feature, phase }.into())
}

/// The error chain as one localised line. A `rospm_core::Error` already
/// describes its own source, so the chain stops there.
pub fn describe(err: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        if let Some(core) = cause.downcast_ref::<Error>() {
            parts.push(core.to_string());
            break;
        }
        parts.push(cause.to_string());
    }
    parts.join(": ")
}

// ---------------------------------------------------------------- init

/// One template per language (comments differ, keys and values do not).
const TEMPLATE_EN: &str = include_str!("../templates/rospm.en.toml");
const TEMPLATE_ZH_CN: &str = include_str!("../templates/rospm.zh-CN.toml");
/// `ros_distro` hard-coded in the templates; `--base` composes the package from it.
const TEMPLATE_DISTRO: RosDistro = RosDistro::Jazzy;
/// Start of the commented-out `ros_base` line that `--base` replaces.
const TEMPLATE_ROS_BASE_LINE: &str = "# ros_base = \"ros-jazzy-ros-base\"";

fn template() -> &'static str {
    match i18n::current() {
        i18n::Lang::En => TEMPLATE_EN,
        i18n::Lang::ZhCn => TEMPLATE_ZH_CN,
    }
}

/// Uncomment the `ros_base` line and set it to `pkg`.
fn set_template_ros_base(text: &str, pkg: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let idx = lines
        .iter()
        .position(|l| l.starts_with(TEMPLATE_ROS_BASE_LINE))
        .expect("internal error: template ros_base line changed");
    lines[idx] = format!(
        "ros_base = {:<26} # {}",
        format!("\"{pkg}\""),
        tr("template.ros_base_comment")
    );
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

pub fn init(global: &Global, from_env: bool, base: Option<RosFlavor>, force: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let path = global
        .manifest
        .clone()
        .unwrap_or_else(|| cwd.join(MANIFEST_FILE));
    if path.exists() && !force {
        bail!("{}", msg!("cli.init.exists", path = path.display()));
    }
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or(cwd);

    if from_env {
        return init_from_env(global, &root, &path, base);
    }

    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "my_project".into());
    let mut text = template().replace("@NAME@", &name);
    if let Some(flavor) = base {
        // The template's distro is fixed (jazzy); `--base` only picks the metapackage.
        let pkg = flavor.package(TEMPLATE_DISTRO);
        text = set_template_ros_base(&text, &pkg);
        assert!(
            text.contains(&pkg),
            "internal error: template ros_base line changed"
        );
    }
    // The template must always satisfy our own validator.
    Manifest::parse(&text, &path).context(tr("cli.init.template_invalid"))?;
    std::fs::write(&path, text)
        .with_context(|| msg!("cli.err.cannot_write", path = path.display()).to_string())?;
    println!("{}", msg!("cli.init.created", path = path.display()));
    println!("{}", tr("cli.init.next_template"));
    Ok(())
}

#[derive(Serialize)]
struct InitReport<'a> {
    ok: bool,
    manifest: String,
    draft: &'a introspect::Draft,
}

fn init_from_env(global: &Global, root: &Path, path: &Path, base: Option<RosFlavor>) -> Result<()> {
    let host = Host::probe();
    let facts = HostFacts::gather(&host);
    let mut draft = introspect::draft(root, &facts);
    if let Some(flavor) = base {
        match draft.ros_distro {
            Some(distro) => {
                let pkg = flavor.package(distro);
                draft.ros_base = Some(pkg.clone());
                draft.todos.retain(|t| !t.has_key_prefix("init.ros_base."));
                draft.notes.push(msg!(
                    "init.ros_base.from_flag",
                    package = pkg,
                    flavor = flavor
                ));
            }
            None => draft
                .todos
                .push(msg!("init.ros_base.flag_ignored", flavor = flavor)),
        }
    }
    let text = draft.render();
    // Decision #3: best effort, but never an invalid file.
    Manifest::parse(&text, path).context(tr("cli.init.draft_invalid"))?;
    std::fs::write(path, &text)
        .with_context(|| msg!("cli.err.cannot_write", path = path.display()).to_string())?;

    if global.json {
        let report = InitReport {
            ok: true,
            manifest: path.display().to_string(),
            draft: &draft,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "{}",
        msg!("cli.init.created_from_env", path = path.display())
    );
    for n in &draft.notes {
        println!("{}", msg!("cli.init.detected", text = n));
    }
    for t in &draft.todos {
        println!("{}", msg!("cli.init.todo", text = t));
    }
    if draft.todos.is_empty() {
        println!("{}", tr("cli.init.next_review"));
    } else if draft.todos.len() == 1 {
        println!("{}", tr("cli.init.next_todos_one"));
    } else {
        println!(
            "{}",
            msg!("cli.init.next_todos_many", count = draft.todos.len())
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- check

#[derive(Serialize)]
struct EnvSummary {
    /// False when probing was skipped (target differs from host).
    probed: bool,
    ready: bool,
    problems: usize,
    unknown: usize,
    items: Vec<rospm_core::probe::EnvItem>,
}

#[derive(Serialize)]
struct CheckReport {
    ok: bool,
    manifest: String,
    project: String,
    ros_distro: String,
    provider: String,
    platforms: Vec<String>,
    evaluated_platform: String,
    build_order: Vec<String>,
    effective_apt: BTreeMap<String, Vec<String>>,
    pip_recorded: Vec<String>,
    notes: Vec<String>,
    environment: EnvSummary,
}

pub fn check(global: &Global, target: Option<Platform>, strict: bool) -> Result<()> {
    let loaded = match load(global) {
        Ok(l) => l,
        Err(err) => {
            if global.json {
                let errors: Vec<String> = match err.downcast_ref::<Error>() {
                    Some(Error::Validation(list)) => list.iter().map(ToString::to_string).collect(),
                    _ => vec![describe(&err)],
                };
                println!("{}", serde_json::json!({ "ok": false, "errors": errors }));
                std::process::exit(1);
            }
            return Err(err);
        }
    };
    let m = &loaded.manifest;
    let host = Host::probe();
    let platform = match target {
        Some(t) => t,
        None => host_platform()?,
    };

    let mut notes = Vec::new();
    if !m.project.platforms.contains(&platform) {
        notes.push(msg!("cli.check.platform_not_listed", platform = platform).to_string());
    }
    if target.is_some() {
        notes.push(tr("cli.check.target_note"));
    }

    let mut effective_apt = BTreeMap::new();
    for p in &m.project.platforms {
        effective_apt.insert(p.to_string(), m.effective_apt(*p));
    }

    // Probe the host only when the evaluated platform *is* the host; probing
    // amd64's dpkg for an arm64 target would just report noise.
    let cross = host.platform.map(|h| h != platform).unwrap_or(true);
    let env_report = if cross {
        notes.push(
            msg!(
                "cli.check.probing_skipped",
                target = platform,
                host = host
                    .platform
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| tr("probe.platform.unsupported"))
            )
            .to_string(),
        );
        None
    } else {
        let ctx = Context {
            manifest: m,
            root: &loaded.root,
            platform,
            locked: false,
            offline: false,
            host: &host,
        };
        Some(provider::for_manifest(m).probe_env(&ctx)?)
    };
    let environment = match &env_report {
        Some(r) => EnvSummary {
            probed: true,
            ready: r.ready(),
            problems: r.problems(),
            unknown: r.unknown(),
            items: r.items.clone(),
        },
        None => EnvSummary {
            probed: false,
            ready: false,
            problems: 0,
            unknown: 0,
            items: Vec::new(),
        },
    };

    let report = CheckReport {
        ok: true,
        manifest: loaded.path.display().to_string(),
        project: m.project.name.clone(),
        ros_distro: m.project.ros_distro.to_string(),
        provider: m.environment.provider.to_string(),
        platforms: m
            .project
            .platforms
            .iter()
            .map(ToString::to_string)
            .collect(),
        evaluated_platform: platform.to_string(),
        build_order: m.build_order()?,
        effective_apt,
        pip_recorded: m.dependencies.pip.clone(),
        notes,
        environment,
    };

    let strict_failed = strict && !report.environment.ready;

    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            msg!("cli.check.manifest_line", path = report.manifest)
        );
        println!(
            "{}",
            msg!(
                "cli.check.project_line",
                name = report.project,
                distro = report.ros_distro,
                provider = report.provider
            )
        );
        println!(
            "{}",
            msg!(
                "cli.check.platforms_line",
                platforms = report.platforms.join(", ")
            )
        );
        println!(
            "{}",
            msg!(
                "cli.check.build_order_line",
                order = report.build_order.join(" -> ")
            )
        );
        for (p, pkgs) in &report.effective_apt {
            if !pkgs.is_empty() {
                println!(
                    "{}",
                    msg!(
                        "cli.check.apt_line",
                        platform = p,
                        packages = pkgs.join(" ")
                    )
                );
            }
        }
        if !report.pip_recorded.is_empty() {
            println!(
                "{}",
                msg!(
                    "cli.check.pip_line",
                    packages = report.pip_recorded.join(" ")
                )
            );
        }
        for n in &report.notes {
            println!("{}", msg!("cli.check.note_line", text = n));
        }
        println!("{}", tr("cli.check.valid_line"));
        if let Some(r) = &env_report {
            println!();
            print_env_report(r, &report.evaluated_platform);
        }
    }

    if strict_failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Expected-vs-actual table. Read-only; the hints point at the command that
/// would change things, nothing is executed here.
fn print_env_report(r: &EnvReport, platform: &str) {
    println!("{}", msg!("cli.check.env_header", platform = platform));
    let subjects: Vec<String> = r.items.iter().map(|i| i.subject.to_string()).collect();
    let width = subjects
        .iter()
        .map(|s| i18n::display_width(s))
        .max()
        .unwrap_or(0)
        .min(48);
    for (item, subject) in r.items.iter().zip(&subjects) {
        let mark = match item.status {
            Status::Ok => tr("cli.status.ok"),
            Status::Missing => tr("cli.status.missing"),
            Status::Mismatch => tr("cli.status.mismatch"),
            Status::Unknown => tr("cli.status.unknown"),
            Status::Info => tr("cli.status.info"),
        };
        println!(
            "  {} {:<8} {}  {}  |  {}",
            i18n::pad(&mark, 8),
            item.area,
            i18n::pad(subject, width),
            item.expected,
            item.actual
        );
    }
    let problems = r.problems();
    let unknown = r.unknown();
    if r.ready() {
        println!("{}", tr("cli.check.ready"));
    } else {
        let mut parts = Vec::new();
        if problems == 1 {
            parts.push(tr("cli.check.problems_one"));
        } else if problems > 1 {
            parts.push(msg!("cli.check.problems_many", count = problems).to_string());
        }
        if unknown > 0 {
            parts.push(msg!("cli.check.undetermined", count = unknown).to_string());
        }
        println!(
            "{}",
            msg!("cli.check.not_ready", summary = parts.join(", "))
        );
    }
}

// ---------------------------------------------------------------- apply / build / env / shell

fn context<'a>(
    loaded: &'a Loaded,
    host: &'a Host,
    locked: bool,
    offline: bool,
) -> Result<Context<'a>> {
    Ok(Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform: host_platform()?,
        locked,
        offline,
        host,
    })
}

pub fn apply(global: &Global, locked: bool, offline: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, locked, offline)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

/// Removes packages, so unlike `apply` it asks first: `--yes` skips the
/// prompt, and without a terminal on stdin the prompt cannot be answered.
pub fn uninstall(global: &Global, keep_source: bool, yes: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_uninstall(&ctx, keep_source)?;
    let mut out = std::io::stdout().lock();
    if dry_run {
        plan.execute(true, &mut out)?;
        out.flush()?;
        return Ok(());
    }
    write!(out, "{}", plan.render())?;
    out.flush()?;
    if plan.action_count() == 0 {
        println!("{}", tr("cli.uninstall.nothing_to_do"));
        return Ok(());
    }
    if !yes {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            bail!("{}", tr("cli.uninstall.needs_confirmation"));
        }
        eprint!("{}", tr("cli.uninstall.confirm"));
        let mut answer = String::new();
        stdin.read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            bail!("{}", tr("cli.uninstall.aborted"));
        }
    }
    plan.perform(&mut out)?;
    out.flush()?;
    Ok(())
}

pub fn build(global: &Global, workspace: Option<&str>, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

pub fn env(global: &Global, shell: Shell) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, shell)?;
    print!("{script}");
    Ok(())
}

pub fn shell(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, Shell::Bash)?;

    let env_dir = loaded.root.join(".rospm");
    std::fs::create_dir_all(&env_dir)?;
    let env_file = env_dir.join("env.sh");
    std::fs::write(&env_file, &script)?;

    // Interactive bash that sources our script after the user's rc files.
    let rc = format!(
        "[ -f ~/.bashrc ] && source ~/.bashrc\nsource {}\nexport ROSPM_PROJECT={}\nPS1=\"(rospm:{}) $PS1\"\n",
        rospm_core::plan::shell_quote(&env_file.to_string_lossy()),
        rospm_core::plan::shell_quote(&loaded.manifest.project.name),
        loaded.manifest.project.name
    );
    let rc_file = env_dir.join("shellrc");
    std::fs::write(&rc_file, rc)?;

    eprintln!(
        "{}",
        msg!("cli.shell.entering", name = loaded.manifest.project.name)
    );
    let status = std::process::Command::new("bash")
        .arg("--rcfile")
        .arg(&rc_file)
        .arg("-i")
        .current_dir(&loaded.root)
        .status()
        .context(tr("cli.shell.start_failed"))?;
    if !status.success() {
        bail!("{}", msg!("cli.shell.exited", status = status));
    }
    Ok(())
}

// ---------------------------------------------------------------- doctor

#[derive(Serialize)]
struct ToolStatus {
    tool: String,
    purpose: String,
    found: Option<String>,
}

#[derive(Serialize)]
struct DoctorReport {
    ok: bool,
    platform: Option<String>,
    os: Option<String>,
    ros_distros_installed: Vec<String>,
    /// apt source file referencing packages.ros.org/ros2, if configured.
    ros_apt_source: Option<String>,
    apt_universe: Option<bool>,
    locale_utf8: bool,
    rosdep_initialized: bool,
    rosdep_updated: bool,
    tools: Vec<ToolStatus>,
}

pub fn doctor(global: &Global) -> Result<()> {
    let host = Host::probe();
    let tools: Vec<ToolStatus> = TOOLS
        .iter()
        .map(|(tool, purpose)| ToolStatus {
            tool: tool.to_string(),
            purpose: tr(purpose),
            found: host.tool(tool).map(|p| p.display().to_string()),
        })
        .collect();

    // Core toolchain for the native provider must be present.
    let core_ok = NATIVE_CORE_TOOLS.iter().all(|t| host.has_tool(t));

    let report = DoctorReport {
        ok: core_ok,
        platform: host.platform.map(|p| p.to_string()),
        os: host.os.as_ref().and_then(|o| o.pretty_name.clone()),
        ros_distros_installed: host.ros_distros.clone(),
        ros_apt_source: host
            .apt_sources
            .ros2
            .as_ref()
            .map(|p| p.display().to_string()),
        apt_universe: host.apt_sources.universe,
        locale_utf8: host.locale_utf8,
        rosdep_initialized: host.rosdep.initialized,
        rosdep_updated: host.rosdep.updated,
        tools,
    };

    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            msg!(
                "cli.doctor.platform",
                value = report
                    .platform
                    .clone()
                    .unwrap_or_else(|| tr("probe.platform.unsupported"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.os",
                value = report
                    .os
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.unknown"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.ros_installs",
                value = if report.ros_distros_installed.is_empty() {
                    tr("cli.doctor.no_ros")
                } else {
                    report.ros_distros_installed.join(", ")
                }
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.ros_apt_source",
                value = report
                    .ros_apt_source
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.source_not_configured"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.universe",
                value = match report.apt_universe {
                    Some(true) => tr("probe.universe.enabled"),
                    Some(false) => tr("cli.doctor.universe_not_enabled"),
                    None => tr("cli.doctor.unknown"),
                }
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.locale",
                value = if report.locale_utf8 {
                    "UTF-8".to_string()
                } else {
                    tr("cli.doctor.not_utf8")
                }
            )
        );
        let done_or_not = |done: bool| {
            if done {
                tr("cli.doctor.done")
            } else {
                tr("cli.doctor.not_run")
            }
        };
        println!(
            "{}",
            msg!(
                "cli.doctor.rosdep",
                init = done_or_not(report.rosdep_initialized),
                update = done_or_not(report.rosdep_updated)
            )
        );
        for t in &report.tools {
            let mark = if t.found.is_some() { "ok " } else { "-- " };
            println!(
                "{mark} {:<12} {} {}",
                t.tool,
                i18n::pad(&t.purpose, 45),
                t.found
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.not_found"))
            );
        }
        if !report.ok {
            println!(
                "{}",
                msg!(
                    "cli.doctor.core_missing",
                    tools = NATIVE_CORE_TOOLS.join(", ")
                )
            );
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_templates_are_valid_manifests() {
        for (lang, text) in [("en", TEMPLATE_EN), ("zh-CN", TEMPLATE_ZH_CN)] {
            let text = text.replace("@NAME@", "demo");
            let m = Manifest::parse(&text, Path::new("rospm.toml"))
                .unwrap_or_else(|e| panic!("{lang} template is invalid: {e}"));
            assert_eq!(m.project.ros_distro, TEMPLATE_DISTRO, "{lang}");
            assert!(
                text.contains(TEMPLATE_ROS_BASE_LINE),
                "{lang} template lost the ros_base line `--base` rewrites"
            );
            let with_base = set_template_ros_base(&text, "ros-jazzy-desktop");
            let m = Manifest::parse(&with_base, Path::new("rospm.toml"))
                .unwrap_or_else(|e| panic!("{lang} template with --base is invalid: {e}"));
            assert_eq!(m.ros_base_package(), "ros-jazzy-desktop", "{lang}");
        }
    }
}
