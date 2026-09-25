//! Command implementations. Anything scheduled for a later roadmap phase
//! returns [`not_implemented`] with the phase so users know where it stands.

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
use rospm_core::Error;

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
    let cwd = std::env::current_dir().context("cannot determine current directory")?;
    Manifest::find(&cwd).ok_or_else(|| {
        anyhow!(
            "no {MANIFEST_FILE} found in {} or any parent directory (run `rospm init`)",
            cwd.display()
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
        .with_context(|| format!("cannot resolve {}", root.display()))?;
    Ok(Loaded {
        path,
        root,
        manifest,
    })
}

fn host_platform() -> Result<Platform> {
    Platform::current().ok_or_else(|| {
        anyhow!("this host is not a supported platform (expected linux x86_64 or aarch64)")
    })
}

pub fn not_implemented(feature: &'static str, phase: &'static str) -> Result<()> {
    Err(Error::NotImplemented { feature, phase }.into())
}

// ---------------------------------------------------------------- init

const TEMPLATE: &str = include_str!("../templates/rospm.toml");
/// `ros_distro` hard-coded in the template; `--base` composes the package from it.
const TEMPLATE_DISTRO: RosDistro = RosDistro::Jazzy;

pub fn init(global: &Global, from_env: bool, base: Option<RosFlavor>, force: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let path = global
        .manifest
        .clone()
        .unwrap_or_else(|| cwd.join(MANIFEST_FILE));
    if path.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
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
    let mut text = TEMPLATE.replace("@NAME@", &name);
    if let Some(flavor) = base {
        // The template's distro is fixed (jazzy); `--base` only picks the metapackage.
        let pkg = flavor.package(TEMPLATE_DISTRO);
        text = text.replace(
            "# ros_base = \"ros-jazzy-ros-base\"        # or ros-jazzy-desktop; `rospm init --base desktop` sets it",
            &format!(
                "ros_base = {pkg:<26} # ros-base (bare bones) | desktop (adds RViz, demos)",
                pkg = format!("\"{pkg}\"")
            ),
        );
        assert!(
            text.contains(&pkg),
            "internal error: template ros_base line changed"
        );
    }
    // The template must always satisfy our own validator.
    Manifest::parse(&text, &path).context("internal error: template is invalid")?;
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    println!("created {}", path.display());
    println!("next: edit the [workspaces] section and create main.repos, then run `rospm check`");
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
                draft
                    .todos
                    .retain(|t| !t.starts_with("environment.native.ros_base"));
                draft
                    .notes
                    .push(format!("environment.native.ros_base: {pkg} (from --base {flavor})"));
            }
            None => draft.todos.push(format!(
                "environment.native.ros_base: --base {flavor} ignored because ros_distro was not detected"
            )),
        }
    }
    let text = draft.render();
    // Decision #3: best effort, but never an invalid file.
    Manifest::parse(&text, path).context("internal error: drafted manifest is invalid")?;
    std::fs::write(path, &text).with_context(|| format!("cannot write {}", path.display()))?;

    if global.json {
        let report = InitReport {
            ok: true,
            manifest: path.display().to_string(),
            draft: &draft,
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("created {} from this host and directory", path.display());
    for n in &draft.notes {
        println!("detected    {n}");
    }
    for t in &draft.todos {
        println!("TODO        {t}");
    }
    if draft.todos.is_empty() {
        println!("next: review the file, then run `rospm check`");
    } else {
        println!(
            "next: resolve the {} TODO line{} above (also marked in the file), then run `rospm check`",
            draft.todos.len(),
            if draft.todos.len() == 1 { "" } else { "s" }
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
                    Some(Error::Validation(list)) => list.clone(),
                    _ => vec![format!("{err:#}")],
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
        notes.push(format!(
            "platform {platform} is not listed in project.platforms"
        ));
    }
    if target.is_some() {
        notes.push(
            "repository availability for the target platform is checked in roadmap P5; \
             only manifest-level overrides were applied"
                .into(),
        );
    }

    let mut effective_apt = BTreeMap::new();
    for p in &m.project.platforms {
        effective_apt.insert(p.to_string(), m.effective_apt(*p));
    }

    // Probe the host only when the evaluated platform *is* the host; probing
    // amd64's dpkg for an arm64 target would just report noise.
    let cross = host.platform.map(|h| h != platform).unwrap_or(true);
    let env_report = if cross {
        notes.push(format!(
            "environment probing skipped: target {platform} differs from this host ({})",
            host.platform
                .map(|p| p.to_string())
                .unwrap_or_else(|| "unsupported".into())
        ));
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
        println!("manifest    {}", report.manifest);
        println!(
            "project     {} (ROS {}, provider {})",
            report.project, report.ros_distro, report.provider
        );
        println!("platforms   {}", report.platforms.join(", "));
        println!("build order {}", report.build_order.join(" -> "));
        for (p, pkgs) in &report.effective_apt {
            if !pkgs.is_empty() {
                println!("apt[{p}]    {}", pkgs.join(" "));
            }
        }
        if !report.pip_recorded.is_empty() {
            println!("pip (recorded only) {}", report.pip_recorded.join(" "));
        }
        for n in &report.notes {
            println!("note        {n}");
        }
        println!("ok          manifest is valid");
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
    println!("environment ({platform}, expected vs actual)");
    let width = r
        .items
        .iter()
        .map(|i| i.subject.len())
        .max()
        .unwrap_or(0)
        .min(48);
    for item in &r.items {
        let mark = match item.status {
            Status::Ok => "  ok      ",
            Status::Missing => "  MISSING ",
            Status::Mismatch => "  MISMATCH",
            Status::Unknown => "  unknown ",
            Status::Info => "  info    ",
        };
        println!(
            "{mark} {:<8} {:<width$}  {}  |  {}",
            item.area, item.subject, item.expected, item.actual
        );
    }
    let problems = r.problems();
    let unknown = r.unknown();
    if r.ready() {
        println!("ready       host matches the manifest");
    } else {
        let mut parts = Vec::new();
        if problems > 0 {
            parts.push(format!(
                "{problems} problem{}",
                if problems == 1 { "" } else { "s" }
            ));
        }
        if unknown > 0 {
            parts.push(format!("{unknown} undetermined"));
        }
        println!(
            "not ready   {}; `rospm apply --dry-run` shows what would be installed, `rospm build --dry-run` what would be built",
            parts.join(", ")
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
        println!("nothing to do");
        return Ok(());
    }
    if !yes {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            bail!("refusing to uninstall without confirmation (pass --yes, or run interactively)");
        }
        eprint!("proceed with the plan above? [y/N] ");
        let mut answer = String::new();
        stdin.read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            bail!("aborted");
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
        "entering rospm shell for `{}` (exit to leave)",
        loaded.manifest.project.name
    );
    let status = std::process::Command::new("bash")
        .arg("--rcfile")
        .arg(&rc_file)
        .arg("-i")
        .current_dir(&loaded.root)
        .status()
        .context("failed to start bash")?;
    if !status.success() {
        bail!("shell exited with {status}");
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
            purpose: purpose.to_string(),
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
            "platform      {}",
            report.platform.as_deref().unwrap_or("unsupported")
        );
        println!(
            "os            {}",
            report.os.as_deref().unwrap_or("unknown")
        );
        println!(
            "ROS installs  {}",
            if report.ros_distros_installed.is_empty() {
                "none found under /opt/ros".to_string()
            } else {
                report.ros_distros_installed.join(", ")
            }
        );
        println!(
            "ROS apt src   {}",
            report
                .ros_apt_source
                .as_deref()
                .unwrap_or("not configured (`rospm apply` adds it)")
        );
        println!(
            "apt universe  {}",
            match report.apt_universe {
                Some(true) => "enabled",
                Some(false) => "not enabled",
                None => "unknown",
            }
        );
        println!(
            "locale        {}",
            if report.locale_utf8 {
                "UTF-8"
            } else {
                "not UTF-8 (ROS needs a UTF-8 locale)"
            }
        );
        println!(
            "rosdep        init: {}  update: {}",
            if report.rosdep_initialized {
                "done"
            } else {
                "not run"
            },
            if report.rosdep_updated {
                "done"
            } else {
                "not run"
            }
        );
        for t in &report.tools {
            let mark = if t.found.is_some() { "ok " } else { "-- " };
            println!(
                "{mark} {:<12} {:<45} {}",
                t.tool,
                t.purpose,
                t.found.as_deref().unwrap_or("not found")
            );
        }
        if !report.ok {
            println!(
                "core tools missing: native provider needs {}",
                NATIVE_CORE_TOOLS.join(", ")
            );
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}
