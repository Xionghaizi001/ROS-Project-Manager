//! Check command implementation.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use rospm_core::mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::Host;
use rospm_core::provider::{self, Context};
use rospm_core::{msg, Error};

use crate::i18n::tr;

use super::report::{print_check_report, print_env_report, EnvSummary};
use super::{describe, host_platform, load, Global};

#[derive(Serialize)]
struct CheckReport {
    ok: bool,
    manifest: String,
    project: String,
    ros_distro: String,
    provider: String,
    platforms: Vec<String>,
    evaluated_platform: String,
    build_order: Vec<String>,
    effective_apt: BTreeMap<String, Vec<String>>,
    pip_recorded: Vec<String>,
    notes: Vec<String>,
    environment: EnvSummary,
    repositories: Option<rospm_core::probe::EnvReport>,
}

pub fn check(global: &Global, target: Option<Platform>, strict: bool) -> Result<()> {
    let loaded = match load(global) {
        Ok(l) => l,
        Err(err) => {
            if global.json {
                let errors: Vec<String> = match err.downcast_ref::<Error>() {
                    Some(Error::Validation(list)) => list.iter().map(ToString::to_string).collect(),
                    _ => vec![describe(&err)],
                };
                println!("{}", serde_json::json!({ "ok": false, "errors": errors }));
                std::process::exit(1);
            }
            return Err(err);
        }
    };
    let m = &loaded.manifest;
    let host = Host::probe();
    let platform = match target {
        Some(t) => t,
        None => host_platform()?,
    };

    let mut notes = Vec::new();
    if !m.project.platforms.contains(&platform) {
        notes.push(msg!("cli.check.platform_not_listed", platform = platform).to_string());
    }
    if target.is_some() {
        notes.push(tr("cli.check.target_note"));
    }

    let mut effective_apt = BTreeMap::new();
    for p in &m.project.platforms {
        let resolved = m.resolve_apt(*p);
        notes.extend(
            resolved
                .replacements
                .iter()
                .map(|r| r.notice(*p).to_string()),
        );
        effective_apt.insert(p.to_string(), resolved.packages);
    }

    // Probe the host only when the evaluated platform *is* the host; probing
    // amd64's dpkg for an arm64 target would just report noise.
    let cross = host.platform.map(|h| h != platform).unwrap_or(true);
    let env_report = if cross {
        notes.push(
            msg!(
                "cli.check.probing_skipped",
                target = platform,
                host = host
                    .platform
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| tr("probe.platform.unsupported"))
            )
            .to_string(),
        );
        None
    } else {
        let ctx = Context {
            manifest: m,
            root: &loaded.root,
            platform,
            locked: false,
            lock: None,
            offline: false,
            host: &host,
            mirror: mirror::OFFICIAL,
        };
        Some(provider::for_manifest(m).probe_env(&ctx)?)
    };
    let environment = env_report.clone().map(EnvSummary::from).unwrap_or_default();

    let repositories = target.map(|_| {
        rospm_core::repository::check(
            m,
            &loaded.root,
            platform,
            &rospm_core::snapshot::Http::default(),
        )
    });
    let report = CheckReport {
        ok: true,
        manifest: loaded.path.display().to_string(),
        project: m.project.name.clone(),
        ros_distro: m.project.ros_distro.to_string(),
        provider: m.environment.provider.to_string(),
        platforms: m
            .project
            .platforms
            .iter()
            .map(ToString::to_string)
            .collect(),
        evaluated_platform: platform.to_string(),
        build_order: m.build_order()?,
        effective_apt,
        pip_recorded: m.dependencies.pip.clone(),
        notes,
        environment,
        repositories,
    };

    let strict_failed = strict
        && ((!cross && !report.environment.ready)
            || report.repositories.as_ref().is_some_and(|r| !r.ready()));

    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            msg!("cli.check.manifest_line", path = report.manifest)
        );
        println!(
            "{}",
            msg!(
                "cli.check.project_line",
                name = report.project,
                distro = report.ros_distro,
                provider = report.provider
            )
        );
        println!(
            "{}",
            msg!(
                "cli.check.platforms_line",
                platforms = report.platforms.join(", ")
            )
        );
        println!(
            "{}",
            msg!(
                "cli.check.build_order_line",
                order = report.build_order.join(" -> ")
            )
        );
        for (p, pkgs) in &report.effective_apt {
            if !pkgs.is_empty() {
                println!(
                    "{}",
                    msg!(
                        "cli.check.apt_line",
                        platform = p,
                        packages = pkgs.join(" ")
                    )
                );
            }
        }
        if !report.pip_recorded.is_empty() {
            println!(
                "{}",
                msg!(
                    "cli.check.pip_line",
                    packages = report.pip_recorded.join(" ")
                )
            );
        }
        for n in &report.notes {
            println!("{}", msg!("cli.check.note_line", text = n));
        }
        println!("{}", tr("cli.check.valid_line"));
        if let Some(r) = &report.repositories {
            print_check_report(r, &report.evaluated_platform, true);
        }
        if let Some(r) = &env_report {
            println!();
            print_env_report(r, &report.evaluated_platform);
        }
    }

    if strict_failed {
        std::process::exit(1);
    }
    Ok(())
}
