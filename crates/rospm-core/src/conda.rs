//! Existing environments are checked only; managed environments live in the
//! project and are prepared by apply, which installs Conda itself when the
//! host has no compatible one. All subprocesses, downloads and writes happen
//! when the plan executes, never while constructing or rendering a preview.
//!
//! `lock` records a managed environment as an explicit lock per platform
//! (`conda list --explicit --sha256`); `apply --locked` replays it with
//! `conda create --file`, which installs exactly those packages without
//! solving. PyPI packages are outside explicit locks, so they must be exact
//! pins in the definition and are installed afterwards with `--no-deps`.

mod bootstrap;
mod definition;
mod explicit;
mod system;
#[cfg(test)]
mod tests;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::lock::{sha256_hex, Lock};
use crate::manifest::CondaBuild;
use crate::msg;
use crate::pixi::conda_platform;
use crate::plan::{shell_quote, Command};
use crate::platform::Platform;
use crate::probe::{CondaHost, EnvReport, Status};
use crate::provider::Shell;

use bootstrap::{compatible, miniforge, Bootstrap};
use definition::environment_yaml;
use explicit::replay;
use system::{Real, System};

pub use definition::definition_sha256;
pub use explicit::{lock_path, resolve_lock};

const MANAGED_PREFIX: &str = ".rospm/conda/env";
const DEFINITION: &str = ".rospm/conda/environment.yml";
const READY: &str = ".rospm/conda/ready.sha256";
const PENDING: &str = "pending\n";
/// What every build command runs inside the environment.
const BUILD_TOOLS: [&str; 1] = ["colcon"];
/// CMake cache entries naming the interpreter, for FindPython3, FindPython
/// and the older FindPythonInterp.
const CACHE_KEYS: [&str; 3] = [
    "Python3_EXECUTABLE",
    "Python_EXECUTABLE",
    "PYTHON_EXECUTABLE",
];
/// Imported with ROS sourced: build-time modules colcon / ament / rosidl need
/// in the environment, and rclpy's C extension, whose file name carries the
/// CPython version ROS was built for. Prints one JSON line.
const IMPORT_CHECK: &str = r#"import importlib, json, os, re, sys
failed = {}
for m in ("catkin_pkg", "em", "rclpy._rclpy_pybind11"):
    try:
        importlib.import_module(m)
    except Exception as e:
        failed[m] = f"{type(e).__name__}: {e}"
ros = sorted({v for p in os.environ.get("PYTHONPATH", "").split(os.pathsep) for v in re.findall(r"/python(3\.\d+)/", p)})
print(json.dumps({"python": "%d.%d" % sys.version_info[:2], "ros": ros, "failed": failed}))
"#;
/// Points Python shebangs of installed executables at this interpreter, so
/// `ros2 run` (which executes the file) uses the environment even when it is
/// not activated. Symlinks (`--symlink-install`) lead into sources: left alone.
/// Each argument lists one workspace's install base candidates, one per line;
/// the first existing is used, none existing is an error.
const PIN_SHEBANGS: &str = r##"import os, re, sys
py = sys.executable
if any(c.isspace() for c in py):
    print(f"shebangs not rewritten: {py} contains whitespace", file=sys.stderr)
    sys.exit()
shebang = re.compile(rb"#![ \t]*(?:/usr/bin/env[ \t]+)?((?:\S*/)?python[0-9.]*)([ \t][^\n]*)?\n?")
tops = []
for arg in sys.argv[1:]:
    found = [c for c in arg.split("\n") if os.path.isdir(c)]
    if not found:
        sys.exit("install base not found: " + ", ".join(arg.split("\n")))
    tops.append(found[0])
