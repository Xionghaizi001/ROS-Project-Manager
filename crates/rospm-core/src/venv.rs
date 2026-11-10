//! Explicit native execution environments; never creates or installs anything.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::plan::shell_quote;
use crate::provider::Context;
use crate::{msg, Error, Result};

pub fn input_path(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    if text.trim().is_empty() || text.contains(['\0', '\n', '\r']) {
        return Err(Error::Host(msg!("venv.invalid_path")));
    }
    Ok(cwd.join(path))
}

pub fn resolve(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let input = input_path(path, cwd)?;
    let target = input.canonicalize().map_err(|e| Error::io(&input, e))?;
    if target.to_string_lossy().contains(['\n', '\r']) {
        return Err(Error::Host(msg!("venv.invalid_path")));
    }
    let cfg = target.join("pyvenv.cfg");
    let contents = std::fs::read_to_string(&cfg).map_err(|e| Error::io(&cfg, e))?;
    if target.join("conda-meta").exists()
        || !contents.lines().any(|line| {
            line.split_once('=').is_some_and(|(k, v)| {
                k.trim()
                    .eq_ignore_ascii_case("include-system-site-packages")
                    && v.trim().eq_ignore_ascii_case("true")
            })
        })
        || !std::fs::metadata(target.join("bin/python"))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    {
        return Err(Error::Host(msg!("venv.invalid", path = input.display())));
    }
    // Keep the input path so previews show symlinks as well as their targets.
    Ok(input)
}

pub(crate) fn selected(ctx: &Context) -> Result<Option<PathBuf>> {
    let Some(path) = ctx.venv else {
        return Ok(None);
    };
    validate_context(ctx)?;
    resolve(path, ctx.root).map(Some)
}

fn validate_context(ctx: &Context) -> Result<()> {
    if ctx.manifest.environment.provider != crate::manifest::ProviderKind::Native
        || ctx.host.platform != Some(ctx.platform)
        || !ctx.manifest.project.platforms.contains(&ctx.platform)
        || !ctx.host.os.as_ref().is_some_and(|os| {
            os.is_ubuntu() && os.version_id.as_deref() == Some(ctx.manifest.ubuntu().as_str())
        })
    {
        return Err(Error::Host(msg!("venv.unsupported")));
    }
    if ctx.manifest.env.keys().any(|key| {
        matches!(
            key.as_str(),
            "PATH" | "VIRTUAL_ENV" | "PYTHONHOME" | "PYTHONPATH" | "PYTHONUSERBASE"
        ) || key.starts_with("CONDA_")
            || key.starts_with("PYTHON")
            || key.starts_with("VIRTUAL_ENV_")
    }) {
        return Err(Error::Host(msg!("venv.env_conflict")));
    }
    Ok(())
}

/// Start from system search paths, then source ROS/overlays, then validate.
pub(crate) fn activation(path: &Path) -> Result<String> {
    let target = path.canonicalize().map_err(|e| Error::io(path, e))?;
    Ok(format!(
        r#"# venv: {} -> {}
unset PYTHONINSPECT PYTHONSTARTUP PYTHONEXECUTABLE PYTHONPLATLIBDIR PYTHONHOME PYTHONPATH PYTHONUSERBASE VIRTUAL_ENV VIRTUAL_ENV_PROMPT _CE_CONDA _CE_M LD_LIBRARY_PATH CMAKE_PREFIX_PATH AMENT_PREFIX_PATH COLCON_PREFIX_PATH
for _rospm_var in $(/usr/bin/env | /usr/bin/sed -n 's/^\(CONDA_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$_rospm_var"; done
unset _rospm_var
export VIRTUAL_ENV={}
export PYTHONNOUSERSITE=1
export PATH={}
"#,
        path.display(),
        target.display(),
        shell_quote(&target.to_string_lossy()),
        shell_quote(&format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            target.join("bin").display()
        )),
    ))
}

