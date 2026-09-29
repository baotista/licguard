use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use super::LockEntry;

/// Computes one Introduction path per lockfile entry reachable from the root
/// entry `""`, keyed by lockfile key. Each path starts with `root_name` and is
/// the shortest one, the first in sorted dependency-name order on ties. A
/// `prod` entry gets the shortest path that uses neither `devDependencies`
/// nor `dev` entries, so it shows why the Package ships. `link` entries are
/// not followed.
pub(super) fn shortest(
    packages: &BTreeMap<String, LockEntry>,
    root_name: &str,
) -> HashMap<String, Vec<String>> {
    let mut paths = walk(packages, root_name, true);
    for (key, path) in walk(packages, root_name, false) {
        paths.entry(key).or_insert(path);
    }
    paths
}

/// Breadth-first walk from the root, visiting dependency names in sorted
/// order; `prod_only` skips `devDependencies` and `dev` entries.
fn walk(
    packages: &BTreeMap<String, LockEntry>,
    root_name: &str,
    prod_only: bool,
) -> HashMap<String, Vec<String>> {
    let mut paths = HashMap::from([(String::new(), vec![root_name.to_string()])]);
    let mut queue = VecDeque::from([String::new()]);
    while let Some(key) = queue.pop_front() {
        let Some(entry) = packages.get(&key) else {
            continue;
        };
        let dev_dependencies = (!prod_only).then_some(&entry.dev_dependencies);
        let names: BTreeSet<&String> = [
            Some(&entry.dependencies),
            dev_dependencies,
            Some(&entry.optional_dependencies),
            Some(&entry.peer_dependencies),
        ]
        .into_iter()
        .flatten()
        .flat_map(|deps| deps.keys())
        .collect();
        for name in names {
            let Some(child) = resolve(packages, &key, name) else {
                continue;
            };
            let child_entry = &packages[&child];
            if paths.contains_key(&child) || child_entry.link || (prod_only && child_entry.dev) {
                continue;
            }
            let mut path = paths[&key].clone();
            path.push(name.clone());
            paths.insert(child.clone(), path);
            queue.push_back(child);
        }
    }
    paths.remove("");
    paths
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
