use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

const CONFIG: &str = "licguard.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    // Declared most severe first, so sorting puts Violations on top.
    Deny,
    Review,
    Allow,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    policy: Policy,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

impl Policy {
    pub fn load(root: &Path) -> Result<Policy> {
        let path = root.join(CONFIG);
        let text = fs::read_to_string(&path).map_err(|err| {
            anyhow!(
                "cannot read {}: {err}\nhint: create a {CONFIG} with a [policy] section",
                path.display()
            )
        })?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("{} is invalid", path.display()))?;
        let policy = config.policy;
        for id in policy.allow.iter().chain(&policy.deny) {
            if spdx::license_id(id).is_none() {
                bail!(
                    "{}: `{id}` is not an SPDX license identifier\nhint: see https://spdx.org/licenses/",
                    path.display()
                );
            }
        }
        Ok(policy)
    }

    /// Verdict for a Normalized license; `None` means the license is Unresolved.
    ///
    /// Until the Policy exposes `unresolved` and `unlisted` settings, their
    /// defaults apply: Unresolved licenses are denied, Unlisted ones reviewed.
    pub fn evaluate(&self, license: Option<&str>) -> Verdict {
        match license {
            None => Verdict::Deny,
            Some(id) if self.deny.iter().any(|d| d == id) => Verdict::Deny,
            Some(id) if self.allow.iter().any(|a| a == id) => Verdict::Allow,
            Some(_) => Verdict::Review,
        }
    }
}

/// Normalizes a Declared license. Only plain SPDX license identifiers are
/// understood for now; anything else is Unresolved.
pub fn normalize(declared: &str) -> Option<String> {
    spdx::license_id(declared.trim()).map(|id| id.name.to_string())
}