for top in tops:
    for d, _, files in os.walk(top):
        for f in files:
            p = os.path.join(d, f)
            if os.path.islink(p) or not os.access(p, os.X_OK):
                continue
            with open(p, "rb") as h:
                m = shebang.fullmatch(h.readline(512))
                if not m or os.path.realpath(m[1]) == os.path.realpath(py):
                    continue
                rest = h.read()
            with open(p, "wb") as h:
                h.write(b"#!" + os.fsencode(py) + (m[2] or b"") + b"\n" + rest)
            print(f"{p}: {os.fsdecode(m[1])} -> {py}")
"##;

/// PyPI distributions in the environment, i.e. those no Conda package owns,
/// as `conda list` reports them. `conda list --json` takes 30 s with 170 PyPI
/// packages (its pip scan); this takes about 1 s. Prints `[name, version]` pairs.
const PYPI_LIST: &str = r#"import glob, importlib.metadata as md, json, os, sys
owned = set()
for f in glob.glob(os.path.join(sys.prefix, "conda-meta", "*.json")):
    with open(f) as h:
        for p in json.load(h).get("files", []):
            owned.update((p, os.path.dirname(p)))
print(json.dumps(sorted({(d.metadata["Name"], d.version) for d in md.distributions()
    if os.path.relpath(str(d._path), sys.prefix) not in owned})))
"#;

/// What a managed environment holds. Its digests complete the completion
/// record, and every run that reuses the environment compares them again,
/// so hand edits (`pip install`, `conda install`) never go unnoticed.
pub(crate) struct Contents {
    /// `conda list --explicit --sha256` output.
    pub explicit: String,
    /// Normalized PyPI name and version, sorted.
    pub pypi: Vec<(String, String)>,
}

impl Contents {
    pub(crate) fn query(prefix: &Path, conda: &str, system: &dyn System) -> Result<Self> {
        let prefix_arg = prefix.to_string_lossy();
        let explicit = system.query(
            conda,
            &["list", "--explicit", "--sha256", "--prefix", &prefix_arg],
        )?;
        let cmd = Selection::Prefix(prefix.to_path_buf())
            .command(conda)
            .args(["python", "-I", "-c", PYPI_LIST]);
        let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
        let text = system.query(&cmd.program, &args)?;
        let listed: Vec<(String, String)> = serde_json::from_str(text.trim())
            .map_err(|e| Error::Host(msg!("conda.query_invalid", error = e)))?;
        let mut pypi: Vec<_> = listed
            .into_iter()
            .map(|(name, version)| (crate::probe::pip_requirement_name(&name), version))
            .collect();
        pypi.sort();
        Ok(Self { explicit, pypi })
    }

    /// The full completion record for `head`.
    pub(crate) fn record(&self, head: &str) -> String {
        let pypi: String = self
            .pypi
            .iter()
            .map(|(name, version)| format!("{name}=={version}\n"))
            .collect();
        format!(
            "{head}\n{}\n{}",
            packages_sha256(&self.explicit),
            sha256_hex(&pypi)
        )
    }
}

/// Digest of an explicit lock's package lines; comments such as
/// `# created-by: conda <version>` differ between Conda versions.
pub(crate) fn packages_sha256(explicit: &str) -> String {
    let lines: String = explicit
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    sha256_hex(&lines)
}

#[derive(Debug, Clone)]
pub enum Selection {
    Name(String),
    Prefix(PathBuf),
}

impl Selection {
    pub fn from_config(config: &CondaBuild, root: &Path) -> Result<Self> {
        config.validate()?;
        Ok(if let Some(name) = &config.name {
            Self::Name(name.clone())
        } else {
            Self::Prefix(
                root.join(
                    config
                        .prefix
                        .as_deref()
                        .unwrap_or(Path::new(MANAGED_PREFIX)),
                ),
            )
        })
    }

    /// `conda run` through `program`, which is `conda` on PATH or the
    /// bootstrapped executable.
    pub fn command(&self, program: &str) -> Command {
        let cmd = Command::new(program).args(["run", "--no-capture-output"]);
        match self {
            Self::Name(name) => cmd.args(["-n", name]),
            Self::Prefix(prefix) => cmd.arg("-p").arg(prefix.to_string_lossy()),
        }
    }

