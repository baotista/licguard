use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::{Ecosystem, LicensedPackage, Package, Scope};

mod paths;

const LOCKFILE: &str = "package-lock.json";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lockfile {
    lockfile_version: u32,
    #[serde(default)]
    packages: BTreeMap<String, LockEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockEntry {
    name: Option<String>,
    version: Option<String>,
    license: Option<serde_json::Value>,
    #[serde(default)]
    link: bool,
    /// Set by npm when the entry is reached only through `devDependencies`.
    #[serde(default)]
    dev: bool,
    /// Dependency names; their version ranges are not needed.
    #[serde(default)]
    dependencies: BTreeMap<String, IgnoredAny>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, IgnoredAny>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, IgnoredAny>,
    #[serde(default)]
    peer_dependencies: BTreeMap<String, IgnoredAny>,
}

#[derive(Deserialize)]
struct InstalledManifest {
    version: Option<String>,
    license: Option<serde_json::Value>,
}

/// Reads the Project's `package-lock.json` and returns its Packages with
/// their Declared license, taken from the first License origin that has one:
/// the installed copy (only when its version matches the lockfile), then the
/// lockfile entry itself. An entry is `dev` only when npm flags it `dev`:
/// `devOptional` and `optional` entries may ship, so they are `prod`. Each
/// Package also gets its shortest Introduction path, when it is reachable
/// from the Project root.
pub fn inventory(root: &Path) -> Result<Vec<LicensedPackage>> {
    let path = root.join(LOCKFILE);
    let text = fs::read_to_string(&path).map_err(|err| {
        anyhow!(
            "cannot read {}: {err}\nhint: run licguard at the root of an npm Project, or run `npm install` to create the lockfile",
            path.display()
        )
    })?;
    let lockfile: Lockfile = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a valid npm lockfile", path.display()))?;
    if lockfile.lockfile_version < 2 {
        bail!(
            "{} uses lockfileVersion {}, which is not supported\nhint: regenerate it with npm 7 or later",
            path.display(),
            lockfile.lockfile_version
        );
    }

    let root_name = lockfile
        .packages
        .get("")
        .and_then(|entry| entry.name.clone())
        .or_else(|| directory_name(root))
        .unwrap_or_default();
    let introduction_paths = paths::shortest(&lockfile.packages, &root_name);

    let mut packages = Vec::new();
    for (key, entry) in &lockfile.packages {
        let Some(name) = package_name(key) else {
            continue; // the root, or a Workspace member
        };
        if entry.link {
            continue;
        }
        let Some(version) = &entry.version else {
            bail!("{}: entry `{key}` has no version", path.display());
        };
        packages.push(LicensedPackage {
            declared_license: installed_license(&root.join(key), version)
                .or_else(|| entry.license.as_ref().and_then(license_string)),
            scope: if entry.dev { Scope::Dev } else { Scope::Prod },
            introduction_path: introduction_paths.get(key).cloned(),
            package: Package {
                ecosystem: Ecosystem::Npm,
                name: name.to_string(),
                version: version.clone(),
            },
        });
    }
    Ok(packages)
}

/// `node_modules/debug/node_modules/ms` -> `ms`; `node_modules/@types/ms` -> `@types/ms`.
fn package_name(key: &str) -> Option<&str> {
    key.rsplit_once("node_modules/").map(|(_, name)| name)
}

fn directory_name(root: &Path) -> Option<String> {
    let root = fs::canonicalize(root).ok()?;
    Some(root.file_name()?.to_string_lossy().into_owned())
}

fn installed_license(dir: &Path, version: &str) -> Option<String> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: InstalledManifest = serde_json::from_str(&text).ok()?;
    if manifest.version.as_deref() != Some(version) {
        return None;
    }
    manifest.license.as_ref().and_then(license_string)
}

/// Legacy object forms of the `license` field are not understood yet.
fn license_string(license: &serde_json::Value) -> Option<String> {
    license.as_str().map(str::to_string)
}
