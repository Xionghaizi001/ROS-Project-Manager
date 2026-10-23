//! Evidence-based dependency advice. This layer consumes read-only probe results;
//! it never resolves names heuristically, runs installers, or edits declarations.
use super::{Code, Diagnostic};
use crate::manifest::{Manifest, ProviderKind};
use crate::package_xml::PackageXml;
use crate::platform::Platform;
use crate::probe::{EnvItem, EnvReport, Status};
use crate::provider::native::deps::DepsReport;
use crate::{msg, Message};

/// A verified association, e.g. from a package file index or upstream metadata.
/// rosdep resolution alone maps keys to installer packages, NOT headers/modules
/// to keys. Do not construct this from a similar name or a guessed prefix.
#[derive(Debug, Clone)]
pub struct DependencyMapping {
    pub code: Code,
    /// Exact name reported by the parser: header, CMake package, library or module.
    pub name: String,
    pub target: DependencyTarget,
    /// Human-readable provenance (index path, metadata URL, etc.). Empty sources
    /// are ignored so a caller cannot accidentally omit the evidence's origin.
    pub source: String,
}

#[derive(Debug, Clone)]
pub enum DependencyTarget {
    RosdepKey(String),
    AptPackage(String),
}

/// All reports must belong to this manifest and the failing command's target
/// host/platform. Reuse native `probe_deps` / `probe_env`; do not pass reports
/// from a different host, provider, or an earlier environment configuration.
pub struct DependencyContext<'a> {
    pub manifest: &'a Manifest,
    pub platform: Platform,
    /// Package owning the failed command, determined from colcon's per-package
    /// log. None for commands without known package ownership.
    pub package: Option<&'a PackageXml>,
    /// Packages discovered across the project, including underlays.
    pub project_packages: &'a [PackageXml],
    pub deps: Option<&'a DepsReport>,
    pub environment: Option<&'a EnvReport>,
    pub mappings: &'a [DependencyMapping],
    /// True only if the caller verified that the failing Python uses the native
    /// system environment. Parent-shell Python and dpkg alone are not evidence.
    pub system_python: bool,
}

/// Append deduplicated advice, including to chained exceptions. Tool facts,
/// raw output, locations and failure codes are never replaced. No I/O occurs.
/// CMake names matching an active package.xml dependency can identify the key
/// to check, but do not establish artifact ownership; concrete restoration
/// commands still require an explicit mapping and a successful missing probe.
pub fn suggest_dependencies(diagnostics: &mut [Diagnostic], ctx: &DependencyContext<'_>) {
    for d in diagnostics {
        suggest_dependencies(&mut d.related, ctx);
        if !matches!(
            d.code,
            Code::MissingHeader
                | Code::PackageNotFound
                | Code::LibraryNotFound
                | Code::ModuleNotFound
                | Code::ImportError
        ) {
            // In particular, an undefined symbol does not imply a missing package.
            continue;
        }
        push(d, msg!("diagnostic.dependency_unconfirmed"));
        let m = ctx.manifest;
        if m.environment.provider != ProviderKind::Native {
            push(
                d,
                msg!(
                    "diagnostic.dependency_provider",
                    provider = m.environment.provider.as_str()
                ),
            );
            continue;
        }
        if matches!(d.code, Code::ModuleNotFound | Code::ImportError) {
            push(d, msg!("diagnostic.python_check_environment"));
            if !m.dependencies.pip.is_empty() {
                push(d, msg!("diagnostic.dependency_pip_record_only"));
            }
            if m.build.conda.is_some() || !ctx.system_python {
                push(d, msg!("diagnostic.dependency_python_scope"));
                continue;
            }
        }
        let Some(name) = artifact(d) else { continue };
        // A same-named declared key is useful context, not a verified mapping.
        let has_mapping = ctx.mappings.iter().any(|mapping| {
            mapping.code == d.code && mapping.name == name && !mapping.source.trim().is_empty()
        });
        if d.code == Code::PackageNotFound && !has_mapping && declared(ctx, &name) {
            advise_key(d, ctx, &name, false);
        }
        let code = d.code;
        for mapping in ctx.mappings.iter().filter(|mapping| {
            mapping.code == code && mapping.name == name && !mapping.source.trim().is_empty()
        }) {
            push(
                d,
                msg!(
                    "diagnostic.dependency_mapping",
                    artifact = name,
                    source = mapping.source
                ),
            );
            match &mapping.target {
                DependencyTarget::RosdepKey(key) => advise_key(d, ctx, key, true),
                DependencyTarget::AptPackage(package) => {
                    if !m.effective_apt(ctx.platform).contains(package) {
                        push(
                            d,
                            msg!("diagnostic.dependency_apt_undeclared", package = package),
                        );
                        continue;
                    }
                    match row(ctx.environment, "apt", package).map(|r| r.status) {
                        Some(Status::Missing) => push(
                            d,
                            msg!("diagnostic.dependency_apt_missing", package = package),
                        ),
                        Some(Status::Ok) => push(
                            d,
                            msg!("diagnostic.dependency_present", dependency = package),
                        ),
                        _ => push(
                            d,
                            msg!("diagnostic.dependency_check_unknown", dependency = package),
                        ),
                    }
                }
            }
        }
    }
}