    fn target(&self) -> String {
        match self {
            Self::Name(name) => shell_quote(name),
            Self::Prefix(prefix) => shell_quote(&prefix.to_string_lossy()),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Name(name) => format!("-n {}", shell_quote(name)),
            Self::Prefix(prefix) => format!("-p {}", shell_quote(&prefix.to_string_lossy())),
        }
    }
}

#[derive(Debug, Clone)]
struct Preparation {
    root: PathBuf,
    prefix: PathBuf,
    definition: PathBuf,
    ready: PathBuf,
    contents: String,
    fingerprint: String,
    platform: Platform,
    source: Option<PathBuf>,
    variables: IndexMap<String, String>,
    /// `apply --locked`: install from the lock instead of solving.
    replay: Option<Replay>,
}

#[derive(Debug, Clone)]
struct Replay {
    lock: PathBuf,
    lock_sha256: String,
    /// [`packages_sha256`] of the lock: the replayed environment must match.
    packages_sha256: String,
    /// `name==version`, installed with `--no-deps` after the Conda packages.
    pip: Vec<String>,
}

impl Preparation {
    fn head(&self) -> String {
        stamp_head(&self.fingerprint, self.platform)
    }

    /// What the completion record claims, read without subprocesses: the
    /// definition digest, then the Conda and PyPI package digests. A
    /// replayed environment also satisfies its definition, so build and
    /// plain apply reuse it; a solved one satisfies a lock only when it
    /// holds exactly the lock's packages (as after `rospm lock`).
    fn is_ready(&self) -> bool {
        let Ok(ready) = fs::read_to_string(&self.ready) else {
            return false;
        };
        let lines: Vec<&str> = ready.lines().collect();
        self.prefix.join("conda-meta/history").is_file()
            && lines.len() == 3
            && lines[0] == self.head()
            && self
                .replay
                .as_ref()
                .is_none_or(|r| lines[1] == r.packages_sha256)
    }

    /// The live half of readiness: the packages are still what was recorded.
    fn unchanged(&self, contents: &Contents) -> bool {
        fs::read_to_string(&self.ready).is_ok_and(|r| r == contents.record(&self.head()))
    }
}

/// First line of the completion record: the definition digest and the Conda
/// subdir, so a prefix copied from another architecture is never reused.
fn stamp_head(fingerprint: &str, platform: Platform) -> String {
    format!("{fingerprint} {}", conda_platform(platform))
}

/// One plan step. `prepare` is false for build: it may only check readiness.
#[derive(Debug, Clone)]
pub struct CondaStep {
    selection: Selection,
    managed: Option<Preparation>,
    /// Managed environments only; `None` on platforms without an installer.
    bootstrap: Option<Bootstrap>,
    /// Chosen while planning from filesystem facts: a completed bootstrap
    /// wins, so apply and later builds keep using the Conda that created the
    /// environment; otherwise `conda` from PATH, checked when executed.
    program: String,
    prepare: bool,
    /// `--no-env-check` clears it: a prepared or existing environment is
    /// used without the live package and build checks.
    checks: bool,
    /// Build only: the line sourcing ROS before the import check.
    ros_setup: Option<String>,
    /// Build only: workspace build bases scanned for another interpreter's
    /// CMake caches.
    build_dirs: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Imports {
    python: String,
    ros: Vec<String>,
    failed: IndexMap<String, String>,
}

impl CondaStep {
    /// `lock` is the lock of `apply --locked`; a managed environment is then
    /// replayed from its record for `platform` or the plan fails.
    pub fn new(
        config: &CondaBuild,
        root: &Path,
        host: &CondaHost,
        platform: Platform,
        prepare: bool,
        lock: Option<&Lock>,
    ) -> Result<Self> {
        let selection = Selection::from_config(config, root)?;
        let managed = if let Some(managed) = &config.managed {
            let definition = environment_yaml(managed, root)?;
            let fingerprint = sha256_hex(&definition.contents);
            let replay = match lock.filter(|_| prepare) {
                Some(lock) => Some(replay(
                    managed,
                    &definition,
                    &fingerprint,
                    lock,
                    root,
                    platform,
                )?),
                None => None,
            };
            Some(Preparation {
                root: root.to_path_buf(),
                prefix: root.join(MANAGED_PREFIX),
                definition: root.join(DEFINITION),
                ready: root.join(READY),
                fingerprint,
                platform,
                contents: definition.contents,
                source: managed.environment_file.as_ref().map(|p| root.join(p)),
                variables: definition.variables,
                replay,
            })
        } else {
            None
        };
        let bootstrap = managed
            .as_ref()
            .and_then(|_| Bootstrap::new(host, platform));
        let program = match &bootstrap {
            Some(b) if b.is_ready() => b.conda().to_string_lossy().into_owned(),
            _ => "conda".to_string(),
        };
        Ok(Self {
            selection,
            managed,
            bootstrap,
            program,
            prepare,
            checks: true,
            ros_setup: None,
            build_dirs: Vec::new(),
        })
    }

