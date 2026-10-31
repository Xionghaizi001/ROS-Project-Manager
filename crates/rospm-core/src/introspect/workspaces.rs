//! Workspace and repository discovery for environment introspection.

use super::*;

// ---------------------------------------------------------------- workspaces

struct WsCandidate {
    dir: PathBuf,
    rel: PathBuf,
    packages: WorkspacePackages,
    built: bool,
    sources: Option<PathBuf>,
}

fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .filter(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `root` itself and its subdirectories (two levels) that hold a `src/`.
fn find_workspace_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.join("src").is_dir() {
        out.push(root.to_path_buf());
    }
    for level1 in list_dirs(root) {
        if level1.join("src").is_dir() {
            out.push(level1);
            continue;
        }
        for level2 in list_dirs(&level1) {
            if level2.join("src").is_dir() {
                out.push(level2);
            }
        }
    }
    out
}

/// `*.repos` files in `root` and two levels below (never inside `src/`).
fn find_repos_files(root: &Path) -> Vec<PathBuf> {
    fn files_in(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().map(|e| e == "repos").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
    let mut out = files_in(root);
    for level1 in list_dirs(root) {
        out.extend(files_in(&level1));
        for level2 in list_dirs(&level1) {
            out.extend(files_in(&level2));
        }
    }
    out
}

pub(super) fn workspace_name(rel: &Path, sources: Option<&Path>) -> String {
    let dir_name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
    let mut name = if dir_name.is_empty()
        || rel == Path::new(".")
        || GENERIC_WS_NAMES.contains(&dir_name.as_str())
    {
        sources.and_then(stem).unwrap_or_else(|| "main".into())
    } else {
        dir_name
            .strip_suffix("_ws")
            .or_else(|| dir_name.strip_suffix("-ws"))
            .unwrap_or(&dir_name)
            .to_string()
    };
    name = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name = "main".into();
    }
    name
}

