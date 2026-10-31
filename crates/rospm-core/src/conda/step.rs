//! Planning and executing Conda environment steps.

use super::conda_verify::{creation_command, replay_commands};
use super::*;

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

    pub(crate) fn perform_observed(
        &self,
        out: &mut dyn Write,
        execution: Option<&crate::execution::Execution<'_>>,
    ) -> Result<()> {
        match execution {
            Some(execution) => self.perform_with(out, &system::Observed(execution)),
            None => self.perform(out),
        }
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

    pub(crate) fn perform_with(&self, out: &mut dyn Write, system: &dyn System) -> Result<()> {
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