    /// Adds the build checks that need ROS and the workspaces.
    pub fn for_build(mut self, ros_setup: String, build_dirs: Vec<PathBuf>) -> Self {
        self.ros_setup = Some(ros_setup);
        self.build_dirs = build_dirs;
        self
    }

    /// The user confirmed the environment; skip the checks that cost subprocesses.
    pub fn skip_checks(&mut self) {
        self.checks = false;
    }

    /// Unchecked use needs no preparation: an existing environment, or a
    /// managed one whose completion record matches.
    fn unchecked(&self) -> bool {
        !self.checks && self.managed.as_ref().is_none_or(Preparation::is_ready)
    }

    /// The selected environment as `conda run` names it (`-n …` / `-p …`).
    pub fn label(&self) -> String {
        self.selection.label()
    }

    /// The `conda run` prefix every build command goes through.
    pub fn command(&self) -> Command {
        self.selection.command(&self.program)
    }

    /// Lines activating the environment in `rospm env` / `shell`, with
    /// Conda's own shell hook: no `conda init` or startup file needed.
    pub fn activation(&self, shell: Shell) -> String {
        format!(
            "eval \"$({} shell.{shell} hook)\"\nconda activate {}\n",
            shell_quote(&self.program),
            self.selection.target()
        )
    }

