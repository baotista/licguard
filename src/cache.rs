//! The license cache: the npm registry's answers, kept across runs so that
//! warm runs are fast and `--offline` runs can use them. Published versions
//! are immutable, so entries never expire.
//!
//! Each registry has its own file, named after a hash of its URL, so that a
//! private registry never shares entries with another. A failing cache never
//! fails the gate: it is skipped with a warning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{env, fs, io, process};

use etcetera::BaseStrategy;
use serde::{Deserialize, Serialize};

use crate::inventory::Package;

/// The environment variable that names the cache directory, unless
/// `--cache-dir` does.
const CACHE_DIR: &str = "LICGUARD_CACHE_DIR";

/// The version of the file format.
const VERSION: u32 = 1;

/// The cached answers of one npm registry.
pub struct Cache {
    /// `None` when no cache directory is known.
    file: Option<PathBuf>,
    registry: String,
    /// The Declared license of each `name@version` the registry has a
    /// document for; `None` when that document declares none.
    entries: BTreeMap<String, Option<String>>,
    /// Whether `entries` differ from the file.
    changed: bool,
    /// Whether a warning was printed: one is enough.
    warned: bool,
}

/// The file of a registry's cache.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    /// For whoever reads the file; its name identifies the registry.
    registry: String,
    packages: BTreeMap<String, Option<String>>,
}

impl Cache {
    /// Loads the cache of the npm `registry`, as [`crate::registry::url`]
    /// normalizes it, from `dir`, else from `LICGUARD_CACHE_DIR`, else from
    /// the user's standard cache directory.
    pub fn open(dir: Option<&Path>, registry: &str) -> Cache {
        let mut cache = Cache {
            file: None,
            registry: registry.to_string(),
            entries: BTreeMap::new(),
            changed: false,
            warned: false,
        };
        let Some(dir) = directory(dir) else {
            cache.warn(format!(
                "cannot find the user's cache directory, the license cache is not used; set {CACHE_DIR}=DIR or --cache-dir DIR to use one"
            ));
            return cache;
        };
        let file = dir.join(format!("npm-{:016x}.json", fnv1a(&cache.registry)));
        match fs::read_to_string(&file) {
            Ok(text) => match serde_json::from_str::<CacheFile>(&text) {
                Ok(content) if content.version == VERSION => cache.entries = content.packages,
                Ok(content) => cache.warn(format!(
                    "ignoring the license cache {}: its format version {} is not {VERSION}",
                    file.display(),
                    content.version
                )),
                Err(err) => cache.warn(format!(
                    "ignoring the license cache {}: {err}",
                    file.display()
                )),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => cache.warn(format!(
                "ignoring the license cache {}: {err}",
                file.display()
            )),
        }
        cache.file = Some(file);
        cache
    }

    /// The registry's answer for `package`: `Some` with its Declared license,
    /// if any, when the registry has a document for it.
    pub fn get(&self, package: &Package) -> Option<&Option<String>> {
        self.entries.get(&key(package))
    }

    /// Records that the registry's document for `package` declares
    /// `license`.
    pub fn insert(&mut self, package: &Package, license: Option<String>) {
        self.entries.insert(key(package), license);
        self.changed = true;
    }

    /// Writes the cache back if it changed, atomically: a concurrent run
    /// reads either the old file or the new one. When two runs write at the
    /// same time, the last one wins and the other's new entries are lost;
    /// that is harmless, as entries are immutable and only cost a request.
    pub fn save(mut self) {
        let Some(file) = self.file.take().filter(|_| self.changed) else {
            return;
        };
        let content = CacheFile {
            version: VERSION,
            registry: self.registry.clone(),
            packages: std::mem::take(&mut self.entries),
        };
        let text = serde_json::to_string_pretty(&content).expect("a cache file is serializable");
        if let Err(err) = write_atomically(&file, &text) {
            self.warn(format!(
                "cannot write the license cache {}: {err}; set {CACHE_DIR}=DIR or --cache-dir DIR to a writable directory",
                file.display()
            ));
        }
    }

    /// Prints `message` as a warning on stderr, unless one already was.
    fn warn(&mut self, message: String) {
        if !self.warned {
            eprintln!("warning: {message}");
            self.warned = true;
        }
    }
}

/// The cache directory: `dir`, else `LICGUARD_CACHE_DIR`, else `licguard`
/// in the user's standard cache directory (e.g. `~/.cache` on Linux,
/// `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on Windows).
fn directory(dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = dir {
        return Some(dir.to_path_buf());
    }
    if let Some(dir) = env::var_os(CACHE_DIR).filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let strategy = etcetera::base_strategy::choose_native_strategy().ok()?;
    Some(strategy.cache_dir().join("licguard"))
}

/// Writes `text` to a temporary file next to `file`, then renames it over
/// `file`.
fn write_atomically(file: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension(format!("{}.tmp", process::id()));
    let result = fs::write(&temp, text).and_then(|()| fs::rename(&temp, file));
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// The key of `package` in its registry's file.
fn key(package: &Package) -> String {
    format!("{}@{}", package.name, package.version)
}

/// The 64-bit FNV-1a hash of `text`: unlike std's hashers, it is the same
/// in every build, so that a cache file keeps its name.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
