//! Apply command implementation.

use std::io::Write;

use anyhow::{anyhow, bail, Context as _, Result};

use rospm_core::lock::{Lock, LOCK_FILE};
use rospm_core::manifest::Manifest;
use rospm_core::mirror::{self, Mirror};
use rospm_core::net::{self, Role, Verdict};
use rospm_core::plan::Step;
use rospm_core::platform::{ubuntu_codename, RosDistro};
use rospm_core::probe::Host;
use rospm_core::provider;
use rospm_core::repos::ReposFile;
use rospm_core::{msg, Error};

use crate::i18n::tr;

use super::build::{run_build, skip_env_checks};
use super::report::print_probe_table;
use super::{context, load, Global, Loaded};

/// `--mirror` beats `ROSPM_MIRROR`; neither means "probe and decide".
pub(super) fn requested_mirror(flag: Option<Mirror>) -> Result<Option<Mirror>> {
    if flag.is_some() {
        return Ok(flag);
    }
    match std::env::var("ROSPM_MIRROR") {
        Ok(v) if !v.trim().is_empty() => {
            let m = v.trim().parse::<Mirror>().map_err(|_| {
                anyhow!(
                    "{}",
                    msg!(
                        "cli.apply.bad_mirror_env",
                        value = v.trim(),
                        names = mirror::NAMES.join(" | ")
                    )
                )
            })?;
            Ok(Some(m))
        }
        _ => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)] // one per CLI flag, called once from main
pub fn apply(
    global: &Global,
    locked: bool,
    build: bool,
    no_env_check: bool,
    offline: bool,
    dry_run: bool,
    mirror: Option<Mirror>,
    distro: Option<RosDistro>,
) -> Result<()> {
    let mut loaded = load(global)?;
    if locked && distro.is_some_and(|d| d != loaded.manifest.project.ros_distro) {
        bail!("{}", tr("cli.apply.locked_distro_conflict"));
    }
    // `--locked` requires a lock before anything else runs: missing or out
    // of date with rospm.toml is a user error, corrected by re-running
    // `rospm lock`, never a silent degradation (spec §3.2).
    let lock = if locked {
        Some(load_locked_lock(&loaded)?)
    } else {
        None
    };
    let host = Host::probe();
    let manifest_update = prepare_distro(&mut loaded, &host, distro)?;
    let requested = requested_mirror(mirror)?;
    // Snapshot dates roll off the snapshot services over time (P0 finding):
    // a real run checks the recorded snapshot is still online and, if not,
    // degrades to the live repositories (announced, never silent) instead of
    // failing outright. `--dry-run` never touches the network here — the
    // printed plan already shows exactly what the lock says.
    let lock = match lock {
        Some(l) if !dry_run => Some(degrade_unreachable_snapshots(&loaded, l)),
        other => other,
    };
    let sources = if dry_run || offline {
        let m = requested.unwrap_or(mirror::OFFICIAL);
        println!(
            "# {}",
            msg!(
                "cli.apply.not_probed",
                reason = if dry_run { "--dry-run" } else { "--offline" },
                name = m.name
            )
        );
        m
    } else {
        // With a reachable ROS snapshot, apt never downloads from the ROS
        // repository, so its reachability must not decide anything.
        let ros_snapshot = lock
            .as_ref()
            .and_then(|l| l.provider.native.as_ref())
            .is_some_and(|n| n.ros_snapshot.is_some());
        select_sources(&loaded, requested, ros_snapshot)?
    };
    let ctx = context(&loaded, &host, locked, lock.as_ref(), offline, sources)?;
    let mut plan = provider::for_manifest(&loaded.manifest).plan_apply(&ctx)?;
    if no_env_check {
        skip_env_checks(&mut plan);
    }
    if let Some(contents) = manifest_update {
        plan.steps.insert(
            0,
            Step::WriteFile {
                path: loaded.path.clone(),
                contents,
                sudo: false,
            },
        );
    }
    let mut out = std::io::stdout().lock();
    let result = plan.execute(dry_run, &mut out);
    out.flush()?;
    match result {
        Err(Error::CommandFailed { command, status }) if !dry_run && sources.is_official() => {
            // Networks that pass a short probe can still stall on a longer
            // transfer; point at the switch instead of leaving the user to guess.
            Err(anyhow!(Error::CommandFailed { command, status })
                .context(tr("cli.apply.official_failed_hint")))
        }
        Err(e) => Err(e.into()),
        Ok(()) if build => {
            // Re-probe: apply may just have installed ROS or Miniforge.
            let host = if dry_run { host } else { Host::probe() };
            run_build(
                &loaded,
                &host,
                None,
                true,
                no_env_check,
                dry_run,
                global.json,
            )
        }
        Ok(()) => Ok(()),
    }
}

