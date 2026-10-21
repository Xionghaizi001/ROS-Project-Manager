//! Install a pinned Miniforge into the per-user directory rospm owns, so a
//! managed environment can be prepared on a host without Conda. The installer
//! is chosen and verified only from the table bundled into the binary; it runs
//! in batch mode as the current user, which neither needs sudo nor edits shell
//! startup files, and rospm never runs `conda init`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use indexmap::IndexMap;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::System;
use crate::error::{Error, Result};
use crate::msg;
use crate::pixi::conda_platform;
use crate::plan::Command;
use crate::platform::Platform;
use crate::probe::CondaHost;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Miniforge {
    pub schema_version: u32,
    pub version: String,
    pub minimum_conda: String,
    pub installer: IndexMap<Platform, Installer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Installer {
    pub url: String,
    pub sha256: String,
}

/// Read once from the data bundled into the binary; never downloads or writes.
pub fn miniforge() -> &'static Miniforge {
    static TABLE: OnceLock<Miniforge> = OnceLock::new();
    TABLE.get_or_init(|| {
        let table: Miniforge = toml::from_str(include_str!("../../data/miniforge.toml"))
            .expect("bundled Miniforge table must be valid");
        assert_eq!(table.schema_version, 1, "unsupported Miniforge schema");
        assert!(version(&table.minimum_conda).is_some());
        for installer in table.installer.values() {
            assert!(installer.url.starts_with("https://"));
            assert!(
                installer.sha256.len() == 64
                    && installer.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            );
        }
        table
    })
}

/// `major.minor` of `conda --version` output (`conda 24.11.3`) or of a bare
/// version string.
fn version(text: &str) -> Option<(u32, u32)> {
    let word = text.split_whitespace().last()?;
    let mut parts = word.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// Whether `conda --version` output is at least the bundled minimum.
pub fn compatible(output: &str) -> bool {
    let minimum = version(&miniforge().minimum_conda).expect("validated minimum");
    version(output).is_some_and(|found| found >= minimum)
}

#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub version: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Installation prefix, one per Miniforge version.
    pub root: PathBuf,
    /// Written only after a successful install; holds the installer digest.
    ready: PathBuf,
    /// Verified installer, reused across runs and projects.
    pub cache: PathBuf,
}

impl Bootstrap {
    /// `None` when no installer is pinned for `platform`.
    pub fn new(host: &CondaHost, platform: Platform) -> Option<Self> {
        let table = miniforge();
        let installer = table.installer.get(&platform)?;
        // Per subdir: a shared (e.g. NFS) home serves hosts of several architectures.
        let name = format!("miniforge3-{}-{}", table.version, conda_platform(platform));
        let file = installer.url.rsplit('/').next().expect("installer file");
        Some(Self {
            version: &table.version,
            url: &installer.url,
            sha256: &installer.sha256,
            root: host.data_dir.join("conda").join(&name),
            ready: host.data_dir.join("conda").join(format!("{name}.ready")),
            cache: host.cache_dir.join("downloads").join(file),
        })
    }

    pub fn conda(&self) -> PathBuf {
        self.root.join("bin/conda")
    }

    /// Only filesystem reads: safe while planning and previewing.
    pub fn is_ready(&self) -> bool {
        fs::read_to_string(&self.ready).is_ok_and(|s| s == self.sha256) && self.conda().is_file()
    }

    pub fn render(&self) -> String {
        msg!(
            "conda.bootstrap",
            minimum = miniforge().minimum_conda,
            version = self.version,
            url = self.url,
            sha256 = self.sha256,
            cache = self.cache.display(),
            root = self.root.display()
        )
        .to_string()
    }

    pub(crate) fn install(&self, out: &mut dyn Write, system: &dyn System) -> Result<()> {
        let say = |out: &mut dyn Write, text: crate::Message| {
            writeln!(out, "{text}").map_err(|e| Error::io("<stdout>", e))
        };
        // The cache is rospm's own: a file with another digest (truncated,
        // tampered, or from an older table) is replaced, never executed.
        if fs::metadata(&self.cache).is_ok() {
            if file_sha256(&self.cache)? == self.sha256 {
                say(
                    out,
                    msg!("conda.installer_cached", path = self.cache.display()),
                )?;
            } else {
                fs::remove_file(&self.cache).map_err(|e| Error::io(&self.cache, e))?;
            }
        }
        if !self.cache.is_file() {
            say(out, msg!("conda.downloading", url = self.url))?;
            let parent = self.cache.parent().expect("cache file has a parent");
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            let part = self.cache.with_extension("sh.part");
            let downloaded = system.download(self.url, &part).and_then(|()| {
                let actual = file_sha256(&part)?;
                if actual != self.sha256 {
                    return Err(Error::Host(msg!(
                        "conda.installer_digest_mismatch",
                        url = self.url,
                        expected = self.sha256,
                        actual = actual
                    )));
                }
                fs::rename(&part, &self.cache).map_err(|e| Error::io(&self.cache, e))
            });
            if downloaded.is_err() {
                let _ = fs::remove_file(&part);
            }
            downloaded?;
        }

        // Anything at the prefix without a completion record is an
        // interrupted rospm install of this version: start over.
        if self.ready.exists() {
            fs::remove_file(&self.ready).map_err(|e| Error::io(&self.ready, e))?;
        }
        if fs::symlink_metadata(&self.root).is_ok() {
            fs::remove_dir_all(&self.root).map_err(|e| Error::io(&self.root, e))?;
        }
        let parent = self.root.parent().expect("install prefix has a parent");
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        say(
            out,
            msg!(
                "conda.installing",
                version = self.version,
                root = self.root.display()
            ),
        )?;
        system.run(&installer_command(&self.cache, &self.root, parent))?;

        let conda = self.conda();
        let found = system.query(&conda.to_string_lossy(), &["--version"])?;
        if !compatible(&found) {
            return Err(Error::Host(msg!(
                "conda.bootstrap_incomplete",
                root = self.root.display(),
                found = found.trim()
            )));
        }
        fs::write(&self.ready, self.sha256).map_err(|e| Error::io(&self.ready, e))?;
        say(out, msg!("conda.installed", conda = conda.display()))
    }
}

/// `-b` is the installer's batch mode: no prompts, no license pager and no
/// shell startup file changes. The bash run is non-login and skips rc files.
fn installer_command(installer: &Path, root: &Path, cwd: &Path) -> Command {
    Command::new("bash")
        .args(["--noprofile", "--norc"])
        .arg(installer.to_string_lossy())
        .args(["-b", "-p"])
        .arg(root.to_string_lossy())
        .cwd(cwd)
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| Error::io(path, e))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|e| Error::io(path, e))?;
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_table_pins_both_platforms_from_the_official_release() {
        let table = miniforge();
        for platform in Platform::ALL {
            let installer = &table.installer[&platform];
            assert!(installer.url.starts_with(&format!(
                "https://github.com/conda-forge/miniforge/releases/download/{}/",
                table.version
            )));
            let arch = match platform {
                Platform::LinuxX86_64 => "x86_64",
                Platform::LinuxAarch64 => "aarch64",
            };
            assert!(installer.url.ends_with(&format!("-Linux-{arch}.sh")));
        }
    }

    #[test]
    fn version_check_compares_major_minor_numerically() {
        assert!(compatible("conda 24.3.0\n"));
        assert!(compatible("conda 26.7.2"));
        assert!(compatible("24.11"));
        assert!(!compatible("conda 24.1.2"));
        assert!(!compatible("conda 4.12.0"));
        assert!(!compatible("conda"));
        assert!(!compatible(""));
    }
}
