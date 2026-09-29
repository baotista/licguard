use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;
use serde::de::IgnoredAny;

use super::{Entry, Format, Lockfile, descriptor_name};

#[derive(Deserialize)]
struct BerryLockfile {
    #[serde(rename = "__metadata")]
    _metadata: IgnoredAny,
    /// Keyed by comma-separated descriptors, e.g. `wrappy@npm:1, wrappy@npm:^1.0.2`.
    #[serde(flatten)]
    entries: BTreeMap<String, BerryEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BerryEntry {
    version: String,
    /// The locator, e.g. `ms@npm:2.1.3` or `ui@workspace:packages/ui`.
    resolution: String,
    /// Includes the optional dependencies.
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    peer_dependencies: BTreeMap<String, String>,
    /// `soft` for workspaces, `link:` and `portal:` entries, which are no
    /// Packages.
    link_type: Option<String>,
}

/// Parses a Yarn Berry (v2 and later) lockfile, a YAML document. Its
/// `workspace:` entries give the Workspace members.
pub(super) fn parse(text: &str) -> Result<Lockfile> {
    let lockfile: BerryLockfile = serde_yaml_ng::from_str(text)?;
    let mut entries = Vec::new();
    let mut workspaces = Vec::new();
    for (key, entry) in lockfile.entries {
        let name = descriptor_name(&entry.resolution);
        let protocol = entry.resolution.get(name.len() + 1..).unwrap_or_default();
        if let Some(dir) = protocol.strip_prefix("workspace:") {
            if dir != "." {
                workspaces.push(dir.to_string());
            }
            continue;
        }
        if entry.link_type.as_deref() == Some("soft") {
            continue;
        }
        let dependencies = entry
            .dependencies
            .iter()
            .chain(&entry.peer_dependencies)
            .map(|(name, range)| (name.clone(), Format::Berry.descriptor(name, range)))
            .collect();
        entries.push(Entry {
            descriptors: key.split(", ").map(str::to_string).collect(),
            name: name.to_string(),
            version: entry.version,
            dependencies,
        });
    }
    workspaces.sort();
    Ok(Lockfile {
        format: Format::Berry,
        entries,
        workspaces: Some(workspaces),
    })
}
