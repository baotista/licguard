use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use spdx::expression::{ExprNode, Operator};
use spdx::{LicenseItem, LicenseReq};

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

/// The result of evaluating one Package's license against the Policy.
pub struct Outcome {
    pub verdict: Verdict,
    pub reason: Reason,
    /// The license the Verdict is based on, with each `OR` replaced by its
    /// Elected license; `None` when the license has no `OR`.
    pub elected: Option<String>,
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
        let lists = [
            ("allow", &policy.allow),
            ("review", &policy.review),
            ("deny", &policy.deny),
        ];
        let mut seen: HashMap<&str, &str> = HashMap::new();
        for (list, entries) in lists {
            for id in entries {
                if !is_policy_entry(id) {
                    bail!(
                        "{}: `{id}` is not an SPDX license identifier, optionally followed by `WITH <exception>`\nhint: see https://spdx.org/licenses/",
                        path.display()
                    );
                }
                if let Some(first) = seen.insert(id, list).filter(|first| *first != list) {
                    bail!(
                        "{}: `{id}` is in both `{first}` and `{list}`\nhint: keep it in exactly one list",
                        path.display()
                    );
                }
            }
        }
        Ok(policy)
    }

    /// Evaluates a Normalized license; `None` means the license is Unresolved.
    pub fn evaluate(&self, license: Option<&str>) -> Outcome {
        let Some(license) = license else {
            return Outcome {
                verdict: self.unresolved,
                reason: Reason::Unresolved,
                elected: None,
            };
        };
        let expression = parse(license).expect("a Normalized license is a valid expression");
        // The expression comes in postfix order: evaluate it with a stack of
        // (verdict, reason, license with each `OR` replaced by its elected option).
        let mut stack: Vec<(Verdict, Reason, String)> = Vec::new();
        let mut has_or = false;
        for node in expression.iter() {
            match node {
                ExprNode::Req(req) => {
                    let (verdict, reason) = self.evaluate_term(&req.req);
                    stack.push((verdict, reason, req.req.to_string()));
                }
                ExprNode::Op(Operator::Or) => {
                    has_or = true;
                    let right = stack.pop().unwrap();
                    let left = stack.pop().unwrap();
                    // Most favorable option wins; the first one on ties.
                    stack.push(if right.0 > left.0 { right } else { left });
                }
                ExprNode::Op(Operator::And) => {
                    let right = stack.pop().unwrap();
                    let left = stack.pop().unwrap();
                    // Most severe term wins; the first one on ties.
                    let (verdict, reason) = if right.0 < left.0 {
                        (right.0, right.1)
                    } else {
                        (left.0, left.1)
                    };
                    stack.push((verdict, reason, format!("{} AND {}", left.2, right.2)));
                }
            }
        }
        let (verdict, reason, elected) = stack.pop().unwrap();
        Outcome {
            verdict,
            reason,
            elected: has_or.then_some(elected),
        }
    }

    /// Evaluates a single license term. A term that is not listed falls back
    /// to its base license: without its SPDX exception, then without
    /// `-or-later`. Both only add permissions, so the base license's Verdict
    /// is a safe bound.
    fn evaluate_term(&self, term: &LicenseReq) -> (Verdict, Reason) {
        let license = term.to_string();
        let listed = |list: &[String]| list.contains(&license);
        if listed(&self.deny) {
            (Verdict::Deny, Reason::Listed)
        } else if listed(&self.review) {
            (Verdict::Review, Reason::Listed)
        } else if listed(&self.allow) {
            (Verdict::Allow, Reason::Listed)
        } else if term.addition.is_some() {
            self.evaluate_term(&LicenseReq {
                license: term.license.clone(),
                addition: None,
            })
        } else if let Some(base) = base_version(&term.license) {
            self.evaluate_term(&LicenseReq {
                license: base,
                addition: None,
            })
        } else {
            (self.unlisted, Reason::Unlisted)
        }
    }
}

/// Normalizes a Declared license into an SPDX expression; `None` when it is
/// not a valid one.
pub fn normalize(declared: &str) -> Option<String> {
    let declared = declared.trim();
    parse(declared).ok().map(|_| declared.to_string())
}

/// Strict SPDX parsing, except that deprecated identifiers (e.g. `GPL-3.0`)
/// are accepted as they are; mapping them to current ones comes with alias
/// normalization.
fn parse(expression: &str) -> Result<spdx::Expression, spdx::ParseError> {
    spdx::Expression::parse_mode(
        expression,
        spdx::ParseMode {
            allow_deprecated: true,
            ..spdx::ParseMode::STRICT
        },
    )
}

/// `GPL-2.0-or-later` -> `GPL-2.0-only`; `Apache-2.0+` -> `Apache-2.0`.
fn base_version(license: &LicenseItem) -> Option<LicenseItem> {
    let LicenseItem::Spdx { id, or_later } = license else {
        return None;
    };
    if *or_later {
        return Some(LicenseItem::Spdx {
            id: *id,
            or_later: false,
        });
    }
    let base = id.name.strip_suffix("-or-later")?;
    spdx::gnu_license_id(base, false).map(|id| LicenseItem::Spdx {
        id,
        or_later: false,
    })
}

/// A policy entry is a single license term (`MIT`, `Apache-2.0+`,
/// `GPL-2.0-only WITH Classpath-exception-2.0`) written in canonical form.
fn is_policy_entry(entry: &str) -> bool {
    let Ok(expression) = parse(entry) else {
        return false;
    };
    let mut nodes = expression.iter();
    matches!(
        (nodes.next(), nodes.next()),
        (Some(ExprNode::Req(req)), None) if req.req.to_string() == entry
    )
}
