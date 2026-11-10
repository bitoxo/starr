//! System-level domain types.

use serde::Serialize;

/// Health of a single subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Ok,
    Degraded,
}

impl Health {
    #[must_use]
    pub fn is_ok(self) -> bool {
        self == Self::Ok
    }
}