    /// Run after building, inside the environment.
    pub fn pin_shebangs(&self, install_bases: impl IntoIterator<Item = Vec<PathBuf>>) -> Command {
        self.command()
            .args(["python", "-I", "-c", PIN_SHEBANGS])
            .args(install_bases.into_iter().map(|bases| {
                bases
                    .iter()
                    .map(|d| d.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n")
            }))
    }

    /// Preview includes the normalized definition and exact creation command.
    pub fn render(&self) -> String {
        if self.unchecked() {
            return msg!("conda.checks_skipped", selection = self.selection.label()).to_string();
        }
        let check = msg!("conda.check", selection = self.selection.label());
        if !self.prepare {
            let python = match &self.selection {
                Selection::Prefix(prefix) => prefix.join("bin/python").display().to_string(),
                Selection::Name(_) => format!("python ({})", self.selection.label()),
            };
            let checks = msg!(
                "conda.build_checks",
                python = python,
                tools = BUILD_TOOLS.join(", ")
            );
            return match &self.managed {
                Some(managed) => format!(
                    "{check}; {}; {checks}",
                    msg!("conda.require_ready", path = managed.ready.display())
                ),
                None => format!("{check}; {checks}"),
            };
        }
        let Some(managed) = &self.managed else {
            return check.to_string();
        };
        let source = managed
            .source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "build.conda.managed".to_string());
        let tool = match &self.bootstrap {
            _ if self.program != "conda" => msg!("conda.tool_ready", conda = self.program),
            Some(bootstrap) => crate::Message::text(bootstrap.render()),
            None => msg!("conda.tool_path_only", minimum = miniforge().minimum_conda),
        };
        let prefix = managed.prefix.display();
        if managed.is_ready() {
            return format!("{tool}\n{}", msg!("conda.reuse_checked", prefix = prefix));
        }
        // What perform will do with a prefix that is already there.
        let tool = match (
            fs::symlink_metadata(&managed.prefix).is_ok(),
            managed.ready.exists(),
        ) {
            (false, _) => tool.to_string(),
            (true, true) => format!("{tool}\n{}", msg!("conda.recreate", prefix = prefix)),
            (true, false) => format!(
                "{tool}\n{}",
                msg!("conda.managed_conflict", prefix = prefix)
            ),
        };
        if let Some(replay) = &managed.replay {
            let mut lines = vec![
                tool.to_string(),
                msg!(
                    "conda.replay",
                    prefix = managed.prefix.display(),
                    lock = replay.lock.display(),
                    sha256 = replay.lock_sha256
                )
                .to_string(),
            ];
            lines.extend(
                replay_commands(managed, replay, &self.selection, &self.program)
                    .iter()
                    .map(Command::render),
            );
            return lines.join("\n");
        }
        format!(
            "{tool}\n{}\n{}\n{}\n{}",
            msg!(
                "conda.prepare",
                prefix = managed.prefix.display(),
                source = source
            ),
            msg!("conda.definition", path = managed.definition.display()),
            managed.contents.trim_end(),
            creation_command(managed, &self.program).render()
        )
    }

    pub fn perform(&self, out: &mut dyn Write) -> Result<()> {
        self.perform_with(out, &Real)
    }

    /// The Conda that prepares or runs the managed environment. Only apply
    /// may install one; build reports what is missing instead.
    fn managed_conda(&self, out: &mut dyn Write, system: &dyn System) -> Result<String> {
        if self.program != "conda" {
            return Ok(self.program.clone());
        }
        let problem = match system.query("conda", &["--version"]) {
            Ok(found) if compatible(&found) => return Ok(self.program.clone()),
            Ok(found) => msg!(
                "conda.tool_incompatible",
                found = found.trim(),
                minimum = miniforge().minimum_conda
            ),
            Err(error) => error.message(),
        };
        let Some(bootstrap) = self.bootstrap.as_ref().filter(|_| self.prepare) else {
            return Err(Error::Host(msg!(
                "conda.tool_unavailable",
                problem = problem,
                hint = if self.prepare {
                    msg!("conda.no_installer", minimum = miniforge().minimum_conda)
                } else {
                    msg!("conda.run_apply")
                }
            )));
        };
        writeln!(out, "{problem}").map_err(|e| Error::io("<stdout>", e))?;
        bootstrap.install(out, system)?;
        Ok(bootstrap.conda().to_string_lossy().into_owned())
    }

    fn perform_with(&self, out: &mut dyn Write, system: &dyn System) -> Result<()> {
        if self.unchecked() {
            writeln!(out, "{}", self.render()).map_err(|e| Error::io("<stdout>", e))?;
            return Ok(());
        }
        let prefix = self.ensure(out, system)?;
        if !self.prepare {
            self.check_build_tools(&prefix, out, system)?;
        }
        Ok(())
    }

