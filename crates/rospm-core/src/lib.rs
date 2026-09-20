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

pub mod error;
pub mod lock;
pub mod manifest;
pub mod plan;
pub mod platform;
pub mod provider;
pub mod scm;

pub use error::{Error, Result};
