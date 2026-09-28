use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::{Ecosystem, LicensedPackage, Package};

const LOCKFILE: &str = "package-lock.json";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lockfile {
    lockfile_version: u32,
    #[serde(default)]
    packages: BTreeMap<String, LockEntry>,
}

#[derive(Deserialize)]
struct LockEntry {
    version: Option<String>,
    license: Option<serde_json::Value>,
    #[serde(default)]
    link: bool,
}

#[derive(Deserialize)]
struct InstalledManifest {
    version: Option<String>,
    license: Option<serde_json::Value>,
}

/// Reads the Project's `package-lock.json` and returns its Packages with
/// their Declared license, taken from the first License origin that has one:
/// the installed copy (only when its version matches the lockfile), then the
/// lockfile entry itself.
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
