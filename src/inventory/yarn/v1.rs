use anyhow::{Result, anyhow, bail};

use super::{Entry, Format, Lockfile, descriptor_name};

/// Parses a Yarn v1 lockfile. Each entry starts with an unindented line of
/// comma-separated descriptors ending with `:`, e.g. `wrappy@1, wrappy@^1.0.2:`,
/// followed by fields indented by two spaces (`version "1.0.2"`) and
/// dependency sections (`dependencies:`) whose `name "range"` lines are
/// indented by four. Descriptors, names and values may be quoted.
pub(super) fn parse(text: &str) -> Result<Lockfile> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut section: Option<&str> = None;
    for (number, line) in text.lines().enumerate() {
        let error = |message: &str| anyhow!("line {}: {message}", number + 1);
        let content = line.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let entry = entries.last_mut();
        match (line.len() - content.len(), entry) {
            (0, _) => {
                let descriptors = content
                    .strip_suffix(':')
                    .ok_or_else(|| error("expected descriptors ending with `:`"))?;
                let descriptors: Vec<String> = descriptors
                    .split(", ")
                    .map(|descriptor| unquote(descriptor.trim()).to_string())
                    .collect();
                entries.push(Entry {
                    name: descriptor_name(&descriptors[0]).to_string(),
                    descriptors,
                    version: String::new(),
                    dependencies: Vec::new(),
                    line: Some(number + 1),
                });
                section = None;
            }
            (2, Some(entry)) => {
                if let Some(name) = content.strip_suffix(':') {
                    section = Some(name);
                    continue;
                }
                section = None;
                let (field, value) =
                    pair(content).ok_or_else(|| error("expected `field value`"))?;
                if field == "version" {
                    entry.version = value.to_string();
                }
            }
            (4, Some(entry)) => {
                let (name, range) = pair(content).ok_or_else(|| error("expected `name range`"))?;
                if matches!(
                    section,
                    Some("dependencies" | "optionalDependencies" | "peerDependencies")
                ) {
                    let descriptor = Format::V1.descriptor(name, range);
                    entry.dependencies.push((name.to_string(), descriptor));
                }
            }
            _ => bail!(error("unexpected indentation")),
        }
    }
    if let Some(entry) = entries.iter().find(|entry| entry.version.is_empty()) {
        bail!("entry `{}` has no version", entry.descriptors.join(", "));
    }
    Ok(Lockfile {
        format: Format::V1,
        entries,
        workspaces: None,
    })
}

/// Splits `key value` at the first space outside quotes, and unquotes both.
fn pair(content: &str) -> Option<(&str, &str)> {
    let split = if let Some(quoted) = content.strip_prefix('"') {
        quoted.find('"')? + 2
    } else {
        content.find(' ')?
    };
    let (key, value) = content.split_at(split);
    Some((unquote(key), unquote(value.trim())))
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
}
