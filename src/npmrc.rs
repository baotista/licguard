//! The npm configuration that names the registries to query: environment
//! variables and `.npmrc` files, read the way npm reads them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use anyhow::{Result, bail};

use crate::inventory::Package;

/// The environment variable that names the registry of every Package,
/// whatever the npm configuration says: for tests and mirrors.
const OVERRIDE: &str = "LICGUARD_NPM_REGISTRY";

/// The registry queried when nothing names another.
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// The registries to query.
pub struct Registries {
    /// The registry of the Packages without a scoped registry, without a
    /// trailing slash.
    default: String,
    /// The registry of the Packages of each scope (`@scope`), without a
    /// trailing slash.
    scopes: BTreeMap<String, String>,
    credentials: Credentials,
}

impl Registries {
    /// Reads the configuration of the Project at `project`.
    pub fn load(project: &Path) -> Result<Registries> {
        // The Project's settings win over the user's.
        let mut settings = Settings::default();
        if let Some(file) = user_npmrc() {
            settings.read(&file);
        }
        settings.read(&project.join(".npmrc"));
        let from_env = [OVERRIDE, "npm_config_registry", "NPM_CONFIG_REGISTRY"]
            .into_iter()
            .find_map(|key| env::var(key).ok().map(|url| (key, url)));
        let default = match (from_env, settings.get("registry")) {
            (Some((key, url)), _) => registry_url(key, &url)?,
            (None, Some((url, file))) => {
                registry_url(&format!("`registry` in {}", file.display()), url)?
            }
            (None, None) => DEFAULT_REGISTRY.to_string(),
        };
        let mut scopes = BTreeMap::new();
        if env::var_os(OVERRIDE).is_none() {
            for (key, (url, file)) in &settings.0 {
                if let Some(scope) = key
                    .strip_suffix(":registry")
                    .filter(|scope| scope.starts_with('@'))
                {
                    let source = format!("`{key}` in {}", file.display());
                    scopes.insert(scope.to_string(), registry_url(&source, url)?);
                }
            }
        }
        Ok(Registries {
            default,
            scopes,
            credentials: Credentials::from(&settings),
        })
    }

    /// The registry of `package`, without a trailing slash: its scope's,
    /// if one is configured, else the default one.
    pub fn of(&self, package: &Package) -> &str {
        package
            .name
            .split_once('/')
            .and_then(|(scope, _)| self.scopes.get(scope))
            .unwrap_or(&self.default)
    }

    /// The credentials of the registries.
    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }
}

/// The `Authorization` header of the requests to each registry that
/// `.npmrc` has a credential for. It is secret: it is never printed nor
/// cached, so the type implements neither `Debug` nor `Display`.
pub struct Credentials {
    /// The header by the registry URL it applies to, without its scheme and
    /// with a trailing slash, as `.npmrc` keys name it: its "nerf dart",
    /// e.g. `//registry.example.com/npm/`.
    headers: BTreeMap<String, String>,
}

impl Credentials {
    /// Reads the `//<registry>/:_authToken` and `//<registry>/:_auth`
    /// settings; a token wins over a `_auth` for the same registry. Warns
    /// about a `username` and `_password` pair, which is not supported.
    fn from(settings: &Settings) -> Credentials {
        let mut found: BTreeMap<String, Found> = BTreeMap::new();
        for (key, (value, file)) in &settings.0 {
            let Some((nerf_dart, field)) = key
                .rsplit_once(':')
                .filter(|(key, _)| key.starts_with("//"))
            else {
                continue;
            };
            // `//host/path:_authToken` is `//host/path/:_authToken`.
            let nerf_dart = format!("{}/", nerf_dart.trim_end_matches('/'));
            let found = found.entry(nerf_dart).or_default();
            let value = Some(value.as_str()).filter(|value| !value.is_empty());
            match field {
                "_authToken" => found.token = value,
                "_auth" => found.auth = value,
                "username" | "_password" => found.password = Some(file),
                _ => {}
            }
        }
        let mut headers = BTreeMap::new();
        for (nerf_dart, found) in found {
            let header = match (found.token, found.auth, found.password) {
                (Some(token), _, _) => format!("Bearer {token}"),
                (None, Some(auth), _) => format!("Basic {auth}"),
                (None, None, Some(file)) => {
                    eprintln!(
                        "warning: {}: `{nerf_dart}:username` and `_password` are not supported: the requests to that registry carry no credential\nhint: set `{nerf_dart}:_authToken` instead",
                        file.display()
                    );
                    continue;
                }
                (None, None, None) => continue,
            };
            headers.insert(nerf_dart, header);
        }
        Credentials { headers }
    }

