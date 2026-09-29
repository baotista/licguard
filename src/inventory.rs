use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub mod npm;
mod paths;
pub mod yarn;

/// The Packages of a Project, and the Inventory sources they come from.
pub struct Inventory {
    /// Paths relative to the Project root, joined with `/`, sorted.
    pub sources: Vec<String>,
    pub packages: Vec<LicensedPackage>,
}

/// Reads every Inventory source of the Project, in sorted order, and returns
/// its Packages, each once: see [`LicensedPackage::merge`].
pub fn inventory(project: &Path) -> Result<Inventory> {
    let sources = lockfiles(project)?;
    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
    for source in &sources {
        let found = if source.rsplit('/').next() == Some(yarn::LOCKFILE) {
            yarn::inventory(project, source)?
        } else {
            npm::inventory(project, source)?
        };
        for found in found {
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

/// Finds the Project's Inventory sources: every `package-lock.json` and
/// `yarn.lock` under `project`, skipping `node_modules` and hidden
/// directories. Returns their paths relative to `project`, joined with `/`,
/// sorted.
fn lockfiles(project: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    find_lockfiles(project, "", &mut found)?;
    if found.is_empty() {
        bail!(
            "no {} or {} found under {}\nhint: run licguard at the root of an npm or Yarn Project, or run `npm install` or `yarn install` to create the lockfile",
            npm::LOCKFILE,
            yarn::LOCKFILE,
            project.display()
        );
    }
    found.sort();
    Ok(found)
}

fn find_lockfiles(dir: &Path, relative: &str, found: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read {}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{relative}{name}");
        let file_type = entry
            .file_type()
            .with_context(|| format!("cannot read {}", entry.path().display()))?;
        if file_type.is_file() && (name == npm::LOCKFILE || name == yarn::LOCKFILE) {
            found.push(path);
        } else if file_type.is_dir() && name != "node_modules" && !name.starts_with('.') {
            find_lockfiles(&entry.path(), &format!("{path}/"), found)?;
        }
    }
    Ok(())
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
    /// The 1-based line of the Package's entry in the first of its
    /// `sources`: the entry that gave it its Introduction path when there
    /// are several; `None` when unknown.
    pub line: Option<usize>,
}

impl LicensedPackage {
    /// Merges `other`, a later occurrence of the same Package: the Package is
    /// `prod` if any occurrence is, keeps the Introduction path of the first
    /// occurrence with that Scope, and the first Declared license found.
    /// Within the `same_source`, the shortest path with that Scope wins, and
    /// the line follows the path.
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
        let takes_path = if self.scope == Scope::Dev && other.scope == Scope::Prod {
            self.scope = Scope::Prod;
            true
        } else {
            same_source
                && self.scope == other.scope
                && length(&other.introduction_path) < length(&self.introduction_path)
        };
        if takes_path {
            self.introduction_path = other.introduction_path;
            if same_source {
                self.line = other.line;
            }
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
