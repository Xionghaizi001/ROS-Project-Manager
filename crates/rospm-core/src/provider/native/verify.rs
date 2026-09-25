//! `rospm verify` for the native provider (spec §4, roadmap P2).
//!
//! Compares a [`Lock`] with the host: source revisions, apt versions, pip
//! versions, manifest hash. Read-only; produces an [`EnvReport`] like `check`.

use crate::error::{Error, Result};
use crate::lock::Lock;
use crate::probe::EnvReport;
use crate::provider::Context;

pub fn verify(_ctx: &Context, _lock: &Lock) -> Result<EnvReport> {
    Err(Error::NotImplemented {
        feature: "verify",
        phase: "P2",
    })
}