    /// Before building, prove that `python` and the build tools resolve
    /// inside the environment, never silently from the system.
    fn check_build_tools(
        &self,
        prefix: &Path,
        out: &mut dyn Write,
        system: &dyn System,
    ) -> Result<()> {
        let query = |cmd: Command| {
            let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
            system.query(&cmd.program, &args)
        };
        let selection = self.selection.label();
        let found =
            query(
                self.command()
                    .args(["python", "-c", "import sys; print(sys.executable)"]),
            )
            .map_err(|e| {
                Error::Host(msg!(
                    "conda.interpreter_missing",
                    selection = selection,
                    error = e.message()
                ))
            })?;
        let found = PathBuf::from(found.trim());
        let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        if !real(&found).starts_with(real(prefix)) {
            return Err(Error::Host(msg!(
                "conda.interpreter_outside",
                selection = selection,
                found = found.display(),
                prefix = prefix.display()
            )));
        }
        writeln!(
            out,
            "{}",
            msg!("conda.interpreter", python = found.display())
        )
        .map_err(|e| Error::io("<stdout>", e))?;
        let bash = |script: &str| {
            query(
                self.command()
                    .args(["bash", "--noprofile", "--norc", "-c", script]),
            )
        };
        for tool in BUILD_TOOLS {
            let path = bash(&format!("command -v {tool}")).map_err(|_| {
                Error::Host(msg!(
                    "conda.build_tool_missing",
                    tool = tool,
                    selection = selection
                ))
            })?;
            // A tool from the system runs, and builds, with the system Python.
            let path = PathBuf::from(path.trim());
            if !real(&path).starts_with(real(prefix)) {
                return Err(Error::Host(msg!(
                    "conda.build_tool_outside",
                    tool = tool,
                    selection = selection,
                    found = path.display(),
                    prefix = prefix.display()
                )));
            }
        }
        if let Some(setup) = &self.ros_setup {
            let output = bash(&format!(
                "set -e\n{setup}\npython -c {}",
                shell_quote(IMPORT_CHECK)
            ))?;
            let imports: Imports = output
                .lines()
                .last()
                .and_then(|line| serde_json::from_str(line).ok())
                .ok_or_else(|| Error::Host(msg!("conda.query_invalid", error = output.trim())))?;
            if !imports.failed.is_empty() {
                let failed = imports
                    .failed
                    .iter()
                    .map(|(m, e)| format!("{m} ({e})"))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(Error::Host(
                    if !imports.ros.is_empty() && !imports.ros.contains(&imports.python) {
                        msg!(
                            "conda.python_abi",
                            selection = selection,
                            python = imports.python,
                            ros = imports.ros.join(", "),
                            failed = failed
                        )
                    } else {
                        msg!(
                            "conda.python_modules",
                            selection = selection,
                            failed = failed
                        )
                    },
                ));
            }
        }
        let python = real(&found);
        let mut stale = Vec::new();
        for entry in self
            .build_dirs
            .iter()
            .filter_map(|dir| fs::read_dir(dir).ok())
            .flatten()
            .flatten()
        {
            let Ok(cache) = fs::read_to_string(entry.path().join("CMakeCache.txt")) else {
                continue;
            };
            // `KEY:TYPE=VALUE`; NOTFOUND and other non-paths are skipped.
            let old = cache.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                let key = key.split(':').next()?;
                (CACHE_KEYS.contains(&key)
                    && value.starts_with('/')
                    && real(Path::new(value)) != python)
                    .then(|| value.to_string())
            });
            if let Some(old) = old {
                stale.push(format!("{} ({old})", entry.path().display()));
            }
        }
        if !stale.is_empty() {
            stale.sort();
            return Err(Error::Host(msg!(
                "conda.stale_cache",
                python = found.display(),
                caches = stale.join(", ")
            )));
        }
        Ok(())
    }

    /// Prepares or checks the environment and returns its prefix.
    fn ensure(&self, out: &mut dyn Write, system: &dyn System) -> Result<PathBuf> {
        if let Some(managed) = &self.managed {
            let conda = self.managed_conda(out, system)?;
            if managed.is_ready() {
                let contents = Contents::query(&managed.prefix, &conda, system)?;
                if managed.unchanged(&contents) {
                    writeln!(
                        out,
                        "{}",
                        msg!("conda.reused", prefix = managed.prefix.display())
                    )
                    .map_err(|e| Error::io("<stdout>", e))?;
                    return Ok(managed.prefix.clone());
                }
                if !self.prepare {
                    return Err(Error::Host(msg!(
                        "conda.managed_changed",
                        prefix = managed.prefix.display()
                    )));
                }
                writeln!(
                    out,
                    "{}",
                    msg!("conda.changed", prefix = managed.prefix.display())
                )
                .map_err(|e| Error::io("<stdout>", e))?;
            }
            if !self.prepare {
                return Err(Error::Host(msg!(
                    "conda.managed_not_ready",
                    prefix = managed.prefix.display()
                )));
            }
            // A completion record, even a pending or outdated one, means rospm
            // created the prefix: an interrupted or outdated environment is
            // recreated. Never adopt or overwrite a directory without one.
            if fs::symlink_metadata(&managed.prefix).is_ok() {
                if !managed.ready.exists() {
                    return Err(Error::Host(msg!(
                        "conda.managed_conflict",
                        prefix = managed.prefix.display()
                    )));
                }
                writeln!(
                    out,
                    "{}",
                    msg!("conda.recreate", prefix = managed.prefix.display())
                )
                .map_err(|e| Error::io("<stdout>", e))?;
                // A half-removed prefix must never match its old record.
                fs::write(&managed.ready, PENDING).map_err(|e| Error::io(&managed.ready, e))?;
                fs::remove_dir_all(&managed.prefix).map_err(|e| Error::io(&managed.prefix, e))?;
            }
            let parent = managed
                .definition
                .parent()
                .expect("project definition has a parent");
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            // Claims the prefix before creation; never a valid stamp.
            fs::write(&managed.ready, PENDING).map_err(|e| Error::io(&managed.ready, e))?;
            fs::write(&managed.definition, &managed.contents)
                .map_err(|e| Error::io(&managed.definition, e))?;
            match &managed.replay {
                Some(replay) => {
                    for command in replay_commands(managed, replay, &self.selection, &conda) {
                        system.run(&command)?;
                    }
                }
                None => system.run(&creation_command(managed, &conda))?,
            }
            if !managed.prefix.join("conda-meta/history").is_file() {
                return Err(Error::Host(msg!(
                    "conda.managed_not_ready",
                    prefix = managed.prefix.display()
                )));
            }
            let contents = Contents::query(&managed.prefix, &conda, system)?;
            if let Some(replay) = &managed.replay {
                if packages_sha256(&contents.explicit) != replay.packages_sha256 {
                    return Err(Error::Host(msg!(
                        "conda.replay_differs",
                        prefix = managed.prefix.display(),
                        lock = replay.lock.display()
                    )));
                }
            }
            fs::write(&managed.ready, contents.record(&managed.head()))
                .map_err(|e| Error::io(&managed.ready, e))?;
            return Ok(managed.prefix.clone());
        }

        let text = system.query("conda", &["env", "list", "--json"])?;
        let envs: EnvironmentList = serde_json::from_str(&text)
            .map_err(|e| Error::Host(msg!("conda.query_invalid", error = e)))?;
        let found = match &self.selection {
            Selection::Prefix(prefix) => Some(prefix.clone()),
            Selection::Name(name) if name == "base" => {
                let output = system.query("conda", &["info", "--base"])?;
                Some(PathBuf::from(output.trim()))
            }
            Selection::Name(name) => envs
                .envs
                .into_iter()
                .find(|p| p.file_name().is_some_and(|n| n == name.as_str())),
        };
        match found {
            Some(prefix) if prefix.join("conda-meta/history").is_file() => Ok(prefix),
            _ => Err(Error::Host(msg!(
                "conda.existing_missing",
                selection = self.selection.label()
            ))),
        }
    }
}

