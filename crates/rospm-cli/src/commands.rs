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
use rospm_core::lock::{sha256_hex, Lock, LOCK_FILE};
use rospm_core::manifest::{CondaBuild, Manifest, RosFlavor, MANIFEST_FILE};
use rospm_core::mirror::{self, Mirror};
use rospm_core::net::{self, ProbeResult, Role, Verdict};
use rospm_core::plan::Step;
use rospm_core::platform::{ubuntu_codename, Platform, RosDistro};
use rospm_core::probe::{
    EnvReport, Host, OsRelease, Status, DEFAULT_LOCALE_FILE, NATIVE_CORE_TOOLS, TOOLS,
};
use rospm_core::provider::{self, Context, Shell};
use rospm_core::repos::ReposFile;
use rospm_core::snapshot::{Fetch, Http};
use rospm_core::{msg, Error, Message};

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

impl Loaded {
    /// `rospm.lock` next to the manifest.
    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// The lock file, if there is one. A present but unparseable lock is an
    /// error: it is a project file, not a cache.
    fn load_lock(&self) -> Result<Option<Lock>> {
        let path = self.lock_path();
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Lock::load(&path)?))
    }
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
    // `--manifest rospm.toml` (no directory part) has an empty parent.
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
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
/// Start of the commented-out `ros_base` line that `--base` replaces.
const TEMPLATE_ROS_BASE_LINE: &str = "# ros_base = ";

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

fn default_distro(os: Option<&OsRelease>) -> RosDistro {
    match os
        .filter(|os| os.is_ubuntu())
        .and_then(|os| os.version_id.as_deref())
    {
        Some("22.04") => RosDistro::Humble,
        _ => RosDistro::Jazzy,
    }
}

pub fn init(
    global: &Global,
    from_env: bool,
    distro: Option<RosDistro>,
    base: Option<RosFlavor>,
    force: bool,
) -> Result<()> {
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
    let distro = distro.unwrap_or_else(|| default_distro(OsRelease::load().as_ref()));
    let mut text = template()
        .replace("@NAME@", &name)
        .replace("@DISTRO@", distro.as_str())
        .replace("@UBUNTU@", distro.default_ubuntu());
    if let Some(flavor) = base {
        let pkg = flavor.package(distro);
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
    repositories: Option<rospm_core::probe::EnvReport>,
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
        let resolved = m.resolve_apt(*p);
        notes.extend(
            resolved
                .replacements
                .iter()
                .map(|r| r.notice(*p).to_string()),
        );
        effective_apt.insert(p.to_string(), resolved.packages);
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
            lock: None,
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

    let repositories = target.map(|_| {
        rospm_core::repository::check(
            m,
            &loaded.root,
            platform,
            &rospm_core::snapshot::Http::default(),
        )
    });
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
        repositories,
    };

    let strict_failed = strict
        && ((!cross && !report.environment.ready)
            || report.repositories.as_ref().is_some_and(|r| !r.ready()));

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
        if let Some(r) = &report.repositories {
            print_check_report(r, &report.evaluated_platform, true);
        }
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
    print_check_report(r, platform, false);
}

fn print_check_report(r: &EnvReport, platform: &str, repositories: bool) {
    let header = if repositories {
        msg!("repository.header", platform = platform)
    } else {
        msg!("cli.check.env_header", platform = platform)
    };
    println!("{header}");
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
        println!(
            "{}",
            tr(if repositories {
                "repository.ready"
            } else {
                "cli.check.ready"
            })
        );
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
            if repositories {
                msg!("repository.not_ready", summary = parts.join(", "))
            } else {
                msg!("cli.check.not_ready", summary = parts.join(", "))
            }
        );
    }
}

// ---------------------------------------------------------------- lock

