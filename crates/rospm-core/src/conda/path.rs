//! Opt-in shell integration for the Miniforge installed by rospm.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::{bootstrap::install_lock, Bootstrap, CondaHost, Platform};
use crate::{msg, plan::shell_quote, Error, Message, Result};

const START: &str = "\n# >>> rospm conda >>>\n";
const END: &str = "# <<< rospm conda <<<\n";

#[derive(Debug, Clone, Copy)]
pub enum PathAction {
    SetDefault,
    Remove,
}

impl PathAction {
    pub fn label(self) -> Message {
        match self {
            Self::SetDefault => msg!("conda.path.set_default"),
            Self::Remove => msg!("conda.path.remove"),
        }
    }

    pub fn available() -> Result<Option<Self>> {
        let host = CondaHost::probe();
        let config = ShellConfig::current()?;
        let files = config.files()?;
        let mut configured = false;
        for (path, _) in &files {
            let contents = read_rc(path)?;
            configured |= without_block(path, &contents)?.is_some();
        }
        Ok(if configured {
            Some(Self::Remove)
        } else if managed_conda(&host).is_some() {
            Some(Self::SetDefault)
        } else {
            None
        })
    }

    pub fn perform(self) -> Result<Message> {
        let host = CondaHost::probe();
        let config = ShellConfig::current()?;
        let block = match self {
            Self::SetDefault => {
                let conda = managed_conda(&host)
                    .ok_or_else(|| Error::Host(msg!("conda.path.not_managed")))?;
                Some(config.block(&conda)?)
            }
            Self::Remove => None,
        };
        let _lock = install_lock(&host.data_dir.join("conda/path.lock"))?;
        let mut edits = Vec::new();
        for (path, enable) in config.files()? {
            let contents = read_rc(&path)?;
            let clean = without_block(&path, &contents)?;
            let next = match &block {
                Some(block) if enable => {
                    format!("{}{block}", clean.as_deref().unwrap_or(&contents))
                }
                _ => clean.unwrap_or_else(|| contents.clone()),
            };
            if next != contents {
                edits.push((path, next));
            }
        }
        for (path, contents) in edits {
            write_rc(&path, &contents).map_err(|err| Error::io(&path, err))?;
        }
        Ok(match self {
            Self::SetDefault => msg!("conda.path.enabled"),
            Self::Remove => msg!("conda.path.disabled"),
        })
    }
}

fn managed_conda(host: &CondaHost) -> Option<PathBuf> {
    let bootstrap = Bootstrap::new(host, Platform::current()?)?;
    bootstrap.is_ready().then(|| bootstrap.conda())
}

#[derive(Clone, Copy)]
enum Shell {
    Bash,
    Zsh,
    Fish,
}

struct ShellConfig {
    shell: Shell,
    directory: PathBuf,
}

