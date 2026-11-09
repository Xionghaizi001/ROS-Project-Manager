//! Explicit native execution environments; never creates or installs anything.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::plan::shell_quote;
use crate::provider::Context;
use crate::{msg, Error, Result};

pub fn resolve(path: &Path, cwd: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    if text.trim().is_empty() || text.contains(['\0', '\n', '\r']) {
        return Err(Error::Host(msg!("venv.invalid_path")));
    }
    let input = cwd.join(path);
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
    resolve(path, ctx.root).map(Some)
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
