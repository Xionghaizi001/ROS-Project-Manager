//! Init command implementation.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Serialize;

use rospm_core::introspect::{self, HostFacts};
use rospm_core::manifest::{Manifest, RosFlavor, MANIFEST_FILE};
use rospm_core::msg;
use rospm_core::platform::RosDistro;
use rospm_core::probe::{Host, OsRelease};

use crate::i18n::{self, tr};

use super::Global;

/// One template per language (comments differ, keys and values do not).
const TEMPLATE_EN: &str = include_str!("../../templates/rospm.en.toml");
const TEMPLATE_ZH_CN: &str = include_str!("../../templates/rospm.zh-CN.toml");
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
}
