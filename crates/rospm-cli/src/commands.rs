//! Command implementations. Anything scheduled for a later roadmap phase
//! returns [`not_implemented`] with the phase so users know where it stands.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Serialize;

use rospm_core::introspect::{self, HostFacts};
use rospm_core::manifest::{Manifest, RosFlavor, MANIFEST_FILE};
use rospm_core::mirror::{self, Mirror};
use rospm_core::net::{self, ProbeResult, Role, Verdict};
use rospm_core::platform::{ubuntu_codename, Platform, RosDistro};
use rospm_core::probe::{EnvReport, Host, Status, NATIVE_CORE_TOOLS, TOOLS};
use rospm_core::provider::{self, Context, Shell};
use rospm_core::repos::ReposFile;
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
    // `--manifest rospm.toml` (no directory part) has an empty parent.
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot resolve project directory {}", root.display()))?;
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
            mirror: mirror::OFFICIAL,
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
    mirror: Mirror,
) -> Result<Context<'a>> {
    Ok(Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform: host_platform()?,
        locked,
        offline,
        host,
        mirror,
    })
}

/// `--mirror` beats `ROSPM_MIRROR`; neither means "probe and decide".
fn requested_mirror(flag: Option<Mirror>) -> Result<Option<Mirror>> {
    if flag.is_some() {
        return Ok(flag);
    }
    match std::env::var("ROSPM_MIRROR") {
        Ok(v) if !v.trim().is_empty() => {
            let m = v
                .trim()
                .parse::<Mirror>()
                .map_err(|e| anyhow!("ROSPM_MIRROR: {e}"))?;
            Ok(Some(m))
        }
        _ => Ok(None),
    }
}

pub fn apply(
    global: &Global,
    locked: bool,
    offline: bool,
    dry_run: bool,
    mirror: Option<Mirror>,
) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let requested = requested_mirror(mirror)?;
    let sources = if dry_run || offline {
        let m = requested.unwrap_or(mirror::OFFICIAL);
        println!(
            "# network: not probed ({}); planning with the {} sources (pass --mirror <name> to plan with a mirror)",
            if dry_run { "--dry-run" } else { "--offline" },
            m.name
        );
        m
    } else {
        select_sources(&loaded, requested)?
    };
    let ctx = context(&loaded, &host, locked, offline, sources)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
    let mut out = std::io::stdout().lock();
    let result = plan.execute(dry_run, &mut out);
    out.flush()?;
    match result {
        Err(Error::CommandFailed { command, status }) if !dry_run && sources.is_official() => {
            // Networks that pass a short probe can still stall on a longer
            // transfer; point at the switch instead of leaving the user to guess.
            Err(anyhow!(Error::CommandFailed { command, status }).context(
                "a step failed while using the official sources; `rospm doctor` shows which sources are reachable and `rospm apply --mirror <name>` retries from a mirror",
            ))
        }
        Err(e) => Err(e.into()),
        Ok(()) => Ok(()),
    }
}

