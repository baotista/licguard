pub mod npm;

/// A published artifact identified by ecosystem, name and version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub ecosystem: Ecosystem,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Npm,
}

impl std::fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ecosystem::Npm => f.write_str("npm"),
        }
    }
}

/// A Package together with its Declared license, if one was found.
#[derive(Debug)]
pub struct LicensedPackage {
    pub package: Package,
    pub declared_license: Option<String>,
}