/// Validate a distro change before any writes or network probes. The returned
/// text is written as the first plan step, so dry runs leave the project intact.
fn prepare_distro(
    loaded: &mut Loaded,
    host: &Host,
    distro: Option<RosDistro>,
) -> Result<Option<String>> {
    let Some(distro) = distro.filter(|d| *d != loaded.manifest.project.ros_distro) else {
        return Ok(None);
    };
    let ubuntu = distro.default_ubuntu();
    if host.ubuntu_matches(ubuntu) != Some(true) {
        let have = host
            .os
            .as_ref()
            .and_then(|os| os.pretty_name.as_deref())
            .map(str::to_string)
            .unwrap_or_else(|| tr("native.unknown_os"));
        bail!(
            "{}",
            msg!(
                "native.apply.wrong_ubuntu",
                ubuntu = ubuntu,
                distro = distro,
                have = have
            )
        );
    }
    let text = std::fs::read_to_string(&loaded.path)?;
    let mut doc = text.parse::<toml_edit::DocumentMut>()?;
    set_toml_string(&mut doc["project"]["ros_distro"], distro.as_str());
    if let Some(native) = doc
        .get_mut("environment")
        .and_then(toml_edit::Item::as_table_like_mut)
        .and_then(|env| env.get_mut("native"))
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        if let Some(base) = native.get_mut("ros_base") {
            let old_prefix = format!("ros-{}-", loaded.manifest.project.ros_distro);
            if let Some(suffix) = base.as_str().and_then(|s| s.strip_prefix(&old_prefix)) {
                let package = format!("ros-{distro}-{suffix}");
                set_toml_string(base, &package);
            }
        }
        if let Some(version) = native.get_mut("ubuntu") {
            set_toml_string(version, ubuntu);
        }
    }
    let text = doc.to_string();
    loaded.manifest = Manifest::parse(&text, &loaded.path)?;
    Ok(Some(text))
}

/// Keep comments and surrounding whitespace when replacing a TOML value.
fn set_toml_string(item: &mut toml_edit::Item, text: &str) {
    let decor = item
        .as_value()
        .expect("validated string field")
        .decor()
        .clone();
    let mut value = toml_edit::Value::from(text);
    *value.decor_mut() = decor;
    *item = toml_edit::Item::Value(value);
}

/// Load and validate `rospm.lock` for `apply --locked` (spec §3.2): absent or
/// stale is corrected by re-running `rospm lock`, never guessed at.
fn load_locked_lock(loaded: &Loaded) -> Result<Lock> {
    let lock = loaded.load_lock()?.ok_or_else(|| {
        anyhow!(
            "{}",
            msg!("cli.apply.locked_lock_missing", file = LOCK_FILE)
        )
    })?;
    let manifest_text =
        std::fs::read_to_string(&loaded.path).with_context(|| loaded.path.display().to_string())?;
    if !lock.matches_manifest(&manifest_text) {
        bail!("{}", msg!("cli.apply.locked_lock_stale", file = LOCK_FILE));
    }
    Ok(lock)
}