/// `verify` rows for Conda, kept apart from apt. Only a managed environment
/// has a lock; an existing environment's name or path pins no versions.
pub fn verify(
    config: Option<&CondaBuild>,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    lock: &Lock,
    r: &mut EnvReport,
) -> Result<()> {
    verify_with(config, root, host, platform, lock, r, &Real)
}

fn verify_with(
    config: Option<&CondaBuild>,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    lock: &Lock,
    r: &mut EnvReport,
    system: &dyn System,
) -> Result<()> {
    for record in lock.conda.iter().filter(|c| c.platform != platform) {
        r.push(
            "conda",
            record.platform.to_string(),
            crate::Message::text("-"),
            msg!("verify.conda.other_platform", lock = record.lock),
            Status::Info,
        );
    }
    let Some(config) = config else {
        for record in lock.conda.iter().filter(|c| c.platform == platform) {
            r.push(
                "conda",
                record.lock.clone(),
                crate::Message::text("-"),
                msg!("verify.conda.not_declared"),
                Status::Mismatch,
            );
        }
        return Ok(());
    };
    let selection = Selection::from_config(config, root)?;
    let Some(managed) = &config.managed else {
        r.push(
            "conda",
            selection.label(),
            crate::Message::text("-"),
            msg!("verify.conda.external"),
            Status::Info,
        );
        return Ok(());
    };
    let expected = match lock.conda_for(platform) {
        Some(record) => format!(
            "{} ({})",
            record.lock,
            record.lock_sha256.get(..12).unwrap_or(&record.lock_sha256)
        ),
        None => lock_path(managed, platform),
    };
    let step = match CondaStep::new(config, root, host, platform, true, Some(lock)) {
        Ok(step) => step,
        Err(e) => {
            let status = match lock.conda_for(platform) {
                Some(_) => Status::Mismatch,
                None => Status::Missing,
            };
            r.push("conda", selection.label(), expected, e.message(), status);
            return Ok(());
        }
    };
    let managed = step.managed.as_ref().expect("managed configuration");
    let (actual, status) = if managed.is_ready() {
        match Contents::query(&managed.prefix, &step.program, system) {
            Ok(contents) if managed.unchanged(&contents) => {
                (msg!("verify.conda.replayed"), Status::Ok)
            }
            Ok(_) => (msg!("verify.conda.changed"), Status::Mismatch),
            Err(e) => (e.message(), Status::Unknown),
        }
    } else if managed.prefix.join("conda-meta/history").is_file() {
        (msg!("verify.conda.differs"), Status::Mismatch)
    } else {
        (msg!("verify.conda.not_prepared"), Status::Missing)
    };
    r.push("conda", selection.label(), expected, actual, status);
    Ok(())
}

