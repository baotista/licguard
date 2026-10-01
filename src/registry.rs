//! The registry License origin: the Declared license the npm registry
//! publishes for a Package's exact version.

use std::env;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use ureq::Agent;

use crate::inventory::{Package, npm};

/// The environment variable that names another npm registry, for tests and
/// mirrors.
const REGISTRY: &str = "LICGUARD_NPM_REGISTRY";

/// The registry queried unless [`REGISTRY`] names another.
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

/// The longest a request may take, from connection to the end of the answer.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The most requests in flight at once (NF-05).
const CONCURRENCY: usize = 16;

/// The wait before each retry of a failed request: 3 attempts in all.
const BACKOFF: [Duration; 2] = [Duration::from_millis(200), Duration::from_millis(400)];

/// For a registry that cannot be reached.
const NETWORK_HINT: &str =
    "hint: check your network or proxy, or run with --offline to use only local License origins";

/// For an answer that is neither a license nor a transient failure.
const ANSWER_HINT: &str = "hint: check that LICGUARD_NPM_REGISTRY, if set, names a public npm registry, or run with --offline to use only local License origins";

/// The license fields of a registry version document.
#[derive(Deserialize)]
struct VersionDocument {
    license: Option<serde_json::Value>,
    licenses: Option<serde_json::Value>,
}

/// What the registry answered for a Package's version.
pub enum Answer {
    /// `404`: the registry does not know the version, but may later, e.g. a
    /// mirror that has not synced it yet.
    NotFound,
    /// The version's document, with its Declared license, if any.
    Found(Option<String>),
}

/// Fetches the answer of the npm `registry`, as [`url`] returns it, for
/// each of `packages`, in order: `None` for those left unrequested once a
/// request failed for good. Also returns how many requests were sent,
/// retries included.
pub fn answers(registry: &str, packages: &[&Package]) -> (Vec<Option<Result<Answer>>>, usize) {
    if packages.is_empty() {
        return (Vec::new(), 0);
    }
    let agent: Agent = Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        // Nothing but these and the Package's name and version (NF-07).
        .user_agent(concat!("licguard/", env!("CARGO_PKG_VERSION")))
        .accept("application/json")
        .build()
        .into();
    // Workers take the next Package from a shared index, and stop taking
    // new ones once a request has failed for good.
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let sent = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<Answer>>>> =
        Mutex::new(packages.iter().map(|_| None).collect());
    thread::scope(|scope| {
        for _ in 0..CONCURRENCY.min(packages.len()) {
            scope.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(package) = packages.get(index) else {
                        break;
                    };
                    let result = fetch(&agent, registry, package, &sent);
                    if result.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    results.lock().unwrap()[index] = Some(result);
                }
            });
        }
    });
    (results.into_inner().unwrap(), sent.into_inner())
}

/// The registry to query: `LICGUARD_NPM_REGISTRY` when set, else the
/// public npm registry, without a trailing slash.
pub fn url() -> Result<String> {
    let Some(url) = env::var_os(REGISTRY) else {
        return Ok(DEFAULT_REGISTRY.to_string());
    };
    let url = url.to_string_lossy();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!(
            "{REGISTRY} `{url}` is not an http:// or https:// URL\nhint: set it to the URL of an npm registry, e.g. `{DEFAULT_REGISTRY}`, or unset it"
        );
    }
    Ok(url.trim_end_matches('/').to_string())
}

/// Fetches the Declared license of `package`, retrying connection errors,
/// timeouts, `429` and `5xx` answers with backoff, and counting each
/// request in `sent`.
fn fetch(agent: &Agent, registry: &str, package: &Package, sent: &AtomicUsize) -> Result<Answer> {
    let url = format!(
        "{registry}/{}/{}",
        encode(&package.name),
        encode(&package.version)
    );
    let what = format!("{}@{}", package.name, package.version);
    let mut retries = BACKOFF.iter();
    loop {
        sent.fetch_add(1, Ordering::Relaxed);
        let failure = match agent.get(&url).call() {
            Ok(mut response) => match response.status().as_u16() {
                200 => match response.body_mut().read_to_string() {
                    Ok(body) => return parse(&body, registry, &what).map(Answer::Found),
                    Err(err) => err.to_string(),
                },
                404 => return Ok(Answer::NotFound),
                status @ (429 | 500..=599) => format!("status {status}"),
                status => bail!(
                    "the npm registry {registry} answered status {status} for {what}\n{ANSWER_HINT}"
                ),
            },
            // Connection errors and timeouts.
            Err(err) => err.to_string(),
        };
        let Some(wait) = retries.next() else {
            bail!(
                "cannot fetch the license of {what} from the npm registry {registry}: {failure}\n{NETWORK_HINT}"
            );
        };
        thread::sleep(*wait);
    }
}

/// Reads the Declared license of a version document, the same way as an
/// installed `package.json`.
fn parse(body: &str, registry: &str, what: &str) -> Result<Option<String>> {
    let document: VersionDocument = serde_json::from_str(body).map_err(|err| {
        anyhow!(
            "the npm registry {registry} answered an invalid document for {what}: {err}\n{ANSWER_HINT}"
        )
    })?;
    Ok(npm::declared_license(
        document.license.as_ref(),
        document.licenses.as_ref(),
    ))
}

/// Percent-encodes a path segment the way npm does for a Package name:
/// `@scope/pkg` becomes `@scope%2Fpkg`.
fn encode(segment: &str) -> String {
    let mut encoded = String::new();
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
