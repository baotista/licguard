use std::fmt;

/// A condition reported by a check that needs attention but never fails the
/// gate. Sorting puts Warnings in their reporting order.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Warning {
    /// A License clarification that matches no Package of the inventory.
    UnmatchedClarification {
        package: String,
        version: Option<String>,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Warning::UnmatchedClarification { package, version } => {
                write!(f, "clarification for {package}")?;
                if let Some(version) = version {
                    write!(f, "@{version}")?;
                }
                f.write_str(" matches no Package")
            }
        }
    }
}
