//! Where the controller gets a VM's compatibility evidence just before a
//! resize.

use rampire_core::VirtioMemCompatibility;
use thiserror::Error;

/// Supplies compatibility evidence immediately before resize preparation.
pub trait CompatibilitySource {
    fn compatibility(&self) -> Result<VirtioMemCompatibility, CompatibilityError>;
}

/// Why a VM's compatibility couldn't be confirmed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompatibilityError {
    /// libvirt couldn't be asked (it refused the connection or didn't
    /// answer): nothing is known about the VM, and it passes by itself.
    #[error("{0}")]
    Unavailable(String),
    /// The VM isn't as it was checked, or its evidence couldn't be read.
    #[error("{0}")]
    Refused(String),
}

impl From<String> for CompatibilityError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

/// For callers that only report the reason.
impl From<CompatibilityError> for String {
    fn from(error: CompatibilityError) -> Self {
        error.to_string()
    }
}

/// Deterministic evidence available only to unit tests.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct FixedCompatibilitySource {
    evidence: VirtioMemCompatibility,
}

#[cfg(test)]
impl FixedCompatibilitySource {
    /// Construct a source that always returns the supplied evidence.
    pub const fn new(evidence: VirtioMemCompatibility) -> Self {
        Self { evidence }
    }
}

#[cfg(test)]
impl CompatibilitySource for FixedCompatibilitySource {
    fn compatibility(&self) -> Result<VirtioMemCompatibility, CompatibilityError> {
        Ok(self.evidence)
    }
}
