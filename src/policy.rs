use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

const CONFIG: &str = "licguard.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    // Declared most severe first, so sorting puts Violations on top.
    Deny,
    Review,
    Allow,
}

/// Why a Package received its Verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Listed,
    Unresolved,
    Unlisted,
}

impl Verdict {
    /// Whether this Verdict fails the gate: `deny`, or `review` in strict mode.
    pub fn is_violation(self, strict: bool) -> bool {
        match self {
            Verdict::Deny => true,
            Verdict::Review => strict,
            Verdict::Allow => false,
        }
    }
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
    review: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
    #[serde(default = "default_unresolved")]
    unresolved: Verdict,
    #[serde(default = "default_unlisted")]
    unlisted: Verdict,
}

fn default_unresolved() -> Verdict {
    Verdict::Deny
}

fn default_unlisted() -> Verdict {
    Verdict::Review
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
        for id in policy
            .allow
            .iter()
            .chain(&policy.review)
            .chain(&policy.deny)
        {
            if spdx_id(id).is_none() {
                bail!(
                    "{}: `{id}` is not an SPDX license identifier\nhint: see https://spdx.org/licenses/",
                    path.display()
                );
            }
        }
        Ok(policy)
    }

    /// Verdict for a Normalized license; `None` means the license is Unresolved.
    pub fn evaluate(&self, license: Option<&str>) -> (Verdict, Reason) {
        let listed = |list: &[String], id| list.iter().any(|l| l == id);
        match license {
            None => (self.unresolved, Reason::Unresolved),
            Some(id) if listed(&self.deny, id) => (Verdict::Deny, Reason::Listed),
            Some(id) if listed(&self.review, id) => (Verdict::Review, Reason::Listed),
            Some(id) if listed(&self.allow, id) => (Verdict::Allow, Reason::Listed),
            Some(_) => (self.unlisted, Reason::Unlisted),
        }
    }
}

/// Normalizes a Declared license. Only plain SPDX license identifiers are
/// understood for now; anything else is Unresolved.
pub fn normalize(declared: &str) -> Option<String> {
    spdx_id(declared.trim()).map(str::to_string)
}

/// The canonical SPDX license identifier equal to `id`, if any.
/// `spdx::license_id` silently drops a trailing `+`, which would turn
/// `GPL-2.0+` into `GPL-2.0`; an exact match is required instead.
fn spdx_id(id: &str) -> Option<&'static str> {
    spdx::license_id(id)
        .map(|license| license.name)
        .filter(|name| *name == id)
}