/// `rospm lock [--platform <p>]`: resolve the host into `rospm.lock` (spec §3).
/// Idempotent: the file is rewritten only when its content changes.
pub fn lock(global: &Global, platform: Option<Platform>) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let platform = match platform {
        Some(p) => p,
        None => host_platform()?,
    };
    let previous = loaded.load_lock()?;
    let ctx = Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform,
        locked: false,
        lock: previous.as_ref(),
        offline: false,
        host: &host,
        mirror: mirror::OFFICIAL,
    };
    let offline = std::env::var_os("ROSPM_NO_NETWORK").is_some_and(|v| !v.is_empty() && v != "0");
    let http = Http::default();
    let fetch: Option<&dyn Fetch> = if offline { None } else { Some(&http) };
    let mut resolution = provider::for_manifest(&loaded.manifest).resolve_lock(&ctx, fetch)?;

    // The hash is of the file the user edits, wherever `--manifest` points.
    let manifest_text = std::fs::read_to_string(&loaded.path)
        .with_context(|| msg!("cli.err.cannot_read", path = loaded.path.display()).to_string())?;
    resolution.lock.manifest_sha256 = sha256_hex(&manifest_text);

    let lock_path = loaded.lock_path();
    let mut changed = true;
    if let Some(prev) = &previous {
        resolution.lock.merge_other_platforms(prev, platform);
        if resolution.lock.same_content(prev) {
            resolution.lock.generated_at = prev.generated_at.clone();
            changed = false;
        }
    }
    if changed {
        resolution.lock.save(&lock_path)?;
    }

    let l = &resolution.lock;
    let native = l.provider.native.as_ref();
    let report = LockReport {
        ok: true,
        lock: lock_path.display().to_string(),
        changed,
        platform: platform.to_string(),
        generated_at: l.generated_at.clone(),
        sources: l.sources.len(),
        apt: l.apt_for(platform).len(),
        apt_other_platforms: l.apt.len() - l.apt_for(platform).len(),
        pip: l.pip.len(),
        conda: l.conda_for(platform).is_some(),
        ros_snapshot: native.and_then(|n| n.ros_snapshot.clone()),
        ubuntu_snapshot: native.and_then(|n| n.ubuntu_snapshot.clone()),
        notes: resolution.notes.iter().map(ToString::to_string).collect(),
    };
    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    for n in &report.notes {
        println!("{}", msg!("cli.lock.note_line", text = n));
    }
    println!(
        "{}",
        msg!(
            "cli.lock.summary",
            platform = report.platform,
            sources = report.sources,
            apt = report.apt,
            pip = report.pip
        )
    );
    if report.apt_other_platforms > 0 {
        println!(
            "{}",
            msg!(
                "cli.lock.other_platforms_kept",
                count = report.apt_other_platforms
            )
        );
    }
    let live = || tr("cli.lock.live");
    println!(
        "{}",
        msg!(
            "cli.lock.snapshots",
            ros = report.ros_snapshot.clone().unwrap_or_else(live),
            ubuntu = report.ubuntu_snapshot.clone().unwrap_or_else(live)
        )
    );
    if changed {
        println!("{}", msg!("cli.lock.written", path = report.lock));
    } else {
        println!("{}", msg!("cli.lock.unchanged", path = report.lock));
    }
    Ok(())
}

#[derive(Serialize)]
struct LockReport {
    ok: bool,
    lock: String,
    changed: bool,
    platform: String,
    generated_at: String,
    sources: usize,
    apt: usize,
    apt_other_platforms: usize,
    pip: usize,
    /// Whether this platform's managed Conda environment was recorded.
    conda: bool,
    ros_snapshot: Option<String>,
    ubuntu_snapshot: Option<String>,
    notes: Vec<String>,
}

// ---------------------------------------------------------------- apply / build / env / shell

