//! Core library for `rospm`.
//!
//! Layering (see `docs/spec.md`):
//! - [`manifest`]: `rospm.toml`, the *intent* — parsed, validated, topologically sorted.
//! - [`lock`]: `rospm.lock`, the *result* — what was actually resolved.
//! - [`plan`]: a list of steps (shell commands / file writes) produced by a provider.
//!   Every system-mutating command renders its plan first and honours `--dry-run`.
//! - [`provider`]: environment providers (`native`, `pixi`, `docker`) that turn a
//!   manifest into plans.
//! - [`scm`]: source-code managers (currently vcstool).
//! - [`repos`] / [`package_xml`]: read-only views of the files a ROS workspace
//!   already has (`.repos`, `package.xml`).
//! - [`probe`]: read-only facts about the host (OS, ROS installs, dpkg, rosdep).
//! - [`mirror`] / [`net`]: package-source mirrors and the reachability probe
//!   `apply` runs before downloading (decision #17).
//! - [`introspect`]: `init --from-env`, a best-effort draft manifest from the
//!   host and the project directory (decision #3).

pub mod error;
pub mod introspect;
pub mod lock;
pub mod manifest;
pub mod mirror;
pub mod net;
pub mod package_xml;
pub mod plan;
pub mod platform;
pub mod probe;
pub mod provider;
pub mod repos;
pub mod scm;

pub use error::{Error, Result};
