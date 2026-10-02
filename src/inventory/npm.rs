use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::{LicenseOrigin, LicensedPackage, Package, Scope, paths};
use crate::ecosystem::Ecosystem;

/// The file name of an npm Inventory source.
pub const LOCKFILE: &str = "package-lock.json";

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
    /// Where npm fetched it from; `None` in some lockfiles, e.g. written with
    /// `--package-lock-only`.
    resolved: Option<String>,
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
    let introduction_paths = paths::shortest(&roots, |key: &String, prod_only| {
        children(&lockfile.packages, key, prod_only)
    });
    let lines = entry_lines(&text);

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
            // An aliased entry, e.g. `node_modules/string-width-cjs`, names
            // the real Package, e.g. `string-width`.
            name: entry.name.as_deref().unwrap_or(name).to_string(),
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
            from_registry: entry
                .resolved
                .as_deref()
                .is_none_or(super::is_registry_tarball),
            scope: if entry.dev { Scope::Dev } else { Scope::Prod },
            introduction_path: introduction_paths.get(key).map(|(path, _)| path.clone()),
            sources: vec![source.to_string()],
            line: lines.get(key.as_str()).copied(),
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

/// The 1-based line of each `node_modules` entry of a lockfile, by key: the
/// line that starts with its `"node_modules/…":` key, as npm writes it.
fn entry_lines(text: &str) -> HashMap<&str, usize> {
    let mut lines = HashMap::new();
    for (number, line) in text.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix('"') else {
            continue;
        };
        if let Some((key, after)) = rest.split_once('"')
            && package_name(key).is_some()
            && after.trim_start().starts_with(':')
        {
            lines.entry(key).or_insert(number + 1);
        }
    }
    lines
}

/// The dependencies of the entry at `key`, as `(name, key)` pairs: `link`
/// entries are not followed, and with `prod_only`, neither `devDependencies`
/// nor `dev` entries.
fn children(
    packages: &BTreeMap<String, LockEntry>,
    key: &str,
    prod_only: bool,
) -> Vec<(String, String)> {
    let Some(entry) = packages.get(key) else {
        return Vec::new();
    };
    let dev_dependencies = (!prod_only).then_some(&entry.dev_dependencies);
    [
        Some(&entry.dependencies),
        dev_dependencies,
        Some(&entry.optional_dependencies),
        Some(&entry.peer_dependencies),
    ]
    .into_iter()
    .flatten()
    .flat_map(|deps| deps.keys())
    .filter_map(|name| {
        let child = resolve(packages, key, name)?;
        let child_entry = &packages[&child];
        let skipped = child_entry.link || (prod_only && child_entry.dev);
        (!skipped).then(|| (name.clone(), child))
    })
    .collect()
}

/// Finds the entry that `name` resolves to when required from the entry at
/// `from`, the way Node does: `<from>/node_modules/<name>`, then the same in
/// each parent `node_modules` level, up to `node_modules/<name>`.
fn resolve(packages: &BTreeMap<String, LockEntry>, from: &str, name: &str) -> Option<String> {
    let mut dir = from;
    loop {
        let candidate = if dir.is_empty() {
            format!("node_modules/{name}")
        } else {
            format!("{dir}/node_modules/{name}")
        };
        if packages.contains_key(&candidate) {
            return Some(candidate);
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir
            .rsplit_once("/node_modules/")
            .map_or("", |(parent, _)| parent);
    }
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

pub(super) fn directory_name(root: &Path) -> Option<String> {
    let root = fs::canonicalize(root).ok()?;
    Some(root.file_name()?.to_string_lossy().into_owned())
}

pub(super) fn installed_license(dir: &Path, version: &str) -> Option<String> {
    let text = fs::read_to_string(dir.join("package.json")).ok()?;
    let manifest: InstalledManifest = serde_json::from_str(&text).ok()?;
    if manifest.version.as_deref() != Some(version) {
        return None;
    }
    declared_license(manifest.license.as_ref(), manifest.licenses.as_ref())
}

/// The Declared license of a manifest or lockfile entry: its `license`
/// field, else the legacy `licenses` field.
pub(crate) fn declared_license(
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
