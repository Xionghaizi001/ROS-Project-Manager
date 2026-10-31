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
mod conda_verify;
mod definition;
mod explicit;
mod step;
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

pub use conda_verify::verify;
#[cfg(test)]
pub(crate) use conda_verify::verify_with;
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

#[derive(Deserialize)]
struct EnvironmentList {
    envs: Vec<PathBuf>,
}
