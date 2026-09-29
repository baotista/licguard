use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::Path;

use anyhow::Result;

pub mod npm;

/// The Packages of a Project, and the Inventory sources they come from.
pub struct Inventory {
    /// Paths relative to the Project root, joined with `/`, sorted.
    pub sources: Vec<String>,
    pub packages: Vec<LicensedPackage>,
}

/// Reads every Inventory source of the Project, in sorted order, and returns
/// its Packages, each once: see [`LicensedPackage::merge`].
pub fn inventory(project: &Path) -> Result<Inventory> {
    let sources = npm::lockfiles(project)?;
    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
    for source in &sources {
        for found in npm::inventory(project, source)? {
            match packages.entry(found.package.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(found);
                }
                Entry::Occupied(mut entry) => entry.get_mut().merge(found, false),
            }
        }
    }
    Ok(Inventory {
        sources,
        packages: packages.into_values().collect(),
    })
}

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

/// A Package together with its Declared license, if one was found, and how
/// the Project uses it.
#[derive(Debug)]
pub struct LicensedPackage {
    pub package: Package,
    pub declared_license: Option<String>,
    /// Where the Declared license came from; `None` when there is none.
    pub license_origin: Option<LicenseOrigin>,
    pub scope: Scope,
    /// Package names from a root (the Project root or a Workspace member) to
    /// this Package; `None` when it is not reachable from any root.
    pub introduction_path: Option<Vec<String>>,
    /// The Inventory sources the Package was found in, sorted.
    pub sources: Vec<String>,
}

impl LicensedPackage {
    /// Merges `other`, a later occurrence of the same Package: the Package is
    /// `prod` if any occurrence is, keeps the Introduction path of the first
    /// occurrence with that Scope, and the first Declared license found.
    /// Within the `same_source`, the shortest path with that Scope wins.
    fn merge(&mut self, other: LicensedPackage, same_source: bool) {
        for source in other.sources {
            if !self.sources.contains(&source) {
                self.sources.push(source);
            }
        }
        if self.declared_license.is_none() {
            self.declared_license = other.declared_license;
            self.license_origin = other.license_origin;
        }
        let length = |path: &Option<Vec<String>>| path.as_ref().map_or(usize::MAX, Vec::len);
        if self.scope == Scope::Dev && other.scope == Scope::Prod {
            self.scope = Scope::Prod;
            self.introduction_path = other.introduction_path;
        } else if same_source
            && self.scope == other.scope
            && length(&other.introduction_path) < length(&self.introduction_path)
        {
            self.introduction_path = other.introduction_path;
        }
    }
}

/// Whether a Dependency is needed by what the Project ships (`Prod`) or only
/// to build and test it (`Dev`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Prod,
    Dev,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Prod => "prod",
            Scope::Dev => "dev",
        }
    }
}

/// Where a Package's license information came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseOrigin {
    Clarification,
    /// The installed copy of the Package, e.g. its `node_modules` manifest.
    Installed,
    /// The Inventory source itself.
    Lockfile,
}

impl LicenseOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            LicenseOrigin::Clarification => "clarification",
            LicenseOrigin::Installed => "installed",
            LicenseOrigin::Lockfile => "lockfile",
        }
    }
}