    /// The `Authorization` header of a request to `url`: that of the
    /// longest registry URL `url` starts with, schemes aside, if any.
    pub fn authorization(&self, url: &str) -> Option<&str> {
        let (_, rest) = url.split_once(':')?;
        self.headers
            .iter()
            .filter(|(nerf_dart, _)| rest.starts_with(nerf_dart.as_str()))
            .max_by_key(|(nerf_dart, _)| nerf_dart.len())
            .map(|(_, header)| header.as_str())
    }
}

/// The credential settings of a registry in the `.npmrc` files.
#[derive(Default)]
struct Found<'a> {
    token: Option<&'a str>,
    auth: Option<&'a str>,
    /// The file of its `username` or `_password`, if any.
    password: Option<&'a Path>,
}

/// The user's `.npmrc`: the file `NPM_CONFIG_USERCONFIG` names, else
/// `.npmrc` in the home directory, `HOME` or else `USERPROFILE`.
fn user_npmrc() -> Option<PathBuf> {
    if let Some(file) = env::var_os("NPM_CONFIG_USERCONFIG").filter(|f| !f.is_empty()) {
        return Some(PathBuf::from(file));
    }
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|key| env::var_os(key).filter(|dir| !dir.is_empty()))
        .map(|home| Path::new(&home).join(".npmrc"))
}

/// The settings of the `.npmrc` files read so far, each with the file that
/// set it last.
#[derive(Default)]
struct Settings(BTreeMap<String, (String, PathBuf)>);

impl Settings {
    /// Reads the `key=value` settings of the `.npmrc` at `file`, if it
    /// exists, over the earlier ones.
    fn read(&mut self, file: &Path) {
        let text = match fs::read_to_string(file) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return,
            Err(err) => {
                eprintln!(
                    "warning: cannot read {}: {err}; it is ignored",
                    file.display()
                );
                return;
            }
        };
        for line in text.lines().map(str::trim) {
            if line.starts_with([';', '#']) {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            self.0.insert(
                expand(key.trim(), file),
                (expand(unquote(value.trim()), file), file.to_path_buf()),
            );
        }
    }

    /// The value of `key`, and the file that sets it.
    fn get(&self, key: &str) -> Option<(&str, &Path)> {
        self.0
            .get(key)
            .map(|(value, file)| (value.as_str(), file.as_path()))
    }
}

/// The text of a setting's `value`: within its quotes, `"…"` or `'…'`, else
/// up to a `;` or `#` comment.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(text) = value
            .strip_prefix(quote)
            .and_then(|value| value.strip_suffix(quote))
        {
            return text;
        }
    }
    value
        .split([';', '#'])
        .next()
        .unwrap_or_default()
        .trim_end()
}

/// Replaces each `${VAR}` of `text`, from `file`, with the value of the
/// environment variable `VAR`: an empty string, with a warning, when it is
/// not set, as npm does.
fn expand(text: &str, file: &Path) -> String {
    let mut expanded = String::new();
    let mut rest = text;
    while let Some((before, after)) = rest.split_once("${") {
        let Some((name, after)) = after.split_once('}') else {
            break;
        };
        expanded.push_str(before);
        match env::var(name) {
            Ok(value) => expanded.push_str(&value),
            Err(_) => eprintln!(
                "warning: {}: ${{{name}}} is not set, it expands to an empty string",
                file.display()
            ),
        }
        rest = after;
    }
    expanded.push_str(rest);
    expanded
}

/// Checks that `url`, which `source` names, is an http:// or https:// URL,
/// and removes its trailing slash.
fn registry_url(source: &str, url: &str) -> Result<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!(
            "{source} `{url}` is not an http:// or https:// URL\nhint: set it to the URL of an npm registry, e.g. `{DEFAULT_REGISTRY}`, or remove it"
        );
    }
    Ok(url.trim_end_matches('/').to_string())
}
