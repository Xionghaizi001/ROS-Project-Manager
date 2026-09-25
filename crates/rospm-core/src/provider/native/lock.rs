//! `rospm lock` for the native provider (spec §3, roadmap P2).
//!
//! Resolves what the host currently has — source checkouts, installed apt
//! versions, pip versions, the snapshot dates the apt state corresponds to —
//! into a [`Lock`]. Read-only on the system.

use crate::error::{Error, Result};
use crate::lock::Lock;
use crate::provider::Context;

pub fn resolve(_ctx: &Context) -> Result<Lock> {
    Err(Error::NotImplemented {
        feature: "lock",
        phase: "P2",
    })
}
