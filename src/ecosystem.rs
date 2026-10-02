//! Ecosystems: the package worlds a Package can belong to, and what differs
//! between them once the Inventory sources have been read.

use std::fmt;

/// The ecosystem of a Package, which qualifies its name and version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Maven,
    Npm,
}

impl Ecosystem {
    /// The identifier of the ecosystem in reports, e.g. `npm`.
    pub fn as_str(self) -> &'static str {
        match self {
            Ecosystem::Maven => "maven",
            Ecosystem::Npm => "npm",
        }
    }

    /// The ecosystem of the package URLs (purls) of type `purl_type`, e.g.
    /// `maven`; `None` when it is not supported.
    pub fn from_purl_type(purl_type: &str) -> Option<Ecosystem> {
        match purl_type {
            "maven" => Some(Ecosystem::Maven),
            "npm" => Some(Ecosystem::Npm),
            _ => None,
        }
    }

    /// The name of a Package whose purl has the decoded `namespace` and
    /// `name`, as the ecosystem writes it: `group:artifact` for Maven,
    /// `@scope/name` for npm.
    pub fn package_name(self, namespace: Option<&str>, name: &str) -> String {
        match (self, namespace) {
            (Ecosystem::Maven, Some(group)) => format!("{group}:{name}"),
            (Ecosystem::Npm, Some(scope)) => format!("{scope}/{name}"),
            (_, None) => name.to_string(),
        }
    }

    /// Whether the ecosystem has a registry License origin, queried for the
    /// Packages that no local origin declares a license for.
    pub fn has_registry(self) -> bool {
        match self {
            // Maven Central is not a License origin yet.
            Ecosystem::Maven => false,
            Ecosystem::Npm => true,
        }
    }
}

impl fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