fn context<'a>(
    loaded: &'a Loaded,
    host: &'a Host,
    locked: bool,
    lock: Option<&'a Lock>,
    offline: bool,
    mirror: Mirror,
) -> Result<Context<'a>> {
    Ok(Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform: host_platform()?,
        locked,
        lock,
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
            let m = v.trim().parse::<Mirror>().map_err(|_| {
                anyhow!(
                    "{}",
                    msg!(
                        "cli.apply.bad_mirror_env",
                        value = v.trim(),
                        names = mirror::NAMES.join(" | ")
                    )
                )
            })?;
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
    distro: Option<RosDistro>,
) -> Result<()> {
    let mut loaded = load(global)?;
    if locked && distro.is_some_and(|d| d != loaded.manifest.project.ros_distro) {
        bail!("{}", tr("cli.apply.locked_distro_conflict"));
    }
    // `--locked` requires a lock before anything else runs: missing or out
    // of date with rospm.toml is a user error, corrected by re-running
    // `rospm lock`, never a silent degradation (spec §3.2).
    let lock = if locked {
        Some(load_locked_lock(&loaded)?)
    } else {
        None
    };
    let host = Host::probe();
    let manifest_update = prepare_distro(&mut loaded, &host, distro)?;
    let requested = requested_mirror(mirror)?;
    let sources = if dry_run || offline {
        let m = requested.unwrap_or(mirror::OFFICIAL);
        println!(
            "# {}",
            msg!(
                "cli.apply.not_probed",
                reason = if dry_run { "--dry-run" } else { "--offline" },
                name = m.name
            )
        );
        m
    } else {
        select_sources(&loaded, requested)?
    };
    // Snapshot dates roll off the snapshot services over time (P0 finding):
    // a real run checks the recorded snapshot is still online and, if not,
    // degrades to the live repositories (announced, never silent) instead of
    // failing outright. `--dry-run` never touches the network here — the
    // printed plan already shows exactly what the lock says.
    let lock = match lock {
        Some(l) if !dry_run => Some(degrade_unreachable_snapshots(&loaded, l)),
        other => other,
    };
    let ctx = context(&loaded, &host, locked, lock.as_ref(), offline, sources)?;
    let mut plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
    if let Some(contents) = manifest_update {
        plan.steps.insert(
            0,
            Step::WriteFile {
                path: loaded.path.clone(),
                contents,
                sudo: false,
            },
        );
    }
    let mut out = std::io::stdout().lock();
    let result = plan.execute(dry_run, &mut out);
    out.flush()?;
    match result {
        Err(Error::CommandFailed { command, status }) if !dry_run && sources.is_official() => {
            // Networks that pass a short probe can still stall on a longer
            // transfer; point at the switch instead of leaving the user to guess.
            Err(anyhow!(Error::CommandFailed { command, status })
                .context(tr("cli.apply.official_failed_hint")))
        }
        Err(e) => Err(e.into()),
        Ok(()) => Ok(()),
    }
}

/// Validate a distro change before any writes or network probes. The returned
/// text is written as the first plan step, so dry runs leave the project intact.
fn prepare_distro(
    loaded: &mut Loaded,
    host: &Host,
    distro: Option<RosDistro>,
) -> Result<Option<String>> {
    let Some(distro) = distro.filter(|d| *d != loaded.manifest.project.ros_distro) else {
        return Ok(None);
    };
    let ubuntu = distro.default_ubuntu();
    if host.ubuntu_matches(ubuntu) != Some(true) {
        let have = host
            .os
            .as_ref()
            .and_then(|os| os.pretty_name.as_deref())
            .map(str::to_string)
            .unwrap_or_else(|| tr("native.unknown_os"));
        bail!(
            "{}",
            msg!(
                "native.apply.wrong_ubuntu",
                ubuntu = ubuntu,
                distro = distro,
                have = have
            )
        );
    }
    let text = std::fs::read_to_string(&loaded.path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    set_toml_string(&mut doc["project"]["ros_distro"], distro.as_str());
    if let Some(native) = doc
        .get_mut("environment")
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|env| env.get_mut("native"))
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        if let Some(base) = native.get_mut("ros_base") {
            let old_prefix = format!("ros-{}-", loaded.manifest.project.ros_distro);
            if let Some(suffix) = base.as_str().and_then(|s| s.strip_prefix(&old_prefix)) {
                let package = format!("ros-{distro}-{suffix}");
                set_toml_string(base, &package);
            }
        }
        if let Some(version) = native.get_mut("ubuntu") {
            set_toml_string(version, ubuntu);
        }
    }
    let text = doc.to_string();
    loaded.manifest = Manifest::parse(&text, &loaded.path)?;
    Ok(Some(text))
}

