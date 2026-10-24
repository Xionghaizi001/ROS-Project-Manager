//! Doctor command implementation.

use anyhow::Result;
use serde::Serialize;

use rospm_core::msg;
use rospm_core::net::{self, ProbeResult};
use rospm_core::probe::{Host, DEFAULT_LOCALE_FILE, NATIVE_CORE_TOOLS, TOOLS};

use crate::i18n::{self, tr};

use super::report::print_probe_table;
use super::Global;

#[derive(Serialize)]
struct ToolStatus {
    tool: String,
    purpose: String,
    found: Option<String>,
}

/// System default (`/etc/default/locale`, what `apply` fixes) versus the
/// current shell (the caller's choice, left alone).
#[derive(Serialize)]
struct LocaleReport {
    system_file: bool,
    system: Option<String>,
    /// `None` when there is no readable system file.
    system_utf8: Option<bool>,
    shell: Option<String>,
    shell_utf8: bool,
}

#[derive(Serialize)]
struct DoctorReport {
    ok: bool,
    platform: Option<String>,
    os: Option<String>,
    ros_distros_installed: Vec<String>,
    /// apt source file referencing packages.ros.org/ros2, if configured.
    ros_apt_source: Option<String>,
    apt_universe: Option<bool>,
    locale: LocaleReport,
    rosdep_initialized: bool,
    rosdep_updated: bool,
    tools: Vec<ToolStatus>,
    /// Reachability of the official sources and known mirrors (`None` with `--no-network`).
    network: Option<Vec<ProbeResult>>,
}

pub fn doctor(global: &Global, no_network: bool) -> Result<()> {
    let host = Host::probe();
    let network = if no_network {
        None
    } else {
        let codename = host
            .os
            .as_ref()
            .and_then(|os| os.codename.clone())
            .unwrap_or_else(|| "jammy".into());
        let endpoints = net::all_endpoints(&codename, &[]);
        Some(net::probe(&endpoints, net::DEFAULT_TIMEOUT))
    };
    let tools: Vec<ToolStatus> = TOOLS
        .iter()
        .map(|(tool, purpose)| ToolStatus {
            tool: tool.to_string(),
            purpose: tr(purpose),
            found: host.tool(tool).map(|p| p.display().to_string()),
        })
        .collect();

    // Core toolchain for the native provider must be present.
    let core_ok = NATIVE_CORE_TOOLS.iter().all(|t| host.has_tool(t));

    let report = DoctorReport {
        ok: core_ok,
        platform: host.platform.map(|p| p.to_string()),
        os: host.os.as_ref().and_then(|o| o.pretty_name.clone()),
        ros_distros_installed: host.ros_distros.clone(),
        ros_apt_source: host
            .apt_sources
            .ros2
            .as_ref()
            .map(|p| p.display().to_string()),
        apt_universe: host.apt_sources.universe,
        locale: LocaleReport {
            system_file: host.locale.system_file,
            system: host.locale.system.clone(),
            system_utf8: host.locale.system_utf8(),
            shell: host.locale.process.clone(),
            shell_utf8: host.locale.process_utf8(),
        },
        rosdep_initialized: host.rosdep.initialized,
        rosdep_updated: host.rosdep.updated,
        tools,
        network,
    };

    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "{}",
            msg!(
                "cli.doctor.platform",
                value = report
                    .platform
                    .clone()
                    .unwrap_or_else(|| tr("probe.platform.unsupported"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.os",
                value = report
                    .os
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.unknown"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.ros_installs",
                value = if report.ros_distros_installed.is_empty() {
                    tr("cli.doctor.no_ros")
                } else {
                    report.ros_distros_installed.join(", ")
                }
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.ros_apt_source",
                value = report
                    .ros_apt_source
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.source_not_configured"))
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.universe",
                value = match report.apt_universe {
                    Some(true) => tr("probe.universe.enabled"),
                    Some(false) => tr("cli.doctor.universe_not_enabled"),
                    None => tr("cli.doctor.unknown"),
                }
            )
        );
        let system_value = report.locale.system.as_deref().unwrap_or("POSIX");
        let shell_value = report.locale.shell.as_deref().unwrap_or("POSIX");
        println!(
            "{}",
            msg!(
                "cli.doctor.locale_system",
                value = match report.locale.system_utf8 {
                    Some(true) => format!("{system_value} ({DEFAULT_LOCALE_FILE})"),
                    Some(false) => msg!(
                        "cli.doctor.not_utf8",
                        value = system_value,
                        file = DEFAULT_LOCALE_FILE
                    )
                    .to_string(),
                    None =>
                        msg!("cli.doctor.locale_no_file", file = DEFAULT_LOCALE_FILE).to_string(),
                }
            )
        );
        println!(
            "{}",
            msg!(
                "cli.doctor.locale_shell",
                value = if report.locale.shell_utf8 {
                    shell_value.to_string()
                } else {
                    msg!("cli.doctor.shell_not_utf8", value = shell_value).to_string()
                }
            )
        );
        let done_or_not = |done: bool| {
            if done {
                tr("cli.doctor.done")
            } else {
                tr("cli.doctor.not_run")
            }
        };
        println!(
            "{}",
            msg!(
                "cli.doctor.rosdep",
                init = done_or_not(report.rosdep_initialized),
                update = done_or_not(report.rosdep_updated)
            )
        );
        for t in &report.tools {
            let mark = if t.found.is_some() { "ok " } else { "-- " };
            println!(
                "{mark} {:<12} {} {}",
                t.tool,
                i18n::pad(&t.purpose, 45),
                t.found
                    .clone()
                    .unwrap_or_else(|| tr("cli.doctor.not_found"))
            );
        }
        if !report.ok {
            println!(
                "{}",
                msg!(
                    "cli.doctor.core_missing",
                    tools = NATIVE_CORE_TOOLS.join(", ")
                )
            );
        }
        match &report.network {
            Some(results) => {
                println!(
                    "{}",
                    msg!(
                        "cli.doctor.network_header",
                        secs = net::DEFAULT_TIMEOUT.as_secs()
                    )
                );
                print_probe_table(results);
            }
            None => println!("{}", tr("cli.doctor.network_skipped")),
        }
    }
    if report.ok {
        Ok(())
    } else {
        std::process::exit(1)
    }
}
