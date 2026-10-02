use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
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

/// The most Introduction paths [`Graph::paths_to`] collects for a node
/// before it stops: past this many, it only tells that there are more.
pub const MAX_PATHS: usize = 1000;

/// A dependency graph reachable from its roots, `dev` edges included, kept to
/// find every Introduction path of a node.
pub(super) struct Graph<K> {
    /// The name of each root.
    roots: HashMap<K, String>,
    /// Each node's dependents, as `(dependent key, name of the node in the
    /// dependent's dependencies)` pairs, in walk order.
    parents: HashMap<K, Vec<(K, String)>>,
}

impl<K: Clone + Eq + Hash> Graph<K> {
    /// Walks the graph from `roots`, as for [`shortest`].
    pub(super) fn new<F>(roots: &[(K, String)], children: F) -> Self
    where
        F: Fn(&K, bool) -> Vec<(String, K)>,
    {
        let mut queue: VecDeque<K> = roots.iter().map(|(key, _)| key.clone()).collect();
        let roots: HashMap<K, String> = roots.iter().cloned().collect();
        let mut parents: HashMap<K, Vec<(K, String)>> = HashMap::new();
        let mut seen: HashSet<K> = roots.keys().cloned().collect();
        while let Some(key) = queue.pop_front() {
            let mut next = children(&key, false);
            next.sort_by(|(a, _), (b, _)| a.cmp(b));
            for (name, child) in next {
                // As for `shortest`, no path goes through a root.
                if roots.contains_key(&child) {
                    continue;
                }
                parents
                    .entry(child.clone())
                    .or_default()
                    .push((key.clone(), name));
                if seen.insert(child.clone()) {
                    queue.push_back(child);
                }
            }
        }
        Graph { roots, parents }
    }

    /// Every Introduction path of the node at `target`, each through
    /// distinct nodes, without duplicates and sorted; it stops past
    /// [`MAX_PATHS`] of them.
    pub(super) fn paths_to(&self, target: &K) -> Vec<Vec<String>> {
        let mut found = BTreeSet::new();
        self.collect(target, &mut Vec::new(), &mut HashSet::new(), &mut found);
        found.into_iter().collect()
    }

    /// Adds to `found` every path from a root to `node` followed by
    /// `suffix`, the names from `node` to the target in reverse order, that
    /// goes through none of the nodes `on_path`.
    fn collect(
        &self,
        node: &K,
        suffix: &mut Vec<String>,
        on_path: &mut HashSet<K>,
        found: &mut BTreeSet<Vec<String>>,
    ) {
        if found.len() > MAX_PATHS {
            return;
        }
        if let Some(root) = self.roots.get(node) {
            let path = std::iter::once(root)
                .chain(suffix.iter().rev())
                .cloned()
                .collect();
            found.insert(path);
            return;
        }
        on_path.insert(node.clone());
        for (parent, name) in self.parents.get(node).into_iter().flatten() {
            if !on_path.contains(parent) {
                suffix.push(name.clone());
                self.collect(parent, suffix, on_path, found);
                suffix.pop();
            }
        }
        on_path.remove(node);
    }
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
