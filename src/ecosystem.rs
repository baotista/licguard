//! Ecosystems: the package worlds a Package can belong to, and what differs
//! between them once the Inventory sources have been read.

use std::fmt;

/// The ecosystem of a Package, which qualifies its name and version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Npm,
}

impl Ecosystem {
    /// The identifier of the ecosystem in reports, e.g. `npm`.
    pub fn as_str(self) -> &'static str {
        match self {
            Ecosystem::Npm => "npm",
        }
    }

    /// Whether the ecosystem has a registry License origin, queried for the
    /// Packages that no local origin declares a license for.
    pub fn has_registry(self) -> bool {
        match self {
            Ecosystem::Npm => true,
        }
    }
}

impl fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
