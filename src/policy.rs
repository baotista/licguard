use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use spdx::expression::{ExprNode, Operator};
use spdx::{LicenseItem, LicenseReq};

use crate::clarification::Clarification;
use crate::normalize;

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

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Listed => "listed",
            Reason::Unresolved => "unresolved",
            Reason::Unlisted => "unlisted",
        }
    }
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Deny => "deny",
            Verdict::Review => "review",
            Verdict::Allow => "allow",
        }
    }

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
    #[serde(default)]
    clarifications: Vec<Clarification>,
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
    /// Whether `dev` Dependencies are evaluated too.
    #[serde(default)]
    pub include_dev: bool,
    /// The `[[clarifications]]` entries, which sit beside `[policy]`.
    #[serde(skip)]
    pub clarifications: Vec<Clarification>,
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
        let mut policy = config.policy;
        policy.clarifications = config.clarifications;
        let mut seen: HashMap<String, &str> = HashMap::new();
        for (list, entries) in [
            ("allow", &mut policy.allow),
            ("review", &mut policy.review),
            ("deny", &mut policy.deny),
        ] {
            for entry in entries.iter_mut() {
                let Some(id) = policy_entry(entry) else {
                    bail!(
                        "{}: `{entry}` is not an SPDX license identifier, optionally followed by `WITH <exception>`\nhint: see https://spdx.org/licenses/",
                        path.display()
                    );
                };
                if let Some(first) = seen.insert(id.clone(), list).filter(|first| *first != list) {
                    bail!(
                        "{}: `{id}` is in both `{first}` and `{list}`\nhint: keep it in exactly one list",
                        path.display()
                    );
                }
                *entry = id;
            }
        }
        let mut clarified: HashMap<(String, Option<String>), usize> = HashMap::new();
        for (i, clarification) in policy.clarifications.iter_mut().enumerate() {
            let number = i + 1;
            let package = match clarification.package.trim() {
                "" => String::new(),
                name => format!(" (`{name}`)"),
            };
            if let Err(err) = clarification.validate() {
                bail!(
                    "{}: clarification #{number}{package}: {err}",
                    path.display()
                );
            }
            let key = (clarification.package.clone(), clarification.version.clone());
            if let Some(first) = clarified.insert(key, number) {
                bail!(
                    "{}: clarification #{number}{package}: same package and version as clarification #{first}\nhint: keep exactly one of them",
                    path.display()
                );
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

/// Strict SPDX parsing, except that deprecated identifiers (e.g. `eCos-2.0`)
/// are accepted: Normalized licenses keep those that have no current
/// equivalent.
pub fn parse(expression: &str) -> Result<spdx::Expression, spdx::ParseError> {
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
/// Returns it as it is matched against Normalized licenses: with a deprecated
/// GNU identifier mapped to its current one (`GPL-3.0` -> `GPL-3.0-only`).
fn policy_entry(entry: &str) -> Option<String> {
    let expression = parse(entry).ok()?;
    let mut nodes = expression.iter();
    match (nodes.next(), nodes.next()) {
        (Some(ExprNode::Req(req)), None) if req.req.to_string() == entry => {
            Some(normalize::current_gnu(&req.req).to_string())
        }
        _ => None,
    }
}
