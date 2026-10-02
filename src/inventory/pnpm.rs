use std::collections::btree_map::Entry as MapEntry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use super::{LicenseOrigin, LicensedPackage, Package, Scope, npm, paths};
use crate::ecosystem::Ecosystem;

/// The file name of a pnpm Inventory source.
pub const LOCKFILE: &str = "pnpm-lock.yaml";

/// Just enough of a `pnpm-lock.yaml` to tell its format.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    lockfile_version: Option<Version>,
}

/// `lockfileVersion`, a string since pnpm 8 but a number before.
#[derive(Deserialize)]
#[serde(untagged)]
enum Version {
    Text(String),
    Number(f64),
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Version::Text(text) => f.write_str(text),
            Version::Number(number) => write!(f, "{number}"),
        }
    }
}

/// The supported formats: `lockfileVersion` `'6.0'` (pnpm 8) and `'9.0'`
/// (pnpm 9 and later).
#[derive(Clone, Copy)]
enum Format {
    V6,
    V9,
}

impl Format {
    /// The key of the `packages` (v6) or `snapshots` (v9) entry that the
    /// dependency `name` at `reference` resolves to, e.g. `ms` at
    /// `2.1.2` -> `/ms@2.1.2` (v6) or `ms@2.1.2` (v9); `None` for a `link:`
    /// to a Workspace member or a local directory. An aliased dependency's
    /// reference already names the real Package, e.g. `string-width@4.2.3`
    /// (v9) or `/string-width@4.2.3` (v6), as does a v6 non-registry one.
    fn key(self, name: &str, reference: &str) -> Option<String> {
        if reference.starts_with("link:") {
            return None;
        }
        let aliased = match self {
            Format::V6 => reference.starts_with("file:") || strip_peers(reference).contains('/'),
            Format::V9 => {
                let at = reference.find('@');
                at.is_some_and(|at| reference.find([':', '(']).is_none_or(|other| at < other))
            }
        };
        if aliased {
            return Some(reference.to_string());
        }
        Some(match self {
            Format::V6 => format!("/{name}@{reference}"),
            Format::V9 => format!("{name}@{reference}"),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lockfile {
    #[serde(default)]
    importers: BTreeMap<String, Importer>,
    /// The single importer of a v6 lockfile without workspaces.
    #[serde(flatten)]
    root: Importer,
    /// v6: every Package with its dependencies; v9: its metadata only.
    #[serde(default)]
    packages: BTreeMap<String, LockEntry>,
    /// v9: every Package with its dependencies.
    #[serde(default)]
    snapshots: BTreeMap<String, LockEntry>,
}

/// The Project root (`.`) or a Workspace member, by directory.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Importer {
    #[serde(default)]
    dependencies: BTreeMap<String, ImporterDependency>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, ImporterDependency>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, ImporterDependency>,
}

#[derive(Deserialize)]
struct ImporterDependency {
    /// The reference it resolves to, e.g. `4.3.4(supports-color@7.2.0)` or
    /// `link:packages/ui`.
    version: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LockEntry {
    /// Set for a v6 Package whose key does not give it, e.g. a tarball.
    name: Option<String>,
    version: Option<String>,
    /// Where it comes from; `None` in a v9 snapshot.
    resolution: Option<Resolution>,
    /// Dependency names, with their references.
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Resolution {
    /// `directory` for a local directory, e.g. a `file:./local` dependency,
    /// or `git`.
    #[serde(rename = "type")]
    kind: Option<String>,
    integrity: Option<String>,
    tarball: Option<String>,
    repo: Option<String>,
    directory: Option<String>,
}

impl Resolution {
    /// Whether it comes from an npm registry: it has only an `integrity`, or
    /// a `tarball` with npm's registry layout.
    fn is_registry(&self) -> bool {
        let others = self.kind.is_none() && self.repo.is_none() && self.directory.is_none();
        others
            && match &self.tarball {
                Some(tarball) => super::is_registry_tarball(tarball),
                None => self.integrity.is_some(),
            }
    }
}

#[derive(Deserialize)]
struct Manifest {
    name: Option<String>,
}

/// A node of the dependency graph: an importer, by index, or a `packages`
/// (v6) or `snapshots` (v9) entry, by key.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    Importer(usize),
    Package(String),
}

/// Reads the `pnpm-lock.yaml` at `source`, relative to `project`, and
/// returns its Packages. Its roots are its importers: the Project root `.`
/// and its Workspace members, named after their `package.json`. A Package
/// is `prod` when an importer reaches it through `dependencies` or
/// `optionalDependencies`, `dev` when only through `devDependencies`, and
/// `prod` when no importer reaches it. The lockfile records no licenses:
/// the only License origin is the installed copy in pnpm's virtual store,
/// `node_modules/.pnpm/<name>@<version>[_<peers>]/node_modules/<name>`,
/// next to the lockfile, when its version matches.
pub fn inventory(project: &Path, source: &str) -> Result<Vec<LicensedPackage>> {
    let path = project.join(source);
    let root = path.parent().unwrap_or(project);
    let text = fs::read_to_string(&path).map_err(|err| {
        anyhow!(
            "cannot read {}: {err}\nhint: check that it is readable",
            path.display()
        )
    })?;
    let unreadable = |err: serde_yaml_ng::Error| {
        anyhow!(
            "{} is not a pnpm lockfile licguard can read: {err}\nhint: regenerate it with `pnpm install`",
            path.display()
        )
    };
    let header: Header = serde_yaml_ng::from_str(&text).map_err(unreadable)?;
    let format = match &header.lockfile_version {
        Some(Version::Text(version)) if version == "6.0" => Format::V6,
        Some(Version::Text(version)) if version == "9.0" => Format::V9,
        version => bail!(
            "{} uses lockfileVersion {}, which is not supported\nhint: regenerate it with pnpm 8 or later",
            path.display(),
            version
                .as_ref()
                .map_or_else(|| "(none)".to_string(), Version::to_string)
        ),
    };
    let mut lockfile: Lockfile = serde_yaml_ng::from_str(&text).map_err(unreadable)?;
    if lockfile.importers.is_empty() {
        lockfile
            .importers
            .insert(".".to_string(), std::mem::take(&mut lockfile.root));
    }
    let (graph, section) = match format {
        Format::V6 => (&lockfile.packages, "packages"),
        Format::V9 => (&lockfile.snapshots, "snapshots"),
    };
    let lines = entry_lines(&text, section);

    let importers: Vec<(&String, &Importer)> = lockfile.importers.iter().collect();
    let roots: Vec<(Node, String)> = importers
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (Node::Importer(index), importer_name(root, id)))
        .collect();
    let children = |node: &Node, prod_only: bool| -> Vec<(String, Node)> {
        let edges: Vec<(&String, &String)> = match node {
            Node::Importer(index) => {
                let importer = importers[*index].1;
                let dev = (!prod_only).then_some(&importer.dev_dependencies);
                [
                    Some(&importer.dependencies),
                    dev,
                    Some(&importer.optional_dependencies),
                ]
                .into_iter()
                .flatten()
                .flatten()
                .map(|(name, dependency)| (name, &dependency.version))
                .collect()
            }
            Node::Package(key) => graph.get(key).map_or_else(Vec::new, |entry| {
                entry
                    .dependencies
                    .iter()
                    .chain(&entry.optional_dependencies)
                    .collect()
            }),
        };
        edges
            .into_iter()
            .filter_map(|(name, reference)| {
                let key = format.key(name, reference)?;
                graph
                    .contains_key(&key)
                    .then(|| (name.clone(), Node::Package(key)))
            })
            .collect()
    };
    let introduction_paths = paths::shortest(&roots, children);

    let store = root.join("node_modules").join(".pnpm");
    let mut store_dirs: Vec<String> = fs::read_dir(&store)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    store_dirs.sort();

    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
    for (key, entry) in graph {
        let metadata = match format {
            Format::V6 => Some(entry),
            Format::V9 => lockfile.packages.get(strip_peers(key)),
        };
        let resolution = metadata.and_then(|m| m.resolution.as_ref());
        if resolution.and_then(|r| r.kind.as_deref()) == Some("directory") {
            continue; // part of the Project, like a Workspace member
        }
        let package = package(format, key, metadata).ok_or_else(|| {
            anyhow!(
                "{}: entry `{key}` has no version\nhint: regenerate it with `pnpm install`",
                path.display()
            )
        })?;
        let declared_license = installed_license(&store, &store_dirs, &package);
        let (introduction_path, scope) = match introduction_paths.get(&Node::Package(key.clone())) {
            Some((path, scope)) => (Some(path.clone()), *scope),
            None => (None, Scope::Prod),
        };
        let found = LicensedPackage {
            license_origin: declared_license.as_ref().map(|_| LicenseOrigin::Installed),
            from_registry: resolution.is_some_and(Resolution::is_registry),
            declared_license,
            scope,
            introduction_path,
            sources: vec![source.to_string()],
            line: lines.get(key.as_str()).copied(),
            package: package.clone(),
        };
        // The same Package has one entry per set of resolved peers.
        match packages.entry(package) {
            MapEntry::Vacant(vacant) => {
                vacant.insert(found);
            }
            MapEntry::Occupied(mut occupied) => occupied.get_mut().merge(found, true),
        }
    }
    Ok(packages.into_values().collect())
}

/// The 1-based line of each entry of the top-level `section`, by key: a
/// line indented by two spaces with the key, quoted or not, followed by `:`,
/// as pnpm writes it.
fn entry_lines<'a>(text: &'a str, section: &str) -> HashMap<&'a str, usize> {
    let mut lines = HashMap::new();
    let mut current = None;
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if !line.starts_with(' ') {
            current = line.strip_suffix(':');
            continue;
        }
        let Some(content) = line.strip_prefix("  ") else {
            continue;
        };
        if current != Some(section) || content.starts_with(' ') {
            continue;
        }
        let key = match content.strip_prefix('\'') {
            Some(quoted) => quoted.split_once("':").map(|(key, _)| key),
            None => content
                .strip_suffix(':')
                .or_else(|| content.split_once(": ").map(|(key, _)| key)),
        };
        if let Some(key) = key {
            lines.entry(key).or_insert(number + 1);
        }
    }
    lines
}

