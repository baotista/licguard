use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

use super::Scope;

/// Computes one Introduction path per node of a dependency graph reachable
/// from `roots`, given as `(key, name)` pairs, with the Scope it proves. Each
/// path starts with the name of a root and is the shortest one, the first in
/// root order then sorted dependency-name order on ties. A node reachable
/// through `prod` edges only gets the shortest such path and is `Prod`, so the
/// path shows why the Package ships; any other reachable node is `Dev`.
///
/// `children(key, prod_only)` gives the dependencies of the node at `key` as
/// `(name, child key)` pairs; with `prod_only`, it leaves out the `dev` ones.
pub(super) fn shortest<K, F>(roots: &[(K, String)], children: F) -> HashMap<K, (Vec<String>, Scope)>
where
    K: Clone + Eq + Hash,
    F: Fn(&K, bool) -> Vec<(String, K)>,
{
    let mut paths: HashMap<K, (Vec<String>, Scope)> = walk(roots, |key| children(key, true))
        .into_iter()
        .map(|(key, path)| (key, (path, Scope::Prod)))
        .collect();
    for (key, path) in walk(roots, |key| children(key, false)) {
        paths.entry(key).or_insert((path, Scope::Dev));
    }
    paths
}

/// Breadth-first walk from all the roots at once, visiting each node's
/// children in sorted name order. The roots get no path.
fn walk<K, F>(roots: &[(K, String)], children: F) -> HashMap<K, Vec<String>>
where
    K: Clone + Eq + Hash,
    F: Fn(&K) -> Vec<(String, K)>,
{
    let mut paths: HashMap<K, Vec<String>> = roots
        .iter()
        .map(|(key, name)| (key.clone(), vec![name.clone()]))
        .collect();
    let mut queue: VecDeque<K> = roots.iter().map(|(key, _)| key.clone()).collect();
    while let Some(key) = queue.pop_front() {
        let mut next = children(&key);
        next.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (name, child) in next {
            if paths.contains_key(&child) {
                continue;
            }
            let mut path = paths[&key].clone();
            path.push(name);
            paths.insert(child.clone(), path);
            queue.push_back(child);
        }
    }
    for (key, _) in roots {
        paths.remove(key);
    }
    paths
}