/// Timeout for the one-shot reachability check below: short enough that a
/// dead snapshot does not stall `apply` for long, generous enough for a slow
/// but working connection.
const SNAPSHOT_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Drop a locked snapshot that no longer answers, announcing it first (never
/// silently, derived principle 4): `plan_apply`'s existing degrade-to-live
/// path (spec §3.2) then takes over exactly as it does for a lock recorded
/// without a snapshot at all. Real network I/O — exercised only by an actual
/// non-dry-run `apply --locked`, which this repository's tests never run
/// (safety constraint); covered here only up to the point tests can reach.
fn degrade_unreachable_snapshots(loaded: &Loaded, mut lock: Lock) -> Lock {
    let Some(codename) = ubuntu_codename(&loaded.manifest.ubuntu()) else {
        return lock;
    };
    let Some(native) = lock.provider.native.as_mut() else {
        return lock;
    };
    if native.ros_snapshot.is_none() && native.ubuntu_snapshot.is_none() {
        return lock;
    }
    println!("{}", tr("cli.apply.checking_snapshot"));
    let distro = loaded.manifest.project.ros_distro;
    if let Some(date) = native.ros_snapshot.clone() {
        let base = rospm_core::snapshot::ros_snapshot_url(distro.as_str(), &date);
        if !probe_snapshot(&base, codename) {
            println!("{}", tr("cli.apply.snapshot_unreachable_warn"));
            native.ros_snapshot = None;
        }
    }
    if let Some(ts) = native.ubuntu_snapshot.clone() {
        let base = rospm_core::snapshot::ubuntu_snapshot_url(&ts);
        if !probe_snapshot(&base, codename) {
            println!("{}", tr("cli.apply.snapshot_unreachable_warn"));
            native.ubuntu_snapshot = None;
        }
    }
    lock
}

/// `GET <base>/dists/<codename>/InRelease` with [`SNAPSHOT_PROBE_TIMEOUT`].
fn probe_snapshot(base: &str, codename: &str) -> bool {
    let endpoint = net::Endpoint {
        role: Role::RosApt,
        source: "snapshot".into(),
        url: rospm_core::snapshot::inrelease_url(base, codename),
    };
    net::probe(&[endpoint], SNAPSHOT_PROBE_TIMEOUT)
        .into_iter()
        .next()
        .is_some_and(|r| r.ok())
}

/// Probe the sources `apply` is about to download from and pick one
/// (decision #17). Never switches silently: a mirror is used only when the
/// user passed `--mirror` / `ROSPM_MIRROR` or confirmed the prompt.
fn select_sources(
    loaded: &Loaded,
    requested: Option<Mirror>,
    ros_snapshot: bool,
) -> Result<Mirror> {
    let m = &loaded.manifest;
    let ubuntu = m.ubuntu();
    let Some(codename) = ubuntu_codename(&ubuntu) else {
        // plan_apply reports the unsupported Ubuntu release with a proper message.
        return Ok(requested.unwrap_or(mirror::OFFICIAL));
    };
    let git_urls = repos_urls(loaded);
    let mut endpoints = match requested {
        Some(mr) => net::needed(&mr, codename, !ros_snapshot),
        None => net::all_endpoints(codename, &git_urls),
    };
    if ros_snapshot {
        endpoints.retain(|e| e.role != Role::RosApt);
    }
    if requested.is_some() {
        endpoints.extend(
            net::all_endpoints(codename, &git_urls)
                .into_iter()
                .filter(|e| e.role == Role::GitHost),
        );
    }
    println!(
        "{}",
        msg!(
            "cli.apply.network_check",
            count = endpoints.len(),
            secs = net::DEFAULT_TIMEOUT.as_secs()
        )
    );
    let results = net::probe(&endpoints, net::DEFAULT_TIMEOUT);
    print_probe_table(&results);

    for r in results
        .iter()
        .filter(|r| r.endpoint.role == Role::GitHost && !r.ok())
    {
        eprintln!(
            "{}",
            msg!("cli.apply.git_host_unreachable", host = r.endpoint.source)
        );
    }

    if let Some(mr) = requested {
        return match net::mirror_ok(&results, &mr, codename, !ros_snapshot) {
            Some(true) => {
                println!(
                    "{}",
                    msg!("cli.apply.sources_line", name = mr.name, label = mr.label)
                );
                Ok(mr)
            }
            _ if mr.is_official() => {
                eprintln!("{}", tr("cli.apply.official_forced"));
                Ok(mr)
            }
            _ => bail!(
                "{}",
                msg!(
                    "cli.apply.mirror_unreachable",
                    name = mr.name,
                    names = mirror::NAMES.join(" | ")
                )
            ),
        };
    }

    match net::verdict(&results, codename, !ros_snapshot) {
        Verdict::OfficialOk => {
            println!(
                "{}",
                msg!(
                    "cli.apply.sources_line",
                    name = mirror::OFFICIAL.name,
                    label = mirror::OFFICIAL.label
                )
            );
            Ok(mirror::OFFICIAL)
        }
        Verdict::UseMirror { failed, candidates } => {
            println!("{}", tr("cli.apply.official_unreachable"));
            for f in &failed {
                println!("  - {f}");
            }
            let best = candidates[0];
            let names: Vec<&str> = candidates.iter().map(|c| c.name).collect();
            println!(
                "{}",
                msg!("cli.apply.mirrors_answered", names = names.join(", "))
            );
            let best = rospm_core::confirmation::switch_mirror(
                &super::interaction::task_id(),
                best,
                &candidates,
                ros_snapshot,
                &super::interaction::Terminal,
            )?;
            println!(
                "{}",
                msg!(
                    "cli.apply.sources_line",
                    name = best.name,
                    label = best.label
                )
            );
            Ok(best)
        }
        Verdict::NothingReachable { failed } => {
            bail!(
                "{}",
                msg!("cli.apply.nothing_reachable", count = failed.len())
            )
        }
    }
}

/// Git URLs from every `.repos` file the manifest names (best effort).
fn repos_urls(loaded: &Loaded) -> Vec<String> {
    let mut urls = Vec::new();
    for ws in loaded.manifest.workspaces.values() {
        if let Some(sources) = &ws.sources {
            if let Ok(repos) = ReposFile::load(&loaded.root.join(sources)) {
                urls.extend(repos.repositories.values().map(|r| r.url.clone()));
            }
        }
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;
    use rospm_core::manifest::MANIFEST_FILE;
    use rospm_core::probe::OsRelease;

    #[test]
    fn distro_switch_prepares_manifest_and_matching_plan_without_writing() {
        let dir = std::env::temp_dir().join(format!("rospm-distro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(MANIFEST_FILE);
        let original = include_str!("../../templates/rospm.en.toml")
            .replace("@NAME@", "demo")
            .replace("@DISTRO@", "jazzy")
            .replace("@UBUNTU@", "24.04")
            .replace(
                "# ros_base = \"ros-jazzy-ros-base\"",
                "ros_base = \"ros-jazzy-desktop\"",
            )
            .replace("# ubuntu =", "ubuntu =");
        std::fs::write(&path, &original).unwrap();
        for distro in [RosDistro::Humble, RosDistro::Rolling] {
            let mut loaded = Loaded {
                path: path.clone(),
                root: dir.clone(),
                manifest: Manifest::parse(&original, &path).unwrap(),
            };
            let mut host = Host::empty();
            host.os = Some(OsRelease::parse(&format!(
                "ID=ubuntu\nVERSION_ID={}\n",
                distro.default_ubuntu()
            )));
            host.ros_distros = vec!["jazzy".into()];
            let text = prepare_distro(&mut loaded, &host, Some(distro))
                .unwrap()
                .unwrap();
            assert_eq!(loaded.manifest.project.ros_distro, distro);
            assert_eq!(
                loaded.manifest.ros_base_package(),
                format!("ros-{distro}-desktop")
            );
            assert_eq!(loaded.manifest.ubuntu(), distro.default_ubuntu());
            assert!(text.contains("# humble | jazzy | rolling"));
            assert!(text.contains("# defaults to the distro's release"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL).unwrap();
            let plan = provider::for_manifest(&loaded.manifest)
                .plan_apply(&ctx)
                .unwrap()
                .render();
            assert!(
                plan.contains(&format!(
                    " {} main",
                    ubuntu_codename(distro.default_ubuntu()).unwrap()
                )),
                "{plan}"
            );
            assert!(
                plan.contains(&format!("apt-get install -y ros-{distro}-desktop")),
                "{plan}"
            );
            assert!(plan.contains("native.apply.other_distros_kept"));

            // Unknown or incompatible hosts must fail before touching even the in-memory manifest.
            for os in [None, Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=20.04"))] {
                host.os = os;
                let before = loaded.manifest.project.ros_distro;
                assert!(prepare_distro(&mut loaded, &host, Some(RosDistro::Jazzy)).is_err());
                assert_eq!(loaded.manifest.project.ros_distro, before);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            }
        }
        let custom = original.replace("ros-jazzy-desktop", "custom-ros");
        std::fs::write(&path, &custom).unwrap();
        let mut loaded = Loaded {
            path: path.clone(),
            root: dir.clone(),
            manifest: Manifest::parse(&custom, &path).unwrap(),
        };
        let mut host = Host::empty();
        host.os = Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=22.04"));
        prepare_distro(&mut loaded, &host, Some(RosDistro::Humble)).unwrap();
        assert_eq!(loaded.manifest.ros_base_package(), "custom-ros");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
