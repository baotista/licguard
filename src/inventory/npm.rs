use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::{Ecosystem, LicenseOrigin, LicensedPackage, Package, Scope};

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
    licenses: Option<serde_json::Value>,
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
    licenses: Option<serde_json::Value>,
}

/// Finds the Project's npm Inventory sources: every `package-lock.json` under
/// `project`, skipping `node_modules` and hidden directories. Returns their
/// paths relative to `project`, joined with `/`, sorted.
pub fn lockfiles(project: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    find_lockfiles(project, "", &mut found)?;
    if found.is_empty() {
        bail!(
            "no {LOCKFILE} found under {}\nhint: run licguard at the root of an npm Project, or run `npm install` to create the lockfile",
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
        if file_type.is_file() && name == LOCKFILE {
            found.push(path);
        } else if file_type.is_dir() && name != "node_modules" && !name.starts_with('.') {
            find_lockfiles(&entry.path(), &format!("{path}/"), found)?;
        }
    }
    Ok(())
}

/// Reads the `package-lock.json` at `source`, relative to `project`, and
/// returns its Packages with their Declared license, taken from the first
/// License origin that has one: the installed copy next to the lockfile (only
/// when its version matches the lockfile), then the lockfile entry itself. An
/// entry is `dev` only when npm flags it `dev`: `devOptional` and `optional`
/// entries may ship, so they are `prod`. Each Package also gets its shortest
/// Introduction path, when it is reachable from a root: the lockfile's root
/// entry `""`, then its Workspace members.
pub fn inventory(project: &Path, source: &str) -> Result<Vec<LicensedPackage>> {
    let path = project.join(source);
    let root = path.parent().unwrap_or(project);
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
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
    let mut roots = vec![(String::new(), root_name)];
    roots.extend(
        lockfile
            .packages
            .iter()
            .filter(|(key, _)| is_workspace_member(key))
            .map(|(key, entry)| (key.clone(), workspace_member_name(key, entry))),
    );
    let introduction_paths = paths::shortest(&lockfile.packages, &roots);

    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
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
        let package = Package {
            ecosystem: Ecosystem::Npm,
            name: name.to_string(),
            version: version.clone(),
        };
        let (declared_license, license_origin) = installed_license(&root.join(key), version)
            .map(|license| (license, LicenseOrigin::Installed))
            .or_else(|| {
                declared_license(entry.license.as_ref(), entry.licenses.as_ref())
                    .map(|license| (license, LicenseOrigin::Lockfile))
            })
            .unzip();
        let found = LicensedPackage {
            declared_license,
            license_origin,
            scope: if entry.dev { Scope::Dev } else { Scope::Prod },
            introduction_path: introduction_paths.get(key).cloned(),
            sources: vec![source.to_string()],
            package: package.clone(),
        };
        // The same Package can be installed at several places in the tree.
        match packages.entry(package) {
            Entry::Vacant(vacant) => {
                vacant.insert(found);
            }
            Entry::Occupied(mut occupied) => occupied.get_mut().merge(found, true),
        }
    }
    Ok(packages.into_values().collect())
}

/// `node_modules/debug/node_modules/ms` -> `ms`; `node_modules/@types/ms` -> `@types/ms`.
fn package_name(key: &str) -> Option<&str> {
    key.rsplit_once("node_modules/").map(|(_, name)| name)
}

/// A Workspace member is an entry outside `node_modules`, e.g. `packages/ui`,
/// other than the root entry `""`.
fn is_workspace_member(key: &str) -> bool {
    !key.is_empty() && package_name(key).is_none()
}

/// The entry's `name`, else the last segment of its key.
fn workspace_member_name(key: &str, entry: &LockEntry) -> String {
    entry
        .name
        .clone()
        .unwrap_or_else(|| key.rsplit('/').next().unwrap_or(key).to_string())
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
    declared_license(manifest.license.as_ref(), manifest.licenses.as_ref())
}

/// The Declared license of a manifest or lockfile entry: its `license`
/// field, else the legacy `licenses` field.
fn declared_license(
    license: Option<&serde_json::Value>,
    licenses: Option<&serde_json::Value>,
) -> Option<String> {
    license
        .or(licenses)
        .map(|l| license_string(l).unwrap_or_else(|| l.to_string()))
}

/// Reads a license field: an expression, the legacy
/// `{"type": "MIT", "url": ...}` object, or a legacy array of either, which
/// offers a choice between its options (`A OR B`), as npm documents it.
/// Returns `None` for any other shape; the caller then keeps its JSON text,
/// which never normalizes, so the Package is Unresolved rather than taking
/// its license from the next License origin.
fn license_string(license: &serde_json::Value) -> Option<String> {
    match license {
        serde_json::Value::Object(object) => object.get("type")?.as_str().map(str::to_string),
        serde_json::Value::Array(options) if !options.is_empty() => {
            let options: Option<Vec<String>> = options.iter().map(license_string).collect();
            Some(options?.join(" OR "))
        }
        _ => license.as_str().map(str::to_string),
    }
}