/// Keep comments and surrounding whitespace when replacing a TOML value.
fn set_toml_string(item: &mut toml_edit::Item, text: &str) {
    let decor = item
        .as_value()
        .expect("validated string field")
        .decor()
        .clone();
    let mut value = toml_edit::Value::from(text);
    *value.decor_mut() = decor;
    *item = toml_edit::Item::Value(value);
}

/// Load and validate `rospm.lock` for `apply --locked` (spec §3.2): absent or
/// stale is corrected by re-running `rospm lock`, never guessed at.
fn load_locked_lock(loaded: &Loaded) -> Result<Lock> {
    let lock = loaded.load_lock()?.ok_or_else(|| {
        anyhow!(
            "{}",
            msg!("cli.apply.locked_lock_missing", file = LOCK_FILE)
        )
    })?;
    let manifest_text =
        std::fs::read_to_string(&loaded.path).with_context(|| loaded.path.display().to_string())?;
    if !lock.matches_manifest(&manifest_text) {
        bail!("{}", msg!("cli.apply.locked_lock_stale", file = LOCK_FILE));
    }
    Ok(lock)
}

/// Timeout for the one-shot reachability check below: short enough that a
/// dead snapshot does not stall `apply` for long, generous enough for a slow
/// but working connection.
const SNAPSHOT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Drop a locked snapshot that no longer answers, announcing it first (never
/// silently, derived principle 4): `plan_apply`'s existing degrade-to-live
/// path (spec §3.2) then takes over exactly as it does for a lock recorded
/// without a snapshot at all. Real network I/O — exercised only by an actual
/// non-dry-run `apply --locked`, which this repository's tests never run
/// (safety constraint); covered here only up to the point tests can reach.
fn degrade_unreachable_snapshots(loaded: &Loaded, mut lock: Lock) -> Lock {
    let Some(codename) = ubuntu_codename(&loaded.manifest.ubuntu()) else {
        return lock;
    };
    let Some(native) = lock.provider.native.as_mut() else {
        return lock;
    };
    if native.ros_snapshot.is_none() && native.ubuntu_snapshot.is_none() {
        return lock;
    }
    println!("{}", tr("cli.apply.checking_snapshot"));
    let distro = loaded.manifest.project.ros_distro;
    if let Some(date) = native.ros_snapshot.clone() {
        let base = rospm_core::snapshot::ros_snapshot_url(distro.as_str(), &date);
        if !probe_snapshot(&base, codename) {
            println!("{}", tr("cli.apply.snapshot_unreachable_warn"));
            native.ros_snapshot = None;
        }
    }
    if let Some(ts) = native.ubuntu_snapshot.clone() {
        let base = rospm_core::snapshot::ubuntu_snapshot_url(&ts);
        if !probe_snapshot(&base, codename) {
            println!("{}", tr("cli.apply.snapshot_unreachable_warn"));
            native.ubuntu_snapshot = None;
        }
    }
    lock
}

/// `GET <base>/dists/<codename>/InRelease` with [`SNAPSHOT_PROBE_TIMEOUT`].
fn probe_snapshot(base: &str, codename: &str) -> bool {
    let endpoint = net::Endpoint {
        role: Role::RosApt,
        source: "snapshot".into(),
        url: rospm_core::snapshot::inrelease_url(base, codename),
    };
    net::probe(&[endpoint], SNAPSHOT_PROBE_TIMEOUT)
        .into_iter()
        .next()
        .is_some_and(|r| r.ok())
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
        "{}",
        msg!(
            "cli.apply.network_check",
            count = endpoints.len(),
            secs = net::DEFAULT_TIMEOUT.as_secs()
        )
    );
    let results = net::probe(&endpoints, net::DEFAULT_TIMEOUT);
    print_probe_table(&results);

    for r in results
        .iter()
        .filter(|r| r.endpoint.role == Role::GitHost && !r.ok())
    {
        eprintln!(
            "{}",
            msg!("cli.apply.git_host_unreachable", host = r.endpoint.source)
        );
    }

    if let Some(mr) = requested {
        return match net::mirror_ok(&results, &mr, codename) {
            Some(true) => {
                println!(
                    "{}",
                    msg!("cli.apply.sources_line", name = mr.name, label = mr.label)
                );
                Ok(mr)
            }
            _ if mr.is_official() => {
                eprintln!("{}", tr("cli.apply.official_forced"));
                Ok(mr)
            }
            _ => bail!(
                "{}",
                msg!(
                    "cli.apply.mirror_unreachable",
                    name = mr.name,
                    names = mirror::NAMES.join(" | ")
                )
            ),
        };
    }

    match net::verdict(&results, codename) {
        Verdict::OfficialOk => {
            println!(
                "{}",
                msg!(
                    "cli.apply.sources_line",
                    name = mirror::OFFICIAL.name,
                    label = mirror::OFFICIAL.label
                )
            );
            Ok(mirror::OFFICIAL)
        }
        Verdict::UseMirror { failed, candidates } => {
            println!("{}", tr("cli.apply.official_unreachable"));
            for f in &failed {
                println!("  - {f}");
            }
            let best = candidates[0];
            let names: Vec<&str> = candidates.iter().map(|c| c.name).collect();
            println!(
                "{}",
                msg!("cli.apply.mirrors_answered", names = names.join(", "))
            );
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                bail!(
                    "{}",
                    msg!(
                        "cli.apply.need_mirror_flag",
                        best = best.name,
                        names = names.join(" | ")
                    )
                );
            }
            eprint!(
                "{}",
                msg!(
                    "cli.apply.confirm_mirror",
                    name = best.name,
                    label = best.label
                )
            );
            let mut answer = String::new();
            // EOF (Ctrl-D) reads 0 bytes and must not count as the empty "yes".
            let got_line = stdin.read_line(&mut answer)? > 0;
            if got_line && matches!(answer.trim(), "" | "y" | "Y" | "yes") {
                println!(
                    "{}",
                    msg!(
                        "cli.apply.sources_line",
                        name = best.name,
                        label = best.label
                    )
                );
                Ok(best)
            } else {
                bail!("{}", tr("cli.apply.aborted"))
            }
        }
        Verdict::NothingReachable { failed } => {
            bail!(
                "{}",
                msg!("cli.apply.nothing_reachable", count = failed.len())
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
        let role = tr(match r.endpoint.role {
            Role::RosApt => "cli.apply.role.ros_apt",
            Role::Rosdistro => "cli.apply.role.rosdistro",
            Role::GitHost => "cli.apply.role.git_host",
        });
        println!(
            "  {} {} {:<9} {}",
            i18n::pad(&r.describe().to_string(), 34),
            i18n::pad(&role, 9),
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
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
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

pub fn build(
    global: &Global,
    workspace: Option<&str>,
    conda_env: Option<String>,
    conda_prefix: Option<PathBuf>,
    dry_run: bool,
) -> Result<()> {
    let mut loaded = load(global)?;
    if conda_env.is_some() || conda_prefix.is_some() {
        let mut conda = CondaBuild {
            name: conda_env,
            prefix: conda_prefix,
            managed: None,
        };
        // Validate before resolving: an empty CLI path must not become cwd.
        conda.validate()?;
        if let Some(prefix) = &mut conda.prefix {
            if prefix.is_relative() {
                *prefix = std::env::current_dir()
                    .context(tr("cli.err.no_cwd"))?
                    .join(&*prefix);
            }
        }
        loaded.manifest.build.conda = Some(conda);
        loaded.manifest.validate()?;
    }
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------- deps

#[derive(Serialize)]
struct DepsCheckReport {
    ok: bool,
    workspaces: Vec<rospm_core::provider::native::deps::WorkspaceDeps>,
    internal: Vec<String>,
    external: Vec<String>,
    skipped: Vec<String>,
    environment: EnvSummary,
}

fn join_or_none<'a>(keys: impl IntoIterator<Item = &'a String>) -> String {
    let joined = keys.into_iter().cloned().collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        tr("cli.deps.none")
    } else {
        joined
    }
}

fn print_deps_environment(r: &EnvReport) {
    println!("{}", tr("cli.deps.env_header"));
    if r.items.is_empty() {
        println!("{}", tr("cli.deps.env_empty"));
        return;
    }
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
        println!("{}", tr("cli.deps.ready"));
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
        println!("{}", msg!("cli.deps.not_ready", summary = parts.join(", ")));
    }
}

/// `rospm deps check`: rosdep keys of every workspace versus the installed set.
pub fn deps_check(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let report = provider::for_manifest(&loaded.manifest).probe_deps(&ctx)?;

    if global.json {
        let out = DepsCheckReport {
            ok: true,
            workspaces: report.workspaces,
            internal: report.internal.into_iter().collect(),
            external: report.external.into_iter().collect(),
            skipped: report.skipped.into_iter().collect(),
            environment: EnvSummary {
                probed: true,
                ready: report.environment.ready(),
                problems: report.environment.problems(),
                unknown: report.environment.unknown(),
                items: report.environment.items,
            },
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    for ws in &report.workspaces {
        println!(
            "{}",
            msg!(
                "cli.deps.workspace_line",
                name = ws.name,
                packages = ws.packages,
                keys = ws.keys
            )
        );
    }
    println!(
        "{}",
        msg!(
            "cli.deps.internal_line",
            keys = join_or_none(&report.internal)
        )
    );
    println!(
        "{}",
        msg!(
            "cli.deps.external_line",
            keys = join_or_none(&report.external)
        )
    );
    if !report.skipped.is_empty() {
        println!(
            "{}",
            msg!(
                "cli.deps.skipped_line",
                keys = join_or_none(&report.skipped)
            )
        );
    }
    println!();
    print_deps_environment(&report.environment);
    Ok(())
}

/// `rospm deps install [--dry-run]`: hand the missing keys to rosdep.
pub fn deps_install(global: &Global, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let mirror = requested_mirror(None)?.unwrap_or(mirror::OFFICIAL);
    let ctx = context(&loaded, &host, false, None, false, mirror)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_deps_install(&ctx)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

pub fn env(global: &Global, shell: Shell) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, shell)?;
    print!("{script}");
    Ok(())
}

pub fn shell(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, Shell::Bash)?;

    let env_dir = loaded.root.join(".rospm");
    std::fs::create_dir_all(&env_dir)?;
    let env_file = env_dir.join("env.sh");
    std::fs::write(&env_file, &script)?;

    // Interactive bash that sources our script after the user's rc files.
    let rc = format!(
        // PS1 expands $ROSPM_PROJECT at display time without re-parsing its value.
        "[ -f ~/.bashrc ] && source ~/.bashrc\nsource {}\nexport ROSPM_PROJECT={}\nPS1='(rospm:$ROSPM_PROJECT) '\"$PS1\"\n",
        rospm_core::plan::shell_quote(&env_file.to_string_lossy()),
        rospm_core::plan::shell_quote(&loaded.manifest.project.name),
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

/// System default (`/etc/default/locale`, what `apply` fixes) versus the
/// current shell (the caller's choice, left alone).
#[derive(Serialize)]
struct LocaleReport {
    system_file: bool,
    system: Option<String>,
    /// `None` when there is no readable system file.
    system_utf8: Option<bool>,
    shell: Option<String>,
    shell_utf8: bool,
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
    locale: LocaleReport,
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
        locale: LocaleReport {
            system_file: host.locale.system_file,
            system: host.locale.system.clone(),
            system_utf8: host.locale.system_utf8(),
            shell: host.locale.process.clone(),
            shell_utf8: host.locale.process_utf8(),
        },
        rosdep_initialized: host.rosdep.initialized,
        rosdep_updated: host.rosdep.updated,
        tools,
        network,
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
        let system_value = report.locale.system.as_deref().unwrap_or("POSIX");
        let shell_value = report.locale.shell.as_deref().unwrap_or("POSIX");
        println!(
            "{}",
            msg!(
                "cli.doctor.locale_system",
                value = match report.locale.system_utf8 {
                    Some(true) => format!("{system_value} ({DEFAULT_LOCALE_FILE})"),
                    Some(false) => msg!(
                        "cli.doctor.not_utf8",
                        value = system_value,
                        file = DEFAULT_LOCALE_FILE
                    )
                    .to_string(),
                    None =>
                        msg!("cli.doctor.locale_no_file", file = DEFAULT_LOCALE_FILE).to_string(),
                }
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.locale_shell",
                value = if report.locale.shell_utf8 {
                    shell_value.to_string()
                } else {
                    msg!("cli.doctor.shell_not_utf8", value = shell_value).to_string()
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
        match &report.network {
            Some(results) => {
                println!(
                    "{}",
                    msg!(
                        "cli.doctor.network_header",
                        secs = net::DEFAULT_TIMEOUT.as_secs()
                    )
                );
                print_probe_table(results);
            }
            None => println!("{}", tr("cli.doctor.network_skipped")),
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

// ---------------------------------------------------------------- verify

#[derive(Serialize)]
struct VerifyReport {
    ok: bool,
    manifest: String,
    lock: String,
    generated_at: String,
    manifest_matches: bool,
    ros_snapshot: Option<String>,
    ubuntu_snapshot: Option<String>,
    environment: EnvSummary,
}

/// `rospm verify`: compare `rospm.lock` with the host (spec §4, read-only).
pub fn verify(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let lock = loaded
        .load_lock()?
        .ok_or_else(|| Error::Host(msg!("cli.verify.no_lock", file = LOCK_FILE)))?;
    let manifest_text =
        std::fs::read_to_string(&loaded.path).map_err(|e| Error::io(loaded.path.as_path(), e))?;
    let manifest_matches = lock.matches_manifest(&manifest_text);

    let host = Host::probe();
    let ctx = context(&loaded, &host, false, Some(&lock), false, mirror::OFFICIAL)?;

    let mut report = EnvReport::default();
    report.push(
        "manifest",
        Message::text(MANIFEST_FILE),
        lock.manifest_sha256.clone(),
        if manifest_matches {
            msg!("cli.verify.manifest_ok")
        } else {
            msg!("cli.verify.manifest_changed")
        },
        if manifest_matches {
            Status::Ok
        } else {
            Status::Mismatch
        },
    );
    let core_report = provider::for_manifest(&loaded.manifest).verify(&ctx, &lock)?;
    report.items.extend(core_report.items);

    let native = lock.provider.native.as_ref();
    let out = VerifyReport {
        ok: true,
        manifest: loaded.path.display().to_string(),
        lock: loaded.lock_path().display().to_string(),
        generated_at: lock.generated_at.clone(),
        manifest_matches,
        ros_snapshot: native.and_then(|n| n.ros_snapshot.clone()),
        ubuntu_snapshot: native.and_then(|n| n.ubuntu_snapshot.clone()),
        environment: EnvSummary {
            probed: true,
            ready: report.ready(),
            problems: report.problems(),
            unknown: report.unknown(),
            items: report.items.clone(),
        },
    };

    if global.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("{}", msg!("cli.verify.manifest_line", path = &out.manifest));
        println!("{}", msg!("cli.verify.lock_line", path = &out.lock));
        println!();
        print_env_report(&report, &ctx.platform.to_string());
    }

    if !out.environment.ready {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_templates_are_valid_manifests() {
        for (lang, text) in [("en", TEMPLATE_EN), ("zh-CN", TEMPLATE_ZH_CN)] {
            for distro in RosDistro::ALL {
                let text = text
                    .replace("@NAME@", "demo")
                    .replace("@DISTRO@", distro.as_str())
                    .replace("@UBUNTU@", distro.default_ubuntu());
                let m = Manifest::parse(&text, Path::new("rospm.toml"))
                    .unwrap_or_else(|e| panic!("{lang} template is invalid: {e}"));
                assert_eq!(m.project.ros_distro, distro, "{lang}");
                assert!(
                    text.contains(TEMPLATE_ROS_BASE_LINE),
                    "{lang} template lost the ros_base line `--base` rewrites"
                );
                let package = RosFlavor::Desktop.package(distro);
                let with_base = set_template_ros_base(&text, &package);
                let m = Manifest::parse(&with_base, Path::new("rospm.toml"))
                    .unwrap_or_else(|e| panic!("{lang} template with --base is invalid: {e}"));
                assert_eq!(m.ros_base_package(), package, "{lang}");
            }
        }
    }

    #[test]
    fn template_distro_defaults_follow_ubuntu() {
        for (release, want) in [
            ("22.04", RosDistro::Humble),
            ("24.04", RosDistro::Jazzy),
            ("26.04", RosDistro::Jazzy),
        ] {
            let os = OsRelease::parse(&format!("ID=ubuntu\nVERSION_ID={release}\n"));
            assert_eq!(default_distro(Some(&os)), want);
        }
        assert_eq!(default_distro(None), RosDistro::Jazzy);
        assert_eq!(
            default_distro(Some(&OsRelease::parse("ID=debian\nVERSION_ID=22.04"))),
            RosDistro::Jazzy
        );
    }

    #[test]
    fn distro_switch_prepares_manifest_and_matching_plan_without_writing() {
        let dir = std::env::temp_dir().join(format!("rospm-distro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE);
        let original = set_template_ros_base(
            &TEMPLATE_EN
                .replace("@NAME@", "demo")
                .replace("@DISTRO@", "jazzy")
                .replace("@UBUNTU@", "24.04"),
            "ros-jazzy-desktop",
        )
        .replace("# ubuntu =", "ubuntu =");
        std::fs::write(&path, &original).unwrap();
        for distro in [RosDistro::Humble, RosDistro::Rolling] {
            let mut loaded = Loaded {
                path: path.clone(),
                root: dir.clone(),
                manifest: Manifest::parse(&original, &path).unwrap(),
            };
            let mut host = Host::empty();
            host.os = Some(OsRelease::parse(&format!(
                "ID=ubuntu\nVERSION_ID={}\n",
                distro.default_ubuntu()
            )));
            host.ros_distros = vec!["jazzy".into()];
            let text = prepare_distro(&mut loaded, &host, Some(distro))
                .unwrap()
                .unwrap();
            assert_eq!(loaded.manifest.project.ros_distro, distro);
            assert_eq!(
                loaded.manifest.ros_base_package(),
                format!("ros-{distro}-desktop")
            );
            assert_eq!(loaded.manifest.ubuntu(), distro.default_ubuntu());
            assert!(text.contains("# humble | jazzy | rolling"));
            assert!(text.contains("# defaults to the distro's release"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL).unwrap();
            let plan = provider::for_manifest(&loaded.manifest)
                .plan_apply(&ctx)
                .unwrap()
                .render();
            assert!(
                plan.contains(&format!(
                    " {} main",
                    ubuntu_codename(distro.default_ubuntu()).unwrap()
                )),
                "{plan}"
            );
            assert!(
                plan.contains(&format!("apt-get install -y ros-{distro}-desktop")),
                "{plan}"
            );
            assert!(plan.contains("native.apply.other_distros_kept"));

            // Unknown or incompatible hosts must fail before touching even the in-memory manifest.
            for os in [None, Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=20.04"))] {
                host.os = os;
                let before = loaded.manifest.project.ros_distro;
                assert!(prepare_distro(&mut loaded, &host, Some(RosDistro::Jazzy)).is_err());
                assert_eq!(loaded.manifest.project.ros_distro, before);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            }
        }
        let custom = original.replace("ros-jazzy-desktop", "custom-ros");
        std::fs::write(&path, &custom).unwrap();
        let mut loaded = Loaded {
            path: path.clone(),
            root: dir.clone(),
            manifest: Manifest::parse(&custom, &path).unwrap(),
        };
        let mut host = Host::empty();
        host.os = Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=22.04"));
        prepare_distro(&mut loaded, &host, Some(RosDistro::Humble)).unwrap();
        assert_eq!(loaded.manifest.ros_base_package(), "custom-ros");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
