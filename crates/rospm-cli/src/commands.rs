//! Command implementations. Anything scheduled for a later roadmap phase
//! returns [`not_implemented`] with the phase so users know where it stands.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Serialize;

use rospm_core::manifest::{Manifest, MANIFEST_FILE};
use rospm_core::platform::Platform;
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

pub fn init(global: &Global, from_env: bool, force: bool) -> Result<()> {
    if from_env {
        return not_implemented("init --from-env", "P1");
    }
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
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "my_project".into());
    let text = TEMPLATE.replace("@NAME@", &name);
    // The template must always satisfy our own validator.
    Manifest::parse(&text, &path).context("internal error: template is invalid")?;
    std::fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
    println!("created {}", path.display());
    println!("next: edit the [workspaces] section and create main.repos, then run `rospm check`");
    Ok(())
}

// ---------------------------------------------------------------- check

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
}

pub fn check(global: &Global, target: Option<Platform>) -> Result<()> {
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
    notes.push("environment probing (installed packages, rosdep state) arrives in P1".into());

    let mut effective_apt = BTreeMap::new();
    for p in &m.project.platforms {
        effective_apt.insert(p.to_string(), m.effective_apt(*p));
    }

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
    };

    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

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
    Ok(())
}

// ---------------------------------------------------------------- apply / build / env / shell

fn context<'a>(loaded: &'a Loaded, locked: bool, offline: bool) -> Result<Context<'a>> {
    Ok(Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform: host_platform()?,
        locked,
        offline,
    })
}

pub fn apply(global: &Global, locked: bool, offline: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let ctx = context(&loaded, locked, offline)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

pub fn build(global: &Global, workspace: Option<&str>, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let ctx = context(&loaded, false, false)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

pub fn env(global: &Global, shell: Shell) -> Result<()> {
    let loaded = load(global)?;
    let ctx = context(&loaded, false, false)?;
    let script = provider::for_manifest(&loaded.manifest).env_script(&ctx, shell)?;
    print!("{script}");
    Ok(())
}

pub fn shell(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let ctx = context(&loaded, false, false)?;
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
    ros_distros_installed: Vec<String>,
    tools: Vec<ToolStatus>,
}

pub fn doctor(global: &Global) -> Result<()> {
    const TOOLS: &[(&str, &str)] = &[
        ("git", "source checkout"),
        ("vcs", "vcstool: multi-repo import/export"),
        ("colcon", "workspace builds"),
        ("rosdep", "dependency resolution"),
        ("apt-get", "native provider"),
        ("dpkg-query", "native provider: installed package versions"),
        ("pixi", "pixi provider (roadmap P3)"),
        ("docker", "docker provider (roadmap P4)"),
    ];

    let tools: Vec<ToolStatus> = TOOLS
        .iter()
        .map(|(tool, purpose)| ToolStatus {
            tool: tool.to_string(),
            purpose: purpose.to_string(),
            found: which::which(tool).ok().map(|p| p.display().to_string()),
        })
        .collect();

    let mut distros: Vec<String> = std::fs::read_dir("/opt/ros")
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().join("setup.bash").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    distros.sort();

    // Core toolchain for the native provider must be present.
    let core_ok = tools
        .iter()
        .filter(|t| matches!(t.tool.as_str(), "git" | "vcs" | "colcon" | "rosdep"))
        .all(|t| t.found.is_some());

    let report = DoctorReport {
        ok: core_ok,
        platform: Platform::current().map(|p| p.to_string()),
        ros_distros_installed: distros,
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
            "ROS installs  {}",
            if report.ros_distros_installed.is_empty() {
                "none found under /opt/ros".to_string()
            } else {
                report.ros_distros_installed.join(", ")
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
            println!("core tools missing: native provider needs git, vcs, colcon and rosdep");
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}