pub(crate) fn check(path: &Path) -> String {
    format!(
        "{} -s -c {} {} || {{ return 1 2>/dev/null || exit 1; }}\n",
        shell_quote(&path.join("bin/python").to_string_lossy()),
        shell_quote(include_str!("venv_check.py")),
        shell_quote(&path.to_string_lossy())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn real_system_venv_cleans_inherited_environment_and_checks_runtime() {
        let temp = std::env::temp_dir().join(format!("rospm-venv-venv-1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let path = temp.as_path().join("env with 'quotes'");
        let created = Command::new("/usr/bin/python3")
            .args([
                "-I",
                "-m",
                "venv",
                "--without-pip",
                "--system-site-packages",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(created.success());
        assert_eq!(resolve(&path, temp.as_path()).unwrap(), path);
        let link = temp.as_path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(resolve(&link, temp.as_path()).unwrap(), link);
        // Stub only the ROS imports; this test does not claim ROS ABI acceptance.
        let modules = temp.as_path().join("modules");
        std::fs::create_dir_all(modules.join("rclpy")).unwrap();
        for name in [
            "catkin_pkg.py",
            "em.py",
            "rclpy/__init__.py",
            "rclpy/_rclpy_pybind11.py",
        ] {
            std::fs::write(modules.join(name), "").unwrap();
        }
        let script = format!("set -e\n{}\ntest -z \"${{CONDA_PREFIX-}}\"\ntest -z \"${{PYTHONHOME-}}\"\ntest -z \"${{CONDA_PREFIX_1-}}\"\nexport PYTHONPATH={}\n{}",
            activation(&link).unwrap(), shell_quote(&modules.to_string_lossy()), check(&link));
        let result = Command::new("bash")
            .args(["--noprofile", "--norc", "-c", &script])
            .env("CONDA_PREFIX", "/foreign/conda")
            .env("CONDA_PREFIX_1", "/foreign/base")
            .env("VIRTUAL_ENV", "/foreign/venv")
            .env("PYTHONHOME", "/invalid")
            .env("PYTHONPATH", "/foreign/modules")
            .env("PATH", "/foreign/bin:/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::remove_file(modules.join("em.py")).unwrap();
        std::fs::remove_dir_all(modules.join("__pycache__")).unwrap();
        // Missing ROS extension must fail even if the caller did not enable errexit.
        std::fs::remove_file(modules.join("rclpy/_rclpy_pybind11.py")).unwrap();
        let failed = Command::new("bash")
            .args([
                "-c",
                &format!(
                    "{}\nexport PYTHONPATH={}\n{}\necho should-not-run",
                    activation(&path).unwrap(),
                    shell_quote(&modules.to_string_lossy()),
                    check(&path)
                ),
            ])
            .output()
            .unwrap();
        assert!(!failed.status.success());
        assert!(!String::from_utf8_lossy(&failed.stdout).contains("should-not-run"));
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn invalid_paths_and_conda_or_isolated_environments_are_rejected() {
        let temp = std::env::temp_dir().join(format!("rospm-venv-venv-2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        for path in ["", "  ", "bad\npath", "bad\0path", "missing"] {
            assert!(resolve(Path::new(path), temp.as_path()).is_err());
        }
        std::fs::create_dir(temp.as_path().join("bin")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/python3", temp.as_path().join("bin/python")).unwrap();
        let cfg = temp.as_path().join("pyvenv.cfg");
        std::fs::write(&cfg, "include-system-site-packages = false\n").unwrap();
        assert!(resolve(temp.as_path(), temp.as_path()).is_err());
        std::fs::write(&cfg, "include-system-site-packages = true\n").unwrap();
        std::fs::create_dir(temp.as_path().join("conda-meta")).unwrap();
        assert!(resolve(temp.as_path(), temp.as_path()).is_err());
        std::fs::remove_dir_all(&temp).unwrap();
    }
}

/// Read-only readiness and cache report shared by CLI and future frontends.
pub fn probe(ctx: &Context) -> crate::probe::EnvReport {
    use crate::probe::{EnvReport, Status};
    let mut report = EnvReport::default();
    let Some(input) = ctx.venv else { return report };
    let path = match selected(ctx) {
        Ok(Some(path)) => path,
        Ok(None) => return report,
        Err(error) => {
            report.push(
                "venv",
                input.display().to_string(),
                msg!("venv.ready"),
                error.to_string(),
                if !input.exists() {
                    Status::Missing
                } else {
                    Status::Mismatch
                },
            );
            return report;
        }
    };
    let setup = format!("/opt/ros/{}/setup.bash", ctx.manifest.project.ros_distro);
    let script = activation(&path).map(|activation| {
        format!(
            "set -e\n{activation}\nexport PYTHONDONTWRITEBYTECODE=1\nsource {}\n{}",
            shell_quote(&setup),
            check(&path)
        )
    });
    let output = script.and_then(|script| {
        std::process::Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script])
            .env_remove("BASH_ENV")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .current_dir(ctx.root)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| Error::io("/bin/bash", e))
    });
    match output {
        Ok(out) if out.status.success() => report.push(
            "venv",
            path.display().to_string(),
            msg!("venv.ready"),
            msg!(
                "venv.resolved",
                path = path.canonicalize().unwrap_or(path.clone()).display()
            ),
            Status::Ok,
        ),
        Ok(out) => report.push(
            "venv",
            path.display().to_string(),
            msg!("venv.ready"),
            msg!(
                "venv.runtime_failed",
                detail = String::from_utf8_lossy(&out.stderr).trim()
            ),
            Status::Mismatch,
        ),
        Err(error) => report.push(
            "venv",
            path.display().to_string(),
            msg!("venv.ready"),
            error.to_string(),
            Status::Unknown,
        ),
    }
    for (name, ws) in &ctx.manifest.workspaces {
        let bases = crate::provider::native::colcon_bases(ctx.root, ws, "build-base", "build");
        let command = cache_command(&path, &bases);
        match std::process::Command::new(&command.program)
            .args(&command.args)
            .stdin(std::process::Stdio::null())
            .output()
        {
            Ok(out) if out.status.success() => report.push(
                "venv_cache",
                name,
                msg!("venv.cache_expected"),
                msg!("venv.cache_ok"),
                Status::Ok,
            ),
            Ok(out) => report.push(
                "venv_cache",
                name,
                msg!("venv.cache_expected"),
                msg!(
                    "venv.cache_failed",
                    detail = String::from_utf8_lossy(&out.stderr).trim()
                ),
                Status::Mismatch,
            ),
            Err(error) => report.push(
                "venv_cache",
                name,
                msg!("venv.cache_expected"),
                error.to_string(),
                Status::Unknown,
            ),
        }
    }
    report
}

pub(crate) fn cache_command(path: &Path, bases: &[PathBuf]) -> crate::plan::Command {
    crate::plan::Command::new("/usr/bin/python3")
        .args(["-I", "-B", "-c", include_str!("venv_cache.py")])
        .arg(path.join("bin/python").to_string_lossy())
        .args(bases.iter().map(|p| p.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn caches_distinguish_venvs_sharing_system_python_and_allow_directory_aliases() {
        let root = std::env::temp_dir().join(format!("rospm-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for name in ["selected", "other"] {
            std::fs::create_dir_all(root.join(name).join("bin")).unwrap();
            std::os::unix::fs::symlink("/usr/bin/python3", root.join(name).join("bin/python"))
                .unwrap();
        }
        std::os::unix::fs::symlink(root.join("selected"), root.join("alias")).unwrap();
        let base = root.join("custom build/nested/pkg");
        std::fs::create_dir_all(&base).unwrap();
        let cache = base.join("CMakeCache.txt");
        let command = cache_command(&root.join("selected"), &[root.join("custom build")]);
        let run = || {
            std::process::Command::new(&command.program)
                .args(&command.args)
                .output()
                .unwrap()
        };
        for value in [
            root.join("other/bin/python"),
            PathBuf::from("/usr/bin/python3"),
        ] {
            std::fs::write(
                &cache,
                format!("_Python3_EXECUTABLE:INTERNAL={}\n", value.display()),
            )
            .unwrap();
            let out = run();
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("CMakeCache.txt"));
        }
        std::fs::write(&cache, format!("Python3_EXECUTABLE:FILEPATH={}/alias/bin/python3\nPYTHON_EXECUTABLE:FILEPATH=PYTHON_EXECUTABLE-NOTFOUND\n", root.display())).unwrap();
        assert!(run().status.success());
        assert!(cache.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// Plan explicit creation of the fixed project directory. Execution never removes it.
/// Frontends must hold the project WorkspaceLock while executing this plan.
pub fn plan_create(ctx: &Context, uv: bool) -> Result<crate::plan::Plan> {
    validate_context(ctx)?;
    let target = input_path(&ctx.root.join(".venv"), ctx.root)?;
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(Error::Host(msg!("venv.exists", path = target.display())));
    }
    let tool = if uv {
        Some(which::which("uv").map_err(|e| Error::Host(msg!("venv.uv_missing", detail = e)))?)
    } else {
        None
    };
    let mut plan = crate::plan::Plan::new(msg!("venv.create_title", path = target.display()));
    let command = crate::plan::Command::new("/usr/bin/python3")
        .args(["-I", "-B", "-c", include_str!("venv_create.py")])
        .arg(target.to_string_lossy())
        .arg(format!(
            "/opt/ros/{}/setup.bash",
            ctx.manifest.project.ros_distro
        ))
        .arg(
            tool.as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
        .arg(include_str!("venv_check.py"))
        .arg(msg!("venv.create_failed", path = target.display()).to_string())
        .arg(msg!("venv.ensurepip_missing").to_string())
        .cwd(ctx.root);
    plan.note(msg!("venv.create_command", command = match tool {
        Some(tool) => format!("{} --no-config venv --python /usr/bin/python3 --no-python-downloads --system-site-packages {}", shell_quote(&tool.to_string_lossy()), shell_quote(&target.to_string_lossy())),
        None => format!("/usr/bin/python3 -m venv --system-site-packages {}", shell_quote(&target.to_string_lossy())),
    }));
    plan.run(command);
    Ok(plan)
}

#[cfg(test)]
mod create_tests {
    #[test]
    fn creation_claims_once_and_preserves_existing_or_failed_directories() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let out = std::process::Command::new("/usr/bin/python3")
            .args(["-I", "-B"])
            .arg(source.join("venv_create_test.py"))
            .arg(source.join("venv_create.py"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
