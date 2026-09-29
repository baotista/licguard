//! Turns a Declared license into a Normalized license.

use spdx::expression::{ExprNode, Operator};
use spdx::{LicenseItem, LicenseReq};

/// Names that unambiguously mean one SPDX license, matched case-insensitively
/// against a whole license term. Names without a version (`BSD`, `GPL`) are
/// deliberately absent: guessing one would be a compliance risk.
const ALIASES: &[(&str, &str)] = &[
    ("Apache 2", "Apache-2.0"),
    ("Apache 2.0", "Apache-2.0"),
    ("Apache-2", "Apache-2.0"),
    ("Apache2", "Apache-2.0"),
    ("Apache License, Version 2.0", "Apache-2.0"),
];

/// Normalizes a Declared license into a canonical SPDX expression; `None`
/// when its intent is not unambiguous, i.e. the Package is Unresolved.
///
/// Beyond strict SPDX, it accepts `/` as `OR`, lower-case operators, `+` on
/// GNU licenses, deprecated identifiers and the [`ALIASES`]. `NOASSERTION`
/// anywhere means the license is unknown.
pub fn normalize(declared: &str) -> Option<String> {
    let mode = spdx::ParseMode {
        allow_imprecise_license_names: false,
        ..spdx::ParseMode::LAX
    };
    let expression = spdx::Expression::parse_mode(&replace_aliases(declared), mode).ok()?;
    (!is_unknown(&expression)).then(|| render(&expression))
}

/// Whether the expression contains `NOASSERTION`, i.e. the license is unknown.
pub fn is_unknown(expression: &spdx::Expression) -> bool {
    expression.requirements().any(|r| {
        r.req
            .license
            .id()
            .is_some_and(|id| id.name == "NOASSERTION")
    })
}

/// Replaces each license term that is an alias, or an SPDX identifier or
/// full name in another case (`mit`, `MIT License`), by its SPDX identifier.
/// A term is the text between parentheses, `/` and operators, so it may
/// contain spaces (`Apache License, Version 2.0 OR MIT`).
fn replace_aliases(declared: &str) -> String {
    let spaced = declared
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " / ");
    let mut out: Vec<String> = Vec::new();
    let mut term: Vec<&str> = Vec::new();
    for word in spaced.split_whitespace() {
        let separator = ["(", ")", "/", "AND", "OR", "WITH"]
            .iter()
            .any(|s| s.eq_ignore_ascii_case(word));
        if separator {
            out.extend(take_term(&mut term));
            out.push(word.to_string());
        } else {
            term.push(word);
        }
    }
    out.extend(take_term(&mut term));
    out.join(" ")
}

/// Empties `words` and returns the term they form, as an SPDX identifier
/// when it is a known name for one.
fn take_term(words: &mut Vec<&str>) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    let text = words.join(" ");
    words.clear();
    if let Some((_, id)) = ALIASES.iter().find(|(a, _)| a.eq_ignore_ascii_case(&text)) {
        return Some(id.to_string());
    }
    let (name, plus) = match text.strip_suffix('+') {
        Some(name) => (name, "+"),
        None => (text.as_str(), ""),
    };
    let license = spdx::identifiers::LICENSES
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(name) || l.full_name.eq_ignore_ascii_case(name));
    Some(license.map_or(text.clone(), |l| format!("{}{plus}", l.name)))
}

/// Renders an expression with canonical operators and parentheses only
/// where precedence needs them, and deprecated GNU identifiers mapped to
/// their current ones.
pub fn render(expression: &spdx::Expression) -> String {
    // The expression comes in postfix order: (text, whether its top-level
    // operator is `OR`).
    let mut stack: Vec<(String, bool)> = Vec::new();
    for node in expression.iter() {
        match node {
            ExprNode::Req(req) => stack.push((current_gnu(&req.req).to_string(), false)),
            ExprNode::Op(op) => {
                let (right, right_or) = stack.pop().unwrap();
                let (left, left_or) = stack.pop().unwrap();
                stack.push(match op {
                    Operator::Or => (format!("{left} OR {right}"), true),
                    Operator::And => {
                        let group = |text: String, or: bool| {
                            if or { format!("({text})") } else { text }
                        };
                        let text =
                            format!("{} AND {}", group(left, left_or), group(right, right_or));
                        (text, false)
                    }
                });
            }
        }
    }
    stack.pop().unwrap().0
}

/// Maps a deprecated GNU identifier to its current one: `GPL-2.0` ->
/// `GPL-2.0-only`. Other identifiers, deprecated or not, are kept.
pub fn current_gnu(term: &LicenseReq) -> LicenseReq {
    let mut term = term.clone();
    if let LicenseItem::Spdx { id, or_later } = &mut term.license
        && id.is_gnu()
        && id.is_deprecated()
        && let Some(current) = spdx::gnu_license_id(id.name, *or_later)
    {
        *id = current;
        *or_later = false;
    }
    term
}