fn creation_command(managed: &Preparation, conda: &str) -> Command {
    Command::new(conda)
        .args(["env", "create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(managed.definition.to_string_lossy())
        .cwd(&managed.root)
}

/// No solving: `conda create --file` installs exactly the explicit lock and
/// verifies each package digest. Definition variables and PyPI pins, which an
/// explicit lock cannot hold, follow.
fn replay_commands(
    managed: &Preparation,
    replay: &Replay,
    selection: &Selection,
    conda: &str,
) -> Vec<Command> {
    let mut commands = vec![Command::new(conda)
        .args(["create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(replay.lock.to_string_lossy())
        .cwd(&managed.root)];
    if !managed.variables.is_empty() {
        commands.push(
            Command::new(conda)
                .args(["env", "config", "vars", "set", "--prefix"])
                .arg(managed.prefix.to_string_lossy())
                .args(managed.variables.iter().map(|(k, v)| format!("{k}={v}")))
                .cwd(&managed.root),
        );
    }
    if !replay.pip.is_empty() {
        commands.push(
            selection
                .command(conda)
                .args(["python", "-m", "pip", "install", "--no-deps", "--no-input"])
                .arg("--disable-pip-version-check")
                .args(replay.pip.iter().cloned())
                .cwd(&managed.root),
        );
    }
    commands
}

#[derive(Deserialize)]
struct EnvironmentList {
    envs: Vec<PathBuf>,
}