/// The name of the importer `id`, a directory relative to `root`: the
/// `name` of its `package.json`, else the name of the directory.
fn importer_name(root: &Path, id: &str) -> String {
    let dir = id
        .split('/')
        .fold(root.to_path_buf(), |dir, segment| dir.join(segment));
    fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Manifest>(&text).ok())
        .and_then(|manifest| manifest.name)
        .or_else(|| npm::directory_name(&dir))
        .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(id).to_string())
}

/// The Package of the entry at `key`, e.g. `debug@4.3.4(supports-color@7.2.0)`
/// (v9) or `/debug@4.3.4(supports-color@7.2.0)` (v6): its name and its
/// version without the peers, unless its `metadata` (the v6 entry itself, or
/// the v9 `packages` entry) gives them, e.g. for a tarball.
fn package(format: Format, key: &str, metadata: Option<&LockEntry>) -> Option<Package> {
    let key = strip_peers(key);
    let bare = match format {
        Format::V6 => key.strip_prefix('/').unwrap_or(key),
        Format::V9 => key,
    };
    let (name, version) = bare
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '@')
        .map_or((bare, None), |(at, _)| (&bare[..at], Some(&bare[at + 1..])));
    let name = metadata.and_then(|m| m.name.as_deref()).unwrap_or(name);
    let version = metadata.and_then(|m| m.version.as_deref()).or(version)?;
    Some(Package {
        ecosystem: Ecosystem::Npm,
        name: name.to_string(),
        version: version.to_string(),
    })
}

/// `debug@4.3.4(supports-color@7.2.0)` -> `debug@4.3.4`.
fn strip_peers(reference: &str) -> &str {
    reference
        .split_once('(')
        .map_or(reference, |(bare, _)| bare)
}

/// The Declared license of the installed copy of `package` in the virtual
/// `store`, whose directories are `store_dirs`: one per set of peers, e.g.
/// `@types+ms@0.7.34` or `debug@4.3.4_supports-color@7.2.0`.
fn installed_license(store: &Path, store_dirs: &[String], package: &Package) -> Option<String> {
    let prefix = format!("{}@{}", package.name.replace('/', "+"), package.version);
    store_dirs
        .iter()
        .filter(|dir| {
            dir.strip_prefix(&prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
        })
        .find_map(|dir| {
            let installed = store.join(dir).join("node_modules").join(&package.name);
            npm::installed_license(&installed, &package.version)
        })
}