pub(super) fn detect_workspaces(d: &mut Draft, root: &Path, facts: &HostFacts) {
    let mut candidates: Vec<WsCandidate> = find_workspace_dirs(root)
        .into_iter()
        .map(|dir| {
            let rel = dir
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let rel = if rel.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                rel
            };
            WsCandidate {
                packages: WorkspacePackages::scan(&dir.join("src")),
                built: dir.join("install").join("setup.bash").is_file(),
                dir,
                rel,
                sources: None,
            }
        })
        .collect();

    // Match every .repos file to the workspace whose src/ holds its checkouts.
    let mut unmatched_repos: Vec<(PathBuf, ReposFile)> = Vec::new();
    for file in find_repos_files(root) {
        let rel = file
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| file.clone());
        let repos = match ReposFile::load(&file) {
            Ok(r) => r,
            Err(e) => {
                d.todos.push(msg!(
                    "init.workspaces.repos_unparseable",
                    file = rel.display(),
                    error = e
                ));
                continue;
            }
        };
        if repos.repositories.is_empty() {
            d.notes
                .push(msg!("init.workspaces.repos_empty", file = rel.display()));
            continue;
        }
        let best = candidates
            .iter_mut()
            .map(|c| {
                let hits = repos
                    .repositories
                    .keys()
                    .filter(|r| c.dir.join("src").join(r).is_dir())
                    .count();
                (hits, c)
            })
            .filter(|(hits, _)| *hits > 0)
            .max_by_key(|(hits, _)| *hits);
        match best {
            Some((hits, c)) if c.sources.is_none() => {
                d.notes.push(msg!(
                    "init.workspaces.repos_matched",
                    file = rel.display(),
                    workspace = c.rel.display(),
                    hits = hits,
                    total = repos.repositories.len()
                ));
                c.sources = Some(rel);
            }
            Some((_, c)) => d.todos.push(msg!(
                "init.workspaces.repos_conflict",
                file = rel.display(),
                workspace = c.rel.display(),
                existing = c
                    .sources
                    .as_ref()
                    .map(|s| s.display().to_string())
                    .unwrap_or_default()
            )),
            None => unmatched_repos.push((rel, repos)),
        }
    }

    // One leftover .repos and one workspace without a sources file: almost
    // certainly a checkout before `vcs import` (the workspace may already
    // hold local packages next to where the imports will land).
    if unmatched_repos.len() == 1 {
        let without_sources: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.sources.is_none())
            .map(|(i, _)| i)
            .collect();
        if let [i] = without_sources.as_slice() {
            let (rel, _) = unmatched_repos.remove(0);
            d.notes.push(msg!(
                "init.workspaces.repos_assigned",
                file = rel.display(),
                workspace = candidates[*i].rel.display()
            ));
            candidates[*i].sources = Some(rel);
        }
    }

    // Overlay order from AMENT_PREFIX_PATH (topmost first). colcon's default
    // isolated install yields `<ws>/install/<pkg>`; merged installs `<ws>/install`.
    let mut sourced: Vec<usize> = Vec::new();
    for entry in &facts.ament_prefix_path {
        let entry = entry.canonicalize().unwrap_or_else(|_| entry.clone());
        for (i, c) in candidates.iter().enumerate() {
            if entry.starts_with(c.dir.join("install")) && !sourced.contains(&i) {
                sourced.push(i);
            }
        }
    }

    let mut names: Vec<String> = Vec::new();
    for c in &candidates {
        let mut name = workspace_name(&c.rel, c.sources.as_deref());
        let base = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        names.push(name);
    }

    let mut underlays: Vec<Vec<String>> = vec![Vec::new(); candidates.len()];
    if sourced.len() >= 2 {
        // bottom-up: the last entry in AMENT_PREFIX_PATH is the lowest overlay.
        for pair in sourced.iter().rev().collect::<Vec<_>>().windows(2) {
            underlays[*pair[1]].push(names[*pair[0]].clone());
        }
        d.notes.push(msg!(
            "init.underlays.from_ament",
            order = sourced
                .iter()
                .rev()
                .map(|i| names[*i].as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    } else if candidates.len() >= 2 {
        d.todos.push(msg!("init.underlays.unknown_order"));
    }

    let mut colcon_defaults: Option<PathBuf> = None;
    if let Some(f) = &facts.colcon_defaults_file {
        match f.canonicalize() {
            Err(_) => d
                .todos
                .push(msg!("init.colcon_defaults.missing", file = f.display())),
            Ok(abs) => match abs.strip_prefix(root) {
                Ok(rel) => {
                    d.notes
                        .push(msg!("init.colcon_defaults.from_env", file = rel.display()));
                    colcon_defaults = Some(rel.to_path_buf());
                }
                Err(_) => d.todos.push(msg!(
                    "init.colcon_defaults.outside_project",
                    file = f.display()
                )),
            },
        }
    }

    let mut all_keys: BTreeSet<String> = BTreeSet::new();
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    for c in &candidates {
        all_names.extend(c.packages.names().iter().map(|s| s.to_string()));
    }
    for (i, c) in candidates.iter().enumerate() {
        let n = c.packages.packages.len();
        let origin = msg!(
            "init.origin.found",
            dir = c.rel.display(),
            packages = if n == 1 {
                msg!("init.origin.one_package").to_string()
            } else {
                msg!("init.origin.n_packages", count = n).to_string()
            },
            built = if c.built {
                msg!("init.origin.built").to_string()
            } else {
                String::new()
            },
            repos = if c.sources.is_some() {
                String::new()
            } else {
                msg!("init.origin.no_repos").to_string()
            }
        );
        for (file, err) in &c.packages.errors {
            d.todos.push(msg!(
                "init.workspaces.invalid_package_xml",
                workspace = names[i],
                file = file.display(),
                error = err
            ));
        }
        for key in c.packages.external_keys().keys() {
            if !all_names.contains(key) {
                all_keys.insert(key.clone());
            }
        }
        d.workspaces.push(DraftWorkspace {
            name: names[i].clone(),
            path: c.rel.clone(),
            sources: c.sources.clone(),
            underlays: underlays[i].clone(),
            colcon_defaults: colcon_defaults.clone(),
            packages: n,
            origin,
        });
    }

    // .repos files nobody imported yet become workspaces of their own.
    for (rel, repos) in unmatched_repos {
        let stem = rel
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "main".into());
        let mut name = workspace_name(Path::new(&stem), None);
        let base = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        names.push(name.clone());
        let path = if name == "main" {
            PathBuf::from("ws")
        } else {
            PathBuf::from(format!("{name}_ws"))
        };
        d.todos.push(msg!(
            "init.workspaces.path_guessed",
            workspace = name,
            file = rel.display(),
            path = path.display()
        ));
        d.workspaces.push(DraftWorkspace {
            name,
            path,
            sources: Some(rel.clone()),
            underlays: Vec::new(),
            colcon_defaults: None,
            packages: 0,
            origin: msg!(
                "init.origin.repos_not_imported",
                file = rel.display(),
                count = repos.repositories.len()
            ),
        });
    }

    if d.workspaces.is_empty() {
        d.todos.push(msg!("init.workspaces.placeholder"));
        d.workspaces.push(DraftWorkspace {
            name: "main".into(),
            path: PathBuf::from("ws"),
            sources: Some(PathBuf::from("main.repos")),
            underlays: Vec::new(),
            colcon_defaults: None,
            packages: 0,
            origin: msg!("init.origin.placeholder"),
        });
    }

    d.external_keys = all_keys.into_iter().collect();
    if !d.external_keys.is_empty() {
        d.notes.push(msg!(
            "init.dependencies.rosdep_keys",
            count = d.external_keys.len()
        ));
    }
    d.todos.push(msg!("init.dependencies.apt_undetectable"));
}