fn declared(ctx: &DependencyContext<'_>, key: &str) -> bool {
    ctx.package.is_some_and(|p| {
        p.depends
            .iter()
            .any(|dep| dep.name == key && dep.applies(ctx.manifest.project.ros_distro.as_str()))
    })
}

fn advise_key(d: &mut Diagnostic, ctx: &DependencyContext<'_>, key: &str, mapped: bool) {
    if ctx.project_packages.iter().any(|p| p.name == key) {
        push(d, msg!("diagnostic.dependency_internal", dependency = key));
        return;
    }
    if !declared(ctx, key) {
        push(
            d,
            msg!("diagnostic.dependency_undeclared", dependency = key),
        );
        return;
    }
    if ctx
        .manifest
        .dependencies
        .rosdep
        .skip_keys
        .iter()
        .any(|k| k == key)
    {
        push(d, msg!("diagnostic.dependency_skipped", dependency = key));
        return;
    }
    if !ctx.manifest.dependencies.rosdep.enabled {
        push(d, msg!("diagnostic.dependency_rosdep_disabled"));
        return;
    }
    let report = ctx.deps.filter(|report| {
        report.external.contains(key)
            && !report.skipped.contains(key)
            && !report.internal.contains(key)
    });
    let item = row(report.map(|r| &r.environment), "deps", key);
    match item {
        Some(item) if item.status == Status::Missing && mapped => {
            // Native deps reports mark the whole resolution missing if ANY apt
            // package is absent. Never claim every resolved package is missing.
            if let Message::Text(packages) = &item.expected {
                if !packages.trim().is_empty() {
                    push(
                        d,
                        msg!(
                            "diagnostic.dependency_rosdep_missing",
                            dependency = key,
                            packages = packages
                        ),
                    );
                    return;
                }
            }
            push(
                d,
                msg!("diagnostic.dependency_check_unknown", dependency = key),
            );
        }
        Some(item) if item.status == Status::Ok => {
            push(d, msg!("diagnostic.dependency_present", dependency = key))
        }
        _ => push(
            d,
            msg!("diagnostic.dependency_check_unknown", dependency = key),
        ),
    }
}

fn row<'a>(report: Option<&'a EnvReport>, area: &str, subject: &str) -> Option<&'a EnvItem> {
    let mut matches = report?
        .items
        .iter()
        .filter(|r| r.area == area && matches!(&r.subject, Message::Text(s) if s == subject));
    let first = matches.next()?;
    // Conflicting/duplicate evidence is not a reliable missing verdict.
    matches.next().is_none().then_some(first)
}

fn artifact(d: &Diagnostic) -> Option<String> {
    let arg = match d.code {
        Code::MissingHeader => "header",
        Code::PackageNotFound => "package",
        Code::LibraryNotFound => "library",
        Code::ModuleNotFound => "message",
        _ => return None,
    };
    let value = &d.explanation.args().iter().find(|(key, _)| *key == arg)?.1;
    if d.code == Code::ModuleNotFound {
        // Keep dotted names intact; never guess their root distribution name.
        let rest = value.strip_prefix("No module named '")?;
        let (name, _) = rest.split_once('\'')?;
        (!name.is_empty()).then(|| name.to_string())
    } else {
        (!value.is_empty() && value != "?").then(|| value.clone())
    }
}

fn push(d: &mut Diagnostic, message: Message) {
    if !d.suggestions.contains(&message) {
        d.suggestions.push(message);
    }
}

#[cfg(test)]
mod tests;
