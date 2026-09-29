use std::collections::btree_map::Entry as MapEntry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use super::{Ecosystem, LicenseOrigin, LicensedPackage, Package, Scope, npm, paths};

mod berry;
mod v1;

/// The file name of a Yarn Inventory source.
pub const LOCKFILE: &str = "yarn.lock";

/// A `yarn.lock` entry, in either format: one resolved Package.
struct Entry {
    /// The descriptors that resolve to this entry, e.g. `ms@^2.1.3` (v1)
    /// or `ms@npm:^2.1.3` (Berry).
    descriptors: Vec<String>,
    name: String,
    version: String,
    /// Its dependencies of any kind, as `(name, descriptor)` pairs.
    dependencies: Vec<(String, String)>,
}

/// A parsed `yarn.lock`.
struct Lockfile {
    format: Format,
    entries: Vec<Entry>,
    /// The directories of the Workspace members, relative to the lockfile's,
    /// when the lockfile records them (Berry).
    workspaces: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
enum Format {
    V1,
    Berry,
}

impl Format {
    /// The lockfile descriptor of the dependency `name` at `range`, as
    /// written in a manifest or a lockfile entry: Berry prefixes a range
    /// without a protocol with the default `npm:` one.
    fn descriptor(self, name: &str, range: &str) -> String {
        match self {
            Format::Berry if !range.contains(':') => format!("{name}@npm:{range}"),
            Format::V1 | Format::Berry => format!("{name}@{range}"),
        }
    }
}

/// The fields of a `package.json` that the lockfile does not record.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    name: Option<String>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    peer_dependencies: BTreeMap<String, String>,
    workspaces: Option<Workspaces>,
}

/// The `workspaces` field: a list of patterns, or an object with them.
#[derive(Deserialize)]
#[serde(untagged)]
enum Workspaces {
    Patterns(Vec<String>),
    Object {
        #[serde(default)]
        packages: Vec<String>,
    },
}

/// A node of the dependency graph: a root (the Project root or a Workspace
/// member), by index, or a lockfile entry, by index.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Node {
    Root(usize),
    Entry(usize),
}

/// Reads the `yarn.lock` at `source`, relative to `project`, and returns its
/// Packages. Its roots are the `package.json` next to it and those of its
/// Workspace members. A Package is `prod` when a root reaches it through
/// `dependencies`, `optionalDependencies` or `peerDependencies`, `dev` when
/// only through a `devDependencies` edge of a root, and `prod` when no root
/// reaches it. The lockfile records no licenses: the only License origin is
/// the installed copy, under the `node_modules` of the lockfile's directory
/// or of a Workspace member, when its version matches.
pub fn inventory(project: &Path, source: &str) -> Result<Vec<LicensedPackage>> {
    let path = project.join(source);
    let root = path.parent().unwrap_or(project);
    let text = fs::read_to_string(&path).map_err(|err| {
        anyhow!(
            "cannot read {}: {err}\nhint: check that it is readable",
            path.display()
        )
    })?;
    let lockfile = parse(&text).map_err(|err| {
        anyhow!(
            "{} is not a Yarn lockfile licguard can read: {err:#}\nhint: regenerate it with `yarn install`",
            path.display()
        )
    })?;

    let root_manifest = manifest(root)?;
    let member_dirs = match &lockfile.workspaces {
        Some(dirs) => dirs.clone(),
        None => workspace_dirs(root, root_manifest.workspaces.as_ref())?,
    };
    let root_name = root_manifest
        .name
        .clone()
        .or_else(|| npm::directory_name(root))
        .unwrap_or_default();
    let mut roots = vec![(root.to_path_buf(), root_name, root_manifest)];
    for dir in &member_dirs {
        let dir = root.join(dir);
        let manifest = manifest(&dir)?;
        let name = manifest
            .name
            .clone()
            .or_else(|| npm::directory_name(&dir))
            .unwrap_or_default();
        roots.push((dir, name, manifest));
    }

    let mut resolved: HashMap<&str, usize> = HashMap::new();
    for (index, entry) in lockfile.entries.iter().enumerate() {
        for descriptor in &entry.descriptors {
            resolved.insert(descriptor, index);
        }
    }
    let format = lockfile.format;
    let children = |node: &Node, prod_only: bool| -> Vec<(String, Node)> {
        let edges: Vec<(String, String)> = match node {
            Node::Root(index) => {
                let manifest = &roots[*index].2;
                let dev = (!prod_only).then_some(&manifest.dev_dependencies);
                [
                    Some(&manifest.dependencies),
                    dev,
                    Some(&manifest.optional_dependencies),
                    Some(&manifest.peer_dependencies),
                ]
                .into_iter()
                .flatten()
                .flatten()
                .map(|(name, range)| (name.clone(), format.descriptor(name, range)))
                .collect()
            }
            Node::Entry(index) => lockfile.entries[*index].dependencies.clone(),
        };
        edges
            .into_iter()
            .filter_map(|(name, descriptor)| {
                let index = resolved.get(descriptor.as_str())?;
                Some((name, Node::Entry(*index)))
            })
            .collect()
    };
    let root_nodes: Vec<(Node, String)> = roots
        .iter()
        .enumerate()
        .map(|(index, (_, name, _))| (Node::Root(index), name.clone()))
        .collect();
    let introduction_paths = paths::shortest(&root_nodes, children);

    let mut installed: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (dir, _, _) in &roots {
        index_installed(&dir.join("node_modules"), &mut installed);
    }

    let mut packages: BTreeMap<Package, LicensedPackage> = BTreeMap::new();
    for (index, entry) in lockfile.entries.iter().enumerate() {
        let package = Package {
            ecosystem: Ecosystem::Npm,
            name: entry.name.clone(),
            version: entry.version.clone(),
        };
        let declared_license = installed
            .get(&entry.name)
            .into_iter()
            .flatten()
            .find_map(|dir| npm::installed_license(dir, &entry.version));
        let (introduction_path, scope) = match introduction_paths.get(&Node::Entry(index)) {
            Some((path, scope)) => (Some(path.clone()), *scope),
            None => (None, Scope::Prod),
        };
        let found = LicensedPackage {
            license_origin: declared_license.as_ref().map(|_| LicenseOrigin::Installed),
            declared_license,
            scope,
            introduction_path,
            sources: vec![source.to_string()],
            package: package.clone(),
        };
        // Several entries can resolve to the same Package, e.g. a patched one.
        match packages.entry(package) {
            MapEntry::Vacant(vacant) => {
                vacant.insert(found);
            }
            MapEntry::Occupied(mut occupied) => occupied.get_mut().merge(found, true),
        }
    }
    Ok(packages.into_values().collect())
}