impl ShellConfig {
    fn current() -> Result<Self> {
        let home = env::home_dir()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| Error::Host(msg!("conda.path.no_home")))?;
        let shell = env::var_os("SHELL").unwrap_or_else(|| "/bin/bash".into());
        let shell = match Path::new(&shell).file_name().and_then(|name| name.to_str()) {
            Some("bash") => Shell::Bash,
            Some("zsh") => Shell::Zsh,
            Some("fish") => Shell::Fish,
            _ => {
                return Err(Error::Host(msg!(
                    "conda.path.unsupported_shell",
                    shell = shell.to_string_lossy()
                )));
            }
        };
        let directory = match shell {
            Shell::Bash => home,
            Shell::Zsh => env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or(home),
            Shell::Fish => env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
                .join("fish"),
        };
        Ok(Self { shell, directory })
    }

    fn files(&self) -> Result<Vec<(PathBuf, bool)>> {
        let names: &[&str] = match self.shell {
            Shell::Bash => &[".bashrc", ".bash_profile", ".bash_login", ".profile"],
            Shell::Zsh => &[".zshrc"],
            Shell::Fish => &["config.fish"],
        };
        let login = names
            .iter()
            .skip(1)
            .find(|name| self.directory.join(name).is_file());
        let mut files: Vec<(PathBuf, bool)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let enable = index == 0 || login.unwrap_or(&".profile") == name;
            let path = self.directory.join(name);
            let path = match path.symlink_metadata() {
                Ok(_) => path.canonicalize().map_err(|err| Error::io(&path, err))?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => path,
                Err(err) => return Err(Error::io(&path, err)),
            };
            if let Some((_, existing)) = files.iter_mut().find(|(file, _)| file == &path) {
                *existing |= enable;
            } else {
                files.push((path, enable));
            }
        }
        Ok(files)
    }

    fn block(&self, conda: &Path) -> Result<String> {
        let condabin = conda.parent().unwrap().with_file_name("condabin");
        let dir = condabin
            .to_str()
            .filter(|path| condabin.is_absolute() && !path.contains([':', '\n', '\r']))
            .ok_or_else(|| {
                Error::Host(msg!("conda.path.invalid_prefix", path = condabin.display()))
            })?;
        let exe = conda.to_str().expect("valid Conda prefix");
        let body = match self.shell {
            Shell::Bash | Shell::Zsh => {
                let (shell, guard) = match self.shell {
                    Shell::Bash => ("bash", r#"[ -n "${BASH_VERSION-}" ] && "#),
                    Shell::Zsh => ("zsh", ""),
                    Shell::Fish => unreachable!(),
                };
                let (exe, dir) = (shell_quote(exe), shell_quote(dir));
                format!(
                    "if {guard}[ -x {exe} ]; then\n\
                     \x20   unalias conda 2>/dev/null || :\n\
                     \x20   eval \"$(CONDA_AUTO_ACTIVATE_BASE=false {exe} shell.{shell} hook)\"\n\
                     \x20   case \"$PATH\" in\n\
                     \x20       {dir}|{dir}:*) ;;\n\
                     \x20       *) export PATH={dir}\"${{PATH:+:$PATH}}\" ;;\n\
                     \x20   esac\n\
                     fi\n"
                )
            }
            Shell::Fish => {
                let quote = |s: &str| format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"));
                let (exe, dir) = (quote(exe), quote(dir));
                format!(
                    "if test -x {exe}\n\
                     \x20   env CONDA_AUTO_ACTIVATE_BASE=false {exe} shell.fish hook | source\n\
                     \x20   fish_add_path --path --move {dir}\n\
                     end\n"
                )
            }
        };
        Ok(format!("{START}{body}{END}"))
    }
}

fn without_block(path: &Path, contents: &str) -> Result<Option<String>> {
    if let Some((before, block)) = contents.split_once(START) {
        if let Some((body, after)) = block.split_once(END) {
            if !before.contains(END)
                && !body.contains(START)
                && !after.contains(START)
                && !after.contains(END)
            {
                return Ok(Some(format!("{before}{after}")));
            }
        }
    } else if !contents.contains(END) {
        return Ok(None);
    }
    Err(Error::Host(msg!(
        "conda.path.invalid_block",
        path = path.display()
    )))
}

fn read_rc(path: &Path) -> Result<String> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => fs::read_to_string(path).map_err(|err| Error::io(path, err)),
        Ok(_) => Err(Error::Host(msg!(
            "conda.path.not_file",
            path = path.display()
        ))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(Error::io(path, err)),
    }
}

fn write_rc(path: &Path, contents: &str) -> io::Result<()> {
    fs::create_dir_all(path.parent().expect("shell config has a parent"))?;
    let mode = match fs::metadata(path) {
        Ok(meta) => meta.permissions().mode() & 0o777,
        Err(err) if err.kind() == io::ErrorKind::NotFound => 0o600,
        Err(err) => return Err(err),
    };
    let tmp = path.with_extension(format!("rospm-{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(contents.as_bytes())?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
