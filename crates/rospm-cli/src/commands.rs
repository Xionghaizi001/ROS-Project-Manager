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
use rospm_core::manifest::{Manifest, RosFlavor, MANIFEST_FILE};
use rospm_core::mirror::{self, Mirror};
use rospm_core::net::{self, ProbeResult, Role, Verdict};
use rospm_core::platform::{ubuntu_codename, Platform, RosDistro};
use rospm_core::probe::{EnvReport, Host, Status, DEFAULT_LOCALE_FILE, NATIVE_CORE_TOOLS, TOOLS};
use rospm_core::provider::{self, Context, Shell};
use rospm_core::repos::ReposFile;
use rospm_core::snapshot::{Fetch, Http};
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
) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
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
    let ctx = context(&loaded, &host, locked, None, offline, sources)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
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
            stdin.read_line(&mut answer)?;
            if matches!(answer.trim(), "" | "y" | "Y" | "yes") {
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

pub fn build(global: &Global, workspace: Option<&str>, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------- deps

/// `rospm deps check`: rosdep keys of every workspace versus the installed set.
pub fn deps_check(global: &Global) -> Result<()> {
    let _ = global;
    not_implemented("deps check", "P2")
}

/// `rospm deps install [--dry-run]`: hand the missing keys to rosdep.
pub fn deps_install(global: &Global, dry_run: bool) -> Result<()> {
    let _ = (global, dry_run);
    not_implemented("deps install", "P2")
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

/// `rospm verify`: compare `rospm.lock` with the host (spec §4, read-only).
pub fn verify(global: &Global) -> Result<()> {
    let _ = global;
    not_implemented("verify", "P2")
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