/// Parses a `yarn.lock`: Berry is YAML with a `__metadata` key, v1 has the
/// `# yarn lockfile v1` header.
fn parse(text: &str) -> Result<Lockfile> {
    if text.lines().any(|line| line.trim_end() == "__metadata:") {
        berry::parse(text)
    } else if text.lines().any(|line| line.trim() == "# yarn lockfile v1") {
        v1::parse(text)
    } else {
        bail!("it has neither the `# yarn lockfile v1` header nor a `__metadata` key")
    }
}

/// `@types/ms@npm:^0.7.34` -> `@types/ms`.
fn descriptor_name(descriptor: &str) -> &str {
    let at = descriptor
        .char_indices()
        .skip(1)
        .find(|(_, c)| *c == '@')
        .map_or(descriptor.len(), |(at, _)| at);
    &descriptor[..at]
}

fn manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join("package.json");
    let text = fs::read_to_string(&path).map_err(|err| {
        anyhow!(
            "cannot read {}: {err}\nhint: a yarn.lock is read with the package.json of its Project and of each Workspace member",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|err| {
        anyhow!(
            "{} is not a valid package.json: {err}\nhint: fix its syntax",
            path.display()
        )
    })
}

/// Expands the `workspaces` patterns of the root `package.json` into the
/// directories of the Workspace members, relative to `root`, sorted. Only
/// plain directories and `dir/*` patterns are supported.
fn workspace_dirs(root: &Path, workspaces: Option<&Workspaces>) -> Result<Vec<String>> {
    let patterns = match workspaces {
        None => return Ok(Vec::new()),
        Some(Workspaces::Patterns(patterns)) => patterns,
        Some(Workspaces::Object { packages }) => packages,
    };
    let mut dirs = Vec::new();
    for pattern in patterns {
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        let parent = if pattern == "*" {
            Some("")
        } else {
            pattern.strip_suffix("/*")
        };
        if parent
            .unwrap_or(pattern)
            .contains(['*', '?', '[', '{', '!'])
        {
            bail!(
                "workspace pattern `{pattern}` in {} is not supported\nhint: licguard expands only plain directories and `dir/*` patterns",
                root.join("package.json").display()
            );
        }
        match parent {
            Some(parent) => {
                let Ok(entries) = fs::read_dir(root.join(parent)) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let dir = if parent.is_empty() {
                        name
                    } else {
                        format!("{parent}/{name}")
                    };
                    if root.join(&dir).join("package.json").is_file() {
                        dirs.push(dir);
                    }
                }
            }
            None => {
                if root.join(pattern).join("package.json").is_file() {
                    dirs.push(pattern.to_string());
                }
            }
        }
    }
    dirs.sort();
    dirs.dedup();
    Ok(dirs)
}

/// Records the directory of every Package installed under `node_modules`,
/// nested copies included, by Package name. Symbolic links, such as the
/// links to Workspace members, are not followed.
fn index_installed(node_modules: &Path, installed: &mut HashMap<String, Vec<PathBuf>>) {
    for (name, dir) in children_dirs(node_modules) {
        if name.starts_with('.') {
            continue;
        }
        let packages = if name.starts_with('@') {
            children_dirs(&dir)
                .into_iter()
                .map(|(scoped, dir)| (format!("{name}/{scoped}"), dir))
                .collect()
        } else {
            vec![(name, dir)]
        };
        for (name, dir) in packages {
            let nested = dir.join("node_modules");
            installed.entry(name).or_default().push(dir);
            index_installed(&nested, installed);
        }
    }
}

/// The subdirectories of `dir`, sorted by name; none if it cannot be read.
fn children_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect();
    dirs.sort();
    dirs
}
