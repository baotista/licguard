//! `licguard init`: writes the neutral template Policy (ADR-0003).

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{Result, anyhow};

use crate::policy::CONFIG;

/// The neutral template `licguard.toml`.
const TEMPLATE: &str = include_str!("template.toml");

/// Writes the template Policy to the Project's `licguard.toml` and returns
/// the message to show the user. An existing file is replaced only when
/// `force` is set.
pub fn init(root: &Path, force: bool) -> Result<String> {
    let path = root.join(CONFIG);
    OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(force)
        .create_new(!force)
        .open(&path)
        .and_then(|mut file| file.write_all(TEMPLATE.as_bytes()))
        .map_err(|err| match err.kind() {
            ErrorKind::AlreadyExists => anyhow!(
                "{} already exists\nhint: pass `--force` to overwrite it with the template",
                path.display()
            ),
            _ => anyhow!(
                "cannot write {}: {err}\nhint: check that the directory exists and is writable",
                path.display()
            ),
        })?;
    Ok(format!(
        "wrote {}\nnext: review the Policy, then run `licguard check`\nnote: the template is not legal advice; have the Policy validated by your own legal counsel\n",
        path.display()
    ))
}