/// Probe the sources `apply` is about to download from and pick one
/// (decision #17). Never switches silently: a mirror is used only when the
/// user passed `--mirror` / `ROSPM_MIRROR` or confirmed the prompt.
fn select_sources(loaded: &Loaded, requested: Option<Mirror>) -> Result<Mirror> {
    let m = &loaded.manifest;
    let ubuntu = m.ubuntu();
    let Some(codename) = ubuntu_codename(&ubuntu) else {
        // plan_apply reports the unsupported Ubuntu release with a proper message.
        return Ok(requested.unwrap_or(mirror::OFFICIAL));
    };
    let git_urls = repos_urls(loaded);
    let mut endpoints = match requested {
        Some(mr) => net::endpoints_for(&mr, codename),
        None => net::all_endpoints(codename, &git_urls),
    };
    if requested.is_some() {
        endpoints.extend(
            net::all_endpoints(codename, &git_urls)
                .into_iter()
                .filter(|e| e.role == Role::GitHost),
        );
    }
    println!(
        "network check ({} URLs, {} s timeout each)",
        endpoints.len(),
        net::DEFAULT_TIMEOUT.as_secs()
    );
    let results = net::probe(&endpoints, net::DEFAULT_TIMEOUT);
    print_probe_table(&results);

    for r in results
        .iter()
        .filter(|r| r.endpoint.role == Role::GitHost && !r.ok())
    {
        eprintln!(
            "warning: {} is not reachable; `vcs import` of the .repos sources will probably fail (rospm has no mirror for git hosts)",
            r.endpoint.source
        );
    }

    if let Some(mr) = requested {
        return match net::mirror_ok(&results, &mr, codename) {
            Some(true) => {
                println!("sources     {} ({})", mr.name, mr.label);
                Ok(mr)
            }
            _ if mr.is_official() => {
                eprintln!("warning: the official sources did not answer; continuing with them because --mirror official was requested");
                Ok(mr)
            }
            _ => bail!(
                "mirror `{}` did not answer on every endpoint it needs; pick another (--mirror {}) or --mirror official",
                mr.name,
                mirror::NAMES.join("|")
            ),
        };
    }

    match net::verdict(&results, codename) {
        Verdict::OfficialOk => {
            println!("sources     official");
            Ok(mirror::OFFICIAL)
        }
        Verdict::UseMirror { failed, candidates } => {
            println!("official sources unreachable:");
            for f in &failed {
                println!("  - {f}");
            }
            let best = candidates[0];
            let names: Vec<&str> = candidates.iter().map(|c| c.name).collect();
            println!(
                "mirrors that answered (fastest first): {}",
                names.join(", ")
            );
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                bail!(
                    "the official ROS sources are unreachable from this host; re-run with `--mirror {}` (or {}) to install from a mirror, or `--mirror official` to try anyway",
                    best.name,
                    names.join(" | ")
                );
            }
            eprint!(
                "use the {} mirror ({}) for the ROS apt source, ros.key and rosdep? [Y/n] ",
                best.name, best.label
            );
            let mut answer = String::new();
            stdin.read_line(&mut answer)?;
            if matches!(answer.trim(), "" | "y" | "Y" | "yes") {
                println!("sources     {} ({})", best.name, best.label);
                Ok(best)
            } else {
                bail!(
                    "aborted; re-run with `--mirror official` to try the official sources anyway, or `--mirror <name>` for a specific mirror"
                )
            }
        }
        Verdict::NothingReachable { failed } => {
            bail!(
                "neither the official ROS sources nor any known mirror answered ({} URLs failed, see the table above); check the network or proxy settings, or pass `--mirror official` to try anyway",
                failed.len()
            )
        }
    }
}

/// Git URLs from every `.repos` file the manifest names (best effort).
fn repos_urls(loaded: &Loaded) -> Vec<String> {
    let mut urls = Vec::new();
    for ws in loaded.manifest.workspaces.values() {
        if let Some(sources) = &ws.sources {
            if let Ok(repos) = ReposFile::load(&loaded.root.join(sources)) {
                urls.extend(repos.repositories.values().map(|r| r.url.clone()));
            }
        }
    }
    urls
}

fn print_probe_table(results: &[ProbeResult]) {
    for r in results {
        let role = match r.endpoint.role {
            Role::RosApt => "ROS apt  ",
            Role::Rosdistro => "rosdistro",
            Role::GitHost => "git host ",
        };
        println!(
            "  {:<34} {role} {:<9} {}",
            r.describe(),
            r.endpoint.source,
            r.endpoint.url
        );
    }
}

/// Removes packages, so unlike `apply` it asks first: `--yes` skips the
/// prompt, and without a terminal on stdin the prompt cannot be answered.
pub fn uninstall(global: &Global, keep_source: bool, yes: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false, mirror::OFFICIAL)?;
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
    let ctx = context(&loaded, &host, false, false, mirror::OFFICIAL)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

pub fn env(global: &Global, shell: Shell) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false, mirror::OFFICIAL)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, shell)?;
    print!("{script}");
    Ok(())
}

pub fn shell(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, false, mirror::OFFICIAL)?;
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
    /// Reachability of the official sources and known mirrors (`None` with `--no-network`).
    network: Option<Vec<ProbeResult>>,
}

pub fn doctor(global: &Global, no_network: bool) -> Result<()> {
    let host = Host::probe();
    let network = if no_network {
        None
    } else {
        let codename = host
            .os
            .as_ref()
            .and_then(|os| os.codename.clone())
            .unwrap_or_else(|| "jammy".into());
        let endpoints = net::all_endpoints(&codename, &[]);
        Some(net::probe(&endpoints, net::DEFAULT_TIMEOUT))
    };
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
        network,
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
        match &report.network {
            Some(results) => {
                println!(
                    "network       official sources and known mirrors ({} s timeout each; `rospm apply --mirror <name>` switches)",
                    net::DEFAULT_TIMEOUT.as_secs()
                );
                print_probe_table(results);
            }
            None => println!("network       not probed (--no-network)"),
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}
