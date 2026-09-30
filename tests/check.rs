use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A copy of a fixture Project in a temporary directory, so each test can
/// write its own Policy or alter the installed packages.
struct Project {
    dir: TempDir,
    /// Environment variables set (`Some`) or removed (`None`) for the
    /// commands run on the Project.
    env: Vec<(String, Option<String>)>,
    /// The npm registry the commands query, unless `LICGUARD_NPM_REGISTRY`
    /// is set otherwise: by default, it knows no Package.
    registry: Registry,
}

/// The date `LICGUARD_TODAY` pins by default, so that no test depends on the
/// day it runs.
const TODAY: &str = "2026-06-01";

impl Project {
    fn from_fixture(name: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        copy_dir(&fixture, dir.path());
        Project {
            dir,
            env: Vec::new(),
            registry: Registry::start(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn with_policy(self, toml: &str) -> Self {
        fs::write(self.dir.path().join("licguard.toml"), toml).unwrap();
        self
    }

    /// Sets the environment variable `key` for the commands run on the Project.
    fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), Some(value.to_string())));
        self
    }

    /// Removes the environment variable `key` for the commands run on the
    /// Project.
    fn without_env(mut self, key: &str) -> Self {
        self.env.push((key.to_string(), None));
        self
    }

    /// Makes the registry answer `responses` to `GET path`, one per
    /// request, repeating the last one: see [`Registry::respond`].
    fn with_registry_response(self, path: &str, responses: &[(u16, &str)]) -> Self {
        self.registry.respond(path, responses);
        self
    }

    fn replace_in(self, file: &str, from: &str, to: &str) -> Self {
        let path = self.dir.path().join(file);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file} does not contain {from}");
        fs::write(path, text.replacen(from, to, 1)).unwrap();
        self
    }

    fn write(self, file: &str, contents: &str) -> Self {
        let path = self.dir.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        self
    }

    fn remove(self, relative: &str) -> Self {
        let path = self.dir.path().join(relative);
        if path.is_dir() {
            fs::remove_dir_all(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        self
    }

    fn rename(self, from: &str, to: &str) -> Self {
        fs::rename(self.dir.path().join(from), self.dir.path().join(to)).unwrap();
        self
    }

    /// Edits the `packages` map of the Project's `package-lock.json`.
    fn edit_lockfile(self, edit: impl FnOnce(&mut serde_json::Value)) -> Self {
        self.edit_lockfile_in("package-lock.json", edit)
    }

    /// Edits the `packages` map of the given `package-lock.json`.
    fn edit_lockfile_in(self, file: &str, edit: impl FnOnce(&mut serde_json::Value)) -> Self {
        let path = self.dir.path().join(file);
        let mut lockfile: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        edit(&mut lockfile["packages"]);
        fs::write(path, serde_json::to_string_pretty(&lockfile).unwrap()).unwrap();
        self
    }

    /// Sets the Declared license of the installed `ms@2.1.3`.
    fn declare_ms_license(self, expression: &str) -> Self {
        self.replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            &format!(r#""license": "{expression}""#),
        )
    }

    fn check(&self) -> assert_cmd::assert::Assert {
        self.check_with(&[])
    }

    fn check_with(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.run("check", args)
    }

    fn list_with(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.run("list", args)
    }

    fn init_with(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.run("init", args)
    }

    fn waive_with(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.run("waive", args)
    }

    /// The Project's `licguard.toml`.
    fn policy_file(&self) -> String {
        fs::read_to_string(self.dir.path().join("licguard.toml")).unwrap()
    }

    /// Runs `licguard <command>` on the Project, with `args` after its path.
    fn run(&self, command: &str, args: &[&str]) -> assert_cmd::assert::Assert {
        self.run_from(Path::new("."), &self.path(), command, args)
    }

    /// Runs `licguard <command> <path>` from the directory `cwd`, with `args`
    /// after `path`, which names the Project from `cwd`.
    fn run_from(
        &self,
        cwd: &Path,
        path: &Path,
        command: &str,
        args: &[&str],
    ) -> assert_cmd::assert::Assert {
        let mut cmd = Command::cargo_bin("licguard").unwrap();
        cmd.current_dir(cwd)
            .arg(command)
            .arg(path)
            .args(args)
            .env("LICGUARD_TODAY", TODAY)
            .env("LICGUARD_NPM_REGISTRY", &self.registry.url);
        // The registry is local: a proxy of the machine must not serve it.
        for proxy in ["ALL_PROXY", "HTTPS_PROXY", "HTTP_PROXY"] {
            cmd.env_remove(proxy).env_remove(proxy.to_lowercase());
        }
        for (key, value) in &self.env {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
        cmd.assert()
    }
}

/// Parses the JSON document a command wrote on stdout.
fn stdout_json(assert: assert_cmd::assert::Assert) -> serde_json::Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("stdout is a JSON document")
}

/// A minimal HTTP/1.1 npm registry on a local port, so that no test reaches
/// the real one. It answers each request with its canned responses for the
/// path, else `404`, one connection per request, and records the requests.
struct Registry {
    url: String,
    state: Arc<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    /// The responses left by path; the last one is repeated.
    responses: Mutex<HashMap<String, VecDeque<(u16, String)>>>,
    requests: Mutex<Vec<RegistryRequest>>,
    /// How long each request is held before it is answered, in milliseconds.
    delay_ms: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

/// A request the registry received.
#[derive(Debug, Clone)]
struct RegistryRequest {
    path: String,
    /// Names in lower case, in the order received.
    headers: Vec<(String, String)>,
}

impl Registry {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(RegistryState::default());
        let server = Arc::clone(&state);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = Arc::clone(&server);
                thread::spawn(move || state.serve(stream));
            }
        });
        Registry { url, state }
    }

    /// Answers the requests for `path` with `responses`, `(status, body)`
    /// pairs, in order, repeating the last one. Status `0` closes the
    /// connection without an answer.
    fn respond(&self, path: &str, responses: &[(u16, &str)]) {
        let responses = responses
            .iter()
            .map(|(status, body)| (*status, body.to_string()))
            .collect();
        self.state
            .responses
            .lock()
            .unwrap()
            .insert(path.to_string(), responses);
    }

    fn delay(&self, delay: Duration) {
        self.state
            .delay_ms
            .store(delay.as_millis() as usize, Ordering::SeqCst);
    }

    fn requests(&self) -> Vec<RegistryRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    /// The paths requested, sorted: concurrent requests come in any order.
    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.requests().into_iter().map(|r| r.path).collect();
        paths.sort();
        paths
    }

    /// The most requests ever handled at the same time.
    fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }
}

impl RegistryState {
    fn serve(&self, stream: TcpStream) {
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let path = line.split(' ').nth(1).unwrap_or_default().to_string();
        let mut headers = Vec::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let Some((name, value)) = line.trim_end().split_once(':') else {
                break; // the blank line that ends the head
            };
            headers.push((name.to_lowercase(), value.trim().to_string()));
        }
        self.requests.lock().unwrap().push(RegistryRequest {
            path: path.clone(),
            headers,
        });
        let (status, body) = {
            let mut responses = self.responses.lock().unwrap();
            match responses.get_mut(&path) {
                Some(queue) if queue.len() > 1 => queue.pop_front().unwrap(),
                Some(queue) => queue[0].clone(),
                None => (404, r#"{"error":"Not found"}"#.to_string()),
            }
        };
        thread::sleep(Duration::from_millis(
            self.delay_ms.load(Ordering::SeqCst) as u64
        ));
        // Before answering, so that the client's next request never counts
        // this one.
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if status == 0 {
            return; // closes the connection without an answer
        }
        let response = format!(
            "HTTP/1.1 {status} Canned\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = (&stream).write_all(response.as_bytes());
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn project_with_only_allowed_licenses_passes() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = []
            "#,
        )
        .check()
        .success()
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains("6 allow"));
}

#[test]
fn denied_license_is_a_violation() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT"]
            deny = ["ISC"]
            "#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0",
        ))
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2",
        ))
        .stdout(predicate::str::contains("2 deny · 0 review · 4 allow"))
        .stdout(predicate::str::contains("✗ Policy violated (exit 1)"));
}

#[test]
fn license_in_no_list_is_reviewed_without_failing() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT"]
            "#,
        )
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  ISC             once@1.4.0",
        ))
        .stdout(predicate::str::contains("0 deny · 2 review · 4 allow"));
}

const ALLOW_ALL: &str = r#"
    [policy]
    allow = ["MIT", "ISC"]
"#;

#[test]
fn installed_copy_with_another_version_is_ignored() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .replace_in(
            "node_modules/once/package.json",
            r#""version": "1.4.0""#,
            r#""version": "1.3.3""#,
        )
        .replace_in(
            "node_modules/once/package.json",
            r#""license": "ISC""#,
            r#""license": "MIT""#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0",
        ));
}

#[test]
fn package_not_installed_uses_the_lockfile_license() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .remove("node_modules/wrappy")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2",
        ));
}

#[test]
fn optional_package_for_another_platform_uses_the_lockfile_license() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .edit_lockfile(|packages| {
            packages["node_modules/@esbuild/linux-x64"] = serde_json::json!({
                "version": "0.21.5",
                "cpu": ["x64"],
                "license": "MIT",
                "optional": true,
                "os": ["linux"],
            });
        })
        .check()
        .success()
        .stdout(predicate::str::contains("7 packages (npm)"));
}

#[test]
fn package_with_no_license_anywhere_is_unresolved() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .remove("node_modules/wrappy")
        .edit_lockfile(|packages| {
            packages["node_modules/wrappy"]
                .as_object_mut()
                .unwrap()
                .remove("license");
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    wrappy@1.0.2",
        ));
}

const DENY_ISC: &str = r#"
    [policy]
    allow = ["MIT"]
    deny = ["ISC"]
"#;

const DENY_MIT: &str = r#"
    [policy]
    allow = ["ISC"]
    deny = ["MIT"]
"#;

#[test]
fn packages_are_listed_by_verdict_then_name_in_stable_order() {
    let project = Project::from_fixture("npm-basic").with_policy(
        r#"
        [policy]
        deny = ["ISC"]
        "#,
    );
    let first = project.check().code(1).get_output().stdout.clone();
    let second = project.check().code(1).get_output().stdout.clone();
    assert_eq!(first, second);

    let stdout = String::from_utf8(first).unwrap();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("DENY") || l.starts_with("REVIEW"))
        .collect();
    assert_eq!(
        lines,
        [
            "DENY    ISC             once@1.4.0  via app > once",
            "DENY    ISC             wrappy@1.0.2  via app > once > wrappy",
            "REVIEW  MIT             @types/ms@0.7.34  via app > @types/ms  (unlisted)",
            "REVIEW  MIT             debug@4.3.4  via app > debug  (unlisted)",
            "REVIEW  MIT             ms@2.1.2  via app > debug > ms  (unlisted)",
            "REVIEW  MIT             ms@2.1.3  via app > ms  (unlisted)",
        ]
    );
}

#[test]
fn lockfile_v2_is_supported() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_MIT)
        .remove("package-lock.json")
        .rename("package-lock.v2.json", "package-lock.json")
        .check()
        .code(1)
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains("DENY    MIT             ms@2.1.2"));
}

#[test]
fn missing_lockfile_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .remove("package-lock.json")
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "no package-lock.json, yarn.lock or pnpm-lock.yaml found under",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn lockfile_v1_is_rejected_with_a_fix() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .replace_in(
            "package-lock.json",
            r#""lockfileVersion": 3"#,
            r#""lockfileVersion": 1"#,
        )
        .check()
        .code(2)
        .stderr(predicate::str::contains("lockfileVersion 1"))
        .stderr(predicate::str::contains("npm 7 or later"));
}

#[test]
fn missing_policy_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .check()
        .code(2)
        .stderr(predicate::str::contains("licguard.toml"))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn malformed_policy_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy("[policy]\nallow = MIT\n")
        .check()
        .code(2)
        .stderr(predicate::str::contains("licguard.toml is invalid"));
}

#[test]
fn unknown_license_identifier_in_policy_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy("[policy]\nallow = [\"Apache 2\"]\n")
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "`Apache 2` is not an SPDX license identifier",
        ));
}

#[test]
fn declared_license_that_is_not_spdx_is_unresolved() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            r#""license": "SEE LICENSE IN LICENSE""#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains("DENY    (unresolved)    ms@2.1.3"));
}

#[test]
fn or_later_suffix_in_policy_is_rejected_until_expressions_are_supported() {
    Project::from_fixture("npm-basic")
        .with_policy("[policy]\ndeny = [\"GPL-2.0+\"]\n")
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "`GPL-2.0+` is not an SPDX license identifier",
        ));
}

const REVIEW_ISC: &str = r#"
    [policy]
    allow = ["MIT"]
    review = ["ISC"]
"#;

#[test]
fn license_in_review_list_is_reviewed_without_failing() {
    Project::from_fixture("npm-basic")
        .with_policy(REVIEW_ISC)
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  ISC             once@1.4.0  via app > once\n",
        ))
        .stdout(predicate::str::contains("0 deny · 2 review · 4 allow"));
}

#[test]
fn strict_mode_makes_reviewed_licenses_violations() {
    Project::from_fixture("npm-basic")
        .with_policy(REVIEW_ISC)
        .check_with(&["--strict"])
        .code(1)
        .stdout(predicate::str::contains(
            "REVIEW  ISC             once@1.4.0  via app > once\n",
        ))
        .stdout(predicate::str::contains("✗ Policy violated (exit 1)"));
}

#[test]
fn unresolved_setting_sets_the_verdict_of_unresolved_licenses() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            unresolved = "review"
            "#,
        )
        .remove("node_modules/wrappy")
        .edit_lockfile(|packages| {
            packages["node_modules/wrappy"]
                .as_object_mut()
                .unwrap()
                .remove("license");
        })
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  (unresolved)    wrappy@1.0.2",
        ));
}

#[test]
fn unlisted_setting_sets_the_verdict_of_unlisted_licenses() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT"]
            unlisted = "deny"
            "#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0",
        ));
}

#[test]
fn unlisted_verdict_reason_is_shown() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT"]
            "#,
        )
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  ISC             once@1.4.0  via app > once  (unlisted)\n",
        ));
}

#[test]
fn invalid_verdict_setting_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy("[policy]\nunresolved = \"block\"\n")
        .check()
        .code(2)
        .stderr(predicate::str::contains("licguard.toml is invalid"))
        .stderr(predicate::str::contains("allow"))
        .stderr(predicate::str::contains("review"))
        .stderr(predicate::str::contains("deny"));
}

#[test]
fn or_expression_takes_the_most_favorable_verdict() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-3.0-only"]
            "#,
        )
        .declare_ms_license("GPL-3.0-only OR MIT")
        .check()
        .success()
        .stdout(predicate::str::contains("6 allow"));
}

#[test]
fn and_expression_takes_the_most_severe_verdict() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-3.0-only"]
            "#,
        )
        .declare_ms_license("MIT AND GPL-3.0-only")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT AND GPL-3.0-only ms@2.1.3",
        ));
}

#[test]
fn elected_license_of_an_or_expression_is_shown() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            review = ["MPL-2.0"]
            deny = ["GPL-3.0-only"]
            "#,
        )
        .declare_ms_license("GPL-3.0-only OR MPL-2.0")
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  MPL-2.0         ms@2.1.3  via app > ms  (elected from GPL-3.0-only OR MPL-2.0)\n",
        ));
}

#[test]
fn or_expression_elects_the_first_option_on_ties() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            review = ["MPL-2.0", "EPL-2.0"]
            "#,
        )
        .declare_ms_license("EPL-2.0 OR MPL-2.0")
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  EPL-2.0         ms@2.1.3  via app > ms  (elected from EPL-2.0 OR MPL-2.0)\n",
        ));
}

#[test]
fn with_expression_listed_in_full_uses_that_entry() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC", "GPL-2.0-only WITH Classpath-exception-2.0"]
            deny = ["GPL-2.0-only"]
            "#,
        )
        .declare_ms_license("GPL-2.0-only WITH Classpath-exception-2.0")
        .check()
        .success()
        .stdout(predicate::str::contains("6 allow"));
}

#[test]
fn with_expression_not_listed_takes_the_base_license_verdict() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-2.0-only"]
            "#,
        )
        .declare_ms_license("GPL-2.0-only WITH Classpath-exception-2.0")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    GPL-2.0-only WITH Classpath-exception-2.0 ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn gnu_or_later_license_takes_its_base_version_verdict() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-2.0-only"]
            "#,
        )
        .declare_ms_license("GPL-2.0-or-later")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    GPL-2.0-or-later ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn plus_or_later_license_takes_its_base_version_verdict() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["Apache-2.0"]
            "#,
        )
        .declare_ms_license("Apache-2.0+")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    Apache-2.0+     ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn or_later_license_listed_explicitly_uses_that_entry() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC", "GPL-2.0-or-later"]
            deny = ["GPL-2.0-only"]
            "#,
        )
        .declare_ms_license("GPL-2.0-or-later")
        .check()
        .success();
}

#[test]
fn license_in_two_lists_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "MPL-2.0"]
            review = ["MPL-2.0"]
            "#,
        )
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "`MPL-2.0` is in both `allow` and `review`",
        ));
}

#[test]
fn compound_expression_in_policy_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy("[policy]\nallow = [\"MIT OR ISC\"]\n")
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "`MIT OR ISC` is not an SPDX license identifier, optionally followed by `WITH <exception>`",
        ));
}

#[test]
fn deprecated_license_identifiers_are_still_understood() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["eCos-2.0"]
            "#,
        )
        .declare_ms_license("eCos-2.0")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    eCos-2.0        ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn each_reported_package_shows_its_introduction_path() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_MIT)
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.3  via app > ms\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.2  via app > debug > ms\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    MIT             @types/ms@0.7.34  via app > @types/ms\n",
        ));
}

#[test]
fn introduction_path_is_the_shortest_then_first_in_name_order() {
    let project = Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            // `once > wrappy` is longer than the direct `wrappy`.
            packages[""]["dependencies"]["wrappy"] = "^1.0.2".into();
            // `shared` is reached through both `once` and `debug`.
            packages["node_modules/shared"] = serde_json::json!({
                "version": "1.0.0",
                "license": "ISC",
            });
            packages["node_modules/once"]["dependencies"]["shared"] = "1".into();
            packages["node_modules/debug"]["dependencies"]["shared"] = "1".into();
        });
    let first = project.check().code(1).get_output().stdout.clone();
    let second = project.check().code(1).get_output().stdout.clone();
    assert_eq!(first, second);

    let stdout = String::from_utf8(first).unwrap();
    assert!(stdout.contains("DENY    ISC             wrappy@1.0.2  via app > wrappy\n"));
    assert!(stdout.contains("DENY    ISC             shared@1.0.0  via app > debug > shared\n"));
}

const DENY_GPL: &str = r#"
    [policy]
    allow = ["MIT", "ISC"]
    deny = ["GPL-3.0-only"]
"#;

/// Adds the `dev` Dependency `test-kit@1.0.0`, licensed `GPL-3.0-only`.
fn add_dev_dependency(packages: &mut serde_json::Value) {
    packages[""]["devDependencies"] = serde_json::json!({ "test-kit": "^1.0.0" });
    packages["node_modules/test-kit"] = serde_json::json!({
        "version": "1.0.0",
        "dev": true,
        "license": "GPL-3.0-only",
    });
}

#[test]
fn dev_dependencies_are_excluded_by_default() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .edit_lockfile(add_dev_dependency)
        .check()
        .success()
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains("test-kit").not())
        .stdout(predicate::str::contains("0 deny · 0 review · 6 allow"));
}

#[test]
fn include_dev_option_includes_dev_dependencies() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .edit_lockfile(add_dev_dependency)
        .check_with(&["--include-dev"])
        .code(1)
        .stdout(predicate::str::contains("7 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0-only    test-kit@1.0.0  via app > test-kit\n",
        ))
        .stdout(predicate::str::contains("1 deny · 0 review · 6 allow"));
}

#[test]
fn include_dev_setting_includes_dev_dependencies() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_GPL}include_dev = true\n"))
        .edit_lockfile(add_dev_dependency)
        .check()
        .code(1)
        .stdout(predicate::str::contains("7 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0-only    test-kit@1.0.0  via app > test-kit\n",
        ));
}

#[test]
fn prod_dependency_path_goes_through_prod_packages_only() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            // `app > a-test-kit > wrappy` comes first in name order, but is dev.
            packages[""]["devDependencies"] = serde_json::json!({ "a-test-kit": "^1.0.0" });
            packages["node_modules/a-test-kit"] = serde_json::json!({
                "version": "1.0.0",
                "dev": true,
                "license": "MIT",
                "dependencies": { "wrappy": "1" },
            });
        })
        .check_with(&["--include-dev"])
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2  via app > once > wrappy\n",
        ));
}

#[test]
fn prod_dependency_path_does_not_start_with_a_root_dev_dependency() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            packages[""]["devDependencies"] = serde_json::json!({ "wrappy": "^1.0.2" });
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2  via app > once > wrappy\n",
        ));
}

#[test]
fn dev_optional_and_optional_dependencies_are_prod() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .edit_lockfile(|packages| {
            // Also reached through a prod path, so npm flags it `devOptional`.
            packages[""]["devDependencies"] = serde_json::json!({ "fsevents": "^2.3.3" });
            packages[""]["optionalDependencies"] = serde_json::json!({ "fsevents": "^2.3.3" });
            packages["node_modules/fsevents"] = serde_json::json!({
                "version": "2.3.3",
                "devOptional": true,
                "license": "GPL-3.0-only",
            });
            packages["node_modules/once"]["optionalDependencies"] =
                serde_json::json!({ "native-once": "1" });
            packages["node_modules/native-once"] = serde_json::json!({
                "version": "1.0.0",
                "optional": true,
                "license": "GPL-3.0-only",
            });
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains("8 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0-only    fsevents@2.3.3  via app > fsevents\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0-only    native-once@1.0.0  via app > once > native-once\n",
        ));
}

#[test]
fn project_root_without_a_name_is_shown_by_its_directory_name() {
    let project = Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            packages[""].as_object_mut().unwrap().remove("name");
        });
    let directory = project
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    project
        .check()
        .code(1)
        .stdout(predicate::str::contains(format!(
            "DENY    ISC             once@1.4.0  via {directory} > once\n"
        )));
}

#[test]
fn dependency_resolves_to_the_nearest_copy_up_the_node_modules_tree() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            packages[""]["dependencies"]["helper"] = "^1.0.0".into();
            packages["node_modules/helper"] = serde_json::json!({
                "version": "1.0.0",
                "license": "ISC",
            });
            // `debug > ms` requires `helper@2`, installed next to it.
            packages["node_modules/debug/node_modules/ms"]["dependencies"] =
                serde_json::json!({ "helper": "2" });
            packages["node_modules/debug/node_modules/helper"] = serde_json::json!({
                "version": "2.0.0",
                "license": "ISC",
            });
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             helper@1.0.0  via app > helper\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    ISC             helper@2.0.0  via app > debug > ms > helper\n",
        ));
}

#[test]
fn deprecated_gnu_identifier_in_policy_matches_its_current_one() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-3.0"]
            "#,
        )
        .declare_ms_license("GPL-3.0-only")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0-only    ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn deprecated_gnu_identifiers_are_mapped_to_current_ones() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["GPL-2.0-only", "LGPL-2.1-or-later"]
            "#,
        )
        .declare_ms_license("GPL-2.0 OR LGPL-2.1+")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    GPL-2.0-only    ms@2.1.3  via app > ms  (elected from GPL-2.0-only OR LGPL-2.1-or-later)\n",
        ));
}

#[test]
fn common_alias_of_a_license_is_normalized() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            deny = ["Apache-2.0"]
            "#,
        )
        .declare_ms_license("Apache 2")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    Apache-2.0      ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn license_identifier_in_another_case_is_normalized() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_MIT)
        .declare_ms_license("mit AND isc")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT AND ISC     ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn full_spdx_license_name_is_normalized() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_MIT)
        .declare_ms_license("MIT License OR apache license 2.0")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "REVIEW  Apache-2.0      ms@2.1.3  via app > ms  (unlisted, elected from MIT OR Apache-2.0)\n",
        ));
}

#[test]
fn legacy_license_object_is_normalized() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            r#""license": {"type": "ISC", "url": "https://opensource.org/licenses/ISC"}"#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.3  via app > ms\n",
        ));
}

const REVIEW_ALL: &str = r#"
    [policy]
    review = ["MIT", "ISC"]
"#;

#[test]
fn legacy_licenses_array_is_read_as_a_choice() {
    Project::from_fixture("npm-basic")
        .with_policy(REVIEW_ALL)
        .replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            r#""licenses": [
                {"type": "MIT", "url": "https://opensource.org/licenses/MIT"},
                {"type": "ISC", "url": "https://opensource.org/licenses/ISC"}
            ]"#,
        )
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  MIT             ms@2.1.3  via app > ms  (elected from MIT OR ISC)\n",
        ));
}

#[test]
fn legacy_licenses_array_with_one_license_in_the_lockfile_is_that_license() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .remove("node_modules/wrappy")
        .edit_lockfile(|packages| {
            let wrappy = packages["node_modules/wrappy"].as_object_mut().unwrap();
            wrappy.remove("license");
            wrappy.insert("licenses".into(), serde_json::json!(["ISC"]));
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2  via app > once > wrappy\n",
        ));
}

#[test]
fn legacy_licenses_array_with_an_option_without_type_is_unresolved() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            r#""licenses": [{"type": "MIT"}, {"url": "https://example.com/LICENSE"}]"#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn license_field_takes_precedence_over_legacy_licenses_array() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .replace_in(
            "node_modules/ms/package.json",
            r#""license": "MIT""#,
            r#""license": "ISC", "licenses": [{"type": "MIT"}]"#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn noassertion_is_unresolved() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC"]
            unlisted = "allow"
            "#,
        )
        .declare_ms_license("MIT OR NOASSERTION")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn unlicensed_package_is_unresolved() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC", "Unlicense"]
            "#,
        )
        .declare_ms_license("UNLICENSED")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn license_name_without_a_version_is_unresolved_rather_than_guessed() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "ISC", "GPL-3.0-only", "GPL-2.0-only"]
            "#,
        )
        .declare_ms_license("GPL")
        .replace_in(
            "node_modules/once/package.json",
            r#""license": "ISC""#,
            r#""license": "gnu gpl v2""#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    ms@2.1.3  via app > ms\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    once@1.4.0  via app > once\n",
        ));
}

#[test]
fn normalized_license_uses_canonical_operators_and_only_needed_parentheses() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT"]
            review = ["ISC"]
            deny = ["BSD-3-Clause"]
            "#,
        )
        .declare_ms_license("((MIT) and (ISC or BSD-3-Clause))")
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  MIT AND ISC     ms@2.1.3  via app > ms  (elected from MIT AND (ISC OR BSD-3-Clause))\n",
        ));
}

#[test]
fn deprecated_and_current_gnu_identifiers_in_two_lists_are_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["MIT", "GPL-3.0"]
            deny = ["GPL-3.0-only"]
            "#,
        )
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "`GPL-3.0-only` is in both `allow` and `deny`",
        ));
}

#[test]
fn slash_in_declared_license_is_read_as_or() {
    Project::from_fixture("npm-basic")
        .with_policy(
            r#"
            [policy]
            allow = ["ISC"]
            review = ["X11"]
            deny = ["MIT"]
            "#,
        )
        .declare_ms_license("MIT/X11")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "REVIEW  X11             ms@2.1.3  via app > ms  (elected from MIT OR X11)\n",
        ));
}

/// `npm-corpus` is a real-world lockfile (`npm install --package-lock-only`),
/// plus the installed manifests of the old Packages whose legacy `licenses`
/// field npm drops from the lockfile.
#[test]
fn at_least_98_percent_of_real_world_packages_are_resolved() {
    let output = Project::from_fixture("npm-corpus")
        .with_policy("[policy]\n")
        .check_with(&["--include-dev"])
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let total: usize = stdout
        .split(" packages (npm)")
        .next()
        .and_then(|header| header.rsplit(' ').next())
        .and_then(|count| count.parse().ok())
        .expect("the report header counts the packages");
    let unresolved: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("(unresolved)"))
        .collect();
    let rate = unresolved.len() as f64 / total as f64 * 100.0;
    println!(
        "{} of {total} packages unresolved ({rate:.2} %):\n{}",
        unresolved.len(),
        unresolved.join("\n")
    );
    assert!(total > 300, "the corpus has only {total} packages");
    assert!(rate <= 2.0, "{rate:.2} % of the packages are unresolved");
}

/// A `[[clarifications]]` entry with its evidence.
fn clarification(package: &str, version: Option<&str>, license: &str) -> String {
    let version = version.map_or(String::new(), |v| format!("version = \"{v}\"\n"));
    format!(
        "\n[[clarifications]]\npackage = \"{package}\"\n{version}license = \"{license}\"\nevidence = \"https://example.com/{package}/LICENSE\"\n"
    )
}

#[test]
fn clarification_replaces_the_declared_license() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}",
            clarification("ms", Some("2.1.3"), "ISC")
        ))
        .declare_ms_license("SEE LICENSE IN LICENSE")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.3  via app > ms",
        ));
}

#[test]
fn clarified_package_is_marked_first_in_its_notes() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "[policy]\nallow = [\"MIT\"]\n{}",
            clarification("ms", Some("2.1.3"), "ISC")
        ))
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  ISC             ms@2.1.3  via app > ms  (clarified, unlisted)\n",
        ));
}

#[test]
fn clarification_takes_priority_over_the_lockfile_license() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_MIT}{}",
            clarification("wrappy", Some("1.0.2"), "MIT")
        ))
        .remove("node_modules/wrappy")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             wrappy@1.0.2  via app > once > wrappy  (clarified)\n",
        ));
}

#[test]
fn clarification_with_a_version_applies_to_that_version_only() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}",
            clarification("ms", Some("2.1.2"), "ISC")
        ))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.2  via app > debug > ms  (clarified)\n",
        ))
        .stdout(predicate::str::contains("ms@2.1.3").not());
}

#[test]
fn clarification_without_a_version_applies_to_all_versions() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_ISC}{}", clarification("ms", None, "ISC")))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.2  via app > debug > ms  (clarified)\n",
        ))
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.3  via app > ms  (clarified)\n",
        ));
}

#[test]
fn clarification_license_that_is_not_strict_spdx_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}{}",
            clarification("once", None, "ISC"),
            clarification("ms", None, "Apache 2")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #2 (`ms`): `license` `Apache 2` is not a valid SPDX expression",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn compound_clarification_license_is_evaluated_in_canonical_form() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"
            [policy]
            allow = ["MIT"]
            review = ["ISC"]
            deny = ["GPL-2.0-only"]
            {}"#,
            clarification("ms", Some("2.1.3"), "(GPL-2.0 OR ISC)")
        ))
        .check()
        .success()
        .stdout(predicate::str::contains(
            "REVIEW  ISC             ms@2.1.3  via app > ms  (clarified, elected from GPL-2.0-only OR ISC)\n",
        ));
}

#[test]
fn clarification_without_evidence_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[clarifications]]
            package = "ms"
            license = "MIT"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #1 (`ms`): `evidence` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn clarification_with_blank_evidence_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[clarifications]]
            package = "ms"
            license = "MIT"
            evidence = "  "
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #1 (`ms`): `evidence` is missing or empty",
        ));
}

#[test]
fn clarification_without_license_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[clarifications]]
            package = "ms"
            evidence = "https://example.com/ms/LICENSE"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #1 (`ms`): `license` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn clarification_without_package_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[clarifications]]
            license = "MIT"
            evidence = "https://example.com/ms/LICENSE"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #1: `package` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn clarification_with_an_unknown_field_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}ecosystem = \"npm\"\n",
            clarification("ms", None, "MIT")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains("unknown field `ecosystem`"));
}

#[test]
fn clarification_matching_no_package_is_a_warning_that_does_not_fail_the_gate() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}",
            clarification("left-pad", Some("1.3.0"), "MIT")
        ))
        .check()
        .success()
        .stdout(predicate::str::contains(
            "(npm)\n\nwarning: clarification for left-pad@1.3.0 matches no Package\n\n0 deny · 0 review · 6 allow\n✓ Policy respected\n",
        ));
}

#[test]
fn warnings_follow_the_packages_in_a_stable_order() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}{}{}{}",
            clarification("zlib", None, "Zlib"),
            clarification("ms", Some("9.9.9"), "MIT"),
            clarification("left-pad", Some("1.3.0"), "MIT"),
            clarification("left-pad", None, "MIT"),
        ))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "wrappy@1.0.2  via app > once > wrappy\n\n\
             warning: clarification for left-pad matches no Package\n\
             warning: clarification for left-pad@1.3.0 matches no Package\n\
             warning: clarification for ms@9.9.9 matches no Package\n\
             warning: clarification for zlib matches no Package\n\n\
             2 deny · 0 review · 4 allow\n",
        ));
}

#[test]
fn clarification_of_an_excluded_dev_dependency_is_not_a_warning() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_GPL}{}",
            clarification("test-kit", None, "MIT")
        ))
        .edit_lockfile(add_dev_dependency)
        .check()
        .success()
        .stdout(predicate::str::contains("warning:").not());
}

#[test]
fn clarification_of_a_version_prevails_over_one_for_all_versions() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}{}",
            clarification("ms", None, "ISC"),
            clarification("ms", Some("2.1.3"), "MIT")
        ))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.2  via app > debug > ms  (clarified)\n",
        ))
        .stdout(predicate::str::contains("ms@2.1.3").not());
}

#[test]
fn two_clarifications_of_the_same_package_version_are_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}{}{}",
            clarification("ms", None, "MIT"),
            clarification("ms", Some("2.1.3"), "MIT"),
            clarification("ms", None, "ISC")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #3 (`ms`): same package and version as clarification #1",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn clarification_to_noassertion_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}",
            clarification("ms", None, "MIT OR NOASSERTION")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "clarification #1 (`ms`): `license` `MIT OR NOASSERTION` asserts no license",
        ));
}

/// A `[[waivers]]` entry with its reason and expiry date.
fn waiver(package: &str, version: Option<&str>, license: &str) -> String {
    let version = version.map_or(String::new(), |v| format!("version = \"{v}\"\n"));
    format!(
        "\n[[waivers]]\npackage = \"{package}\"\n{version}license = \"{license}\"\nreason = \"Approved by legal, ticket LEGAL-142\"\nexpires = \"2027-01-01\"\n"
    )
}

#[test]
fn waiver_turns_a_denied_package_into_an_allowed_one() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}",
            waiver("once", Some("1.4.0"), "ISC")
        ))
        .check()
        .code(1)
        .stdout(predicate::str::contains("once@1.4.0").not())
        .stdout(predicate::str::contains(
            "DENY    ISC             wrappy@1.0.2",
        ))
        .stdout(predicate::str::contains(
            "1 deny · 0 review · 5 allow (1 waived)\n",
        ));
}

#[test]
fn waived_review_is_not_a_violation_in_strict_mode() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{REVIEW_ISC}{}{}",
            waiver("once", None, "ISC"),
            waiver("wrappy", None, "ISC")
        ))
        .check_with(&["--strict"])
        .success()
        .stdout(predicate::str::contains("REVIEW").not())
        .stdout(predicate::str::contains(
            "0 deny · 0 review · 6 allow (2 waived)\n✓ Policy respected\n",
        ));
}

#[test]
fn waiver_with_a_version_applies_to_that_version_only() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_MIT}{}", waiver("ms", Some("2.1.2"), "MIT")))
        .check()
        .code(1)
        .stdout(predicate::str::contains("ms@2.1.2").not())
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.3  via app > ms\n",
        ));
}

#[test]
fn waiver_for_another_license_does_not_apply() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_ISC}{}", waiver("once", None, "MIT")))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0  via app > once\n",
        ))
        .stdout(predicate::str::contains("waived").not());
}

#[test]
fn waiver_of_an_allowed_package_changes_nothing() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_ISC}{}", waiver("ms", None, "MIT")))
        .check()
        .code(1)
        .stdout(predicate::str::contains("2 deny · 0 review · 4 allow\n"));
}

#[test]
fn waiver_matches_the_license_of_a_clarification() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}{}",
            clarification("ms", None, "ISC"),
            waiver("ms", Some("2.1.3"), "ISC")
        ))
        .declare_ms_license("MIT OR Apache-2.0")
        .check()
        .code(1)
        .stdout(predicate::str::contains("ms@2.1.3").not())
        .stdout(predicate::str::contains(
            "DENY    ISC             ms@2.1.2  via app > debug > ms  (clarified)\n",
        ))
        .stdout(predicate::str::contains(
            "3 deny · 0 review · 3 allow (1 waived)\n",
        ));
}

#[test]
fn compound_waiver_license_is_matched_in_canonical_form() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"
            [policy]
            allow = ["ISC"]
            deny = ["GPL-2.0-only", "MIT"]
            {}"#,
            waiver("ms", Some("2.1.3"), "(GPL-2.0 OR MIT)")
        ))
        .declare_ms_license("GPL-2.0-only OR MIT")
        .check()
        .code(1)
        .stdout(predicate::str::contains("ms@2.1.3").not())
        .stdout(predicate::str::contains("(1 waived)"));
}

#[test]
fn waiver_license_that_is_not_strict_spdx_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}{}",
            waiver("once", None, "ISC"),
            waiver("ms", None, "Apache 2")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #2 (`ms`): `license` `Apache 2` is not a valid SPDX expression",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_of_noassertion_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}",
            waiver("ms", None, "MIT OR NOASSERTION")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #1 (`ms`): `license` `MIT OR NOASSERTION` asserts no license",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_without_reason_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[waivers]]
            package = "ms"
            license = "MIT"
            expires = "2027-01-01"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #1 (`ms`): `reason` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_with_blank_reason_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[waivers]]
            package = "ms"
            license = "MIT"
            reason = "  "
            expires = "2027-01-01"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #1 (`ms`): `reason` is missing or empty",
        ));
}

#[test]
fn waiver_without_license_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[waivers]]
            package = "ms"
            reason = "Approved by legal"
            expires = "2027-01-01"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #1 (`ms`): `license` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_without_package_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[waivers]]
            license = "MIT"
            reason = "Approved by legal"
            expires = "2027-01-01"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains("waiver #1: `package` is missing"))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_without_expiry_date_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            r#"{ALLOW_ALL}
            [[waivers]]
            package = "ms"
            license = "MIT"
            reason = "Approved by legal"
            "#
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #1 (`ms`): `expires` is missing",
        ))
        .stderr(predicate::str::contains("hint:"));
}

/// A `[[waivers]]` entry for `ms` whose `expires` is the TOML value `expires`.
fn waiver_expiring(expires: &str) -> String {
    format!(
        "\n[[waivers]]\npackage = \"ms\"\nlicense = \"MIT\"\nreason = \"Approved by legal\"\nexpires = {expires}\n"
    )
}

#[test]
fn waiver_expiry_date_that_is_not_a_calendar_date_is_a_runtime_error() {
    for expires in [
        "\"2027-13-01\"",
        "\"2027-02-30\"",
        "\"2027-02-29\"",
        "\"2027-04-31\"",
        "\"2027-00-10\"",
        "\"2027-01-00\"",
        "\"2027-1-1\"",
        "\"01/01/2027\"",
        "\"2027-01-01 \"",
        "\"next year\"",
    ] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!("{ALLOW_ALL}{}", waiver_expiring(expires)))
            .check()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "waiver #1 (`ms`): `expires` {} is not a calendar date written `YYYY-MM-DD`",
                expires.replace('"', "`")
            )))
            .stderr(predicate::str::contains("hint:"));
    }
}

#[test]
fn waiver_expiry_date_in_another_iso_8601_form_is_a_runtime_error() {
    for expires in ["\"20270101\"", "\"+002027-01-01\"", "\"2027-01-01T00:00\""] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!("{ALLOW_ALL}{}", waiver_expiring(expires)))
            .check()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "waiver #1 (`ms`): `expires` {} is not a calendar date written `YYYY-MM-DD`",
                expires.replace('"', "`")
            )));
    }
}

#[test]
fn waiver_expiry_date_is_a_string_or_a_toml_local_date() {
    for expires in [
        "\"2027-12-31\"",
        "\"2028-02-29\"",
        "\"2000-02-29\"",
        "2027-01-01",
        "2028-02-29",
    ] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!("{DENY_MIT}{}", waiver_expiring(expires)))
            // Before every date above, so that no Waiver has expired.
            .with_env("LICGUARD_TODAY", "2000-01-01")
            .check()
            .code(1)
            .stdout(predicate::str::contains("(2 waived)"));
    }
}

#[test]
fn waiver_expiry_that_is_not_a_toml_local_date_is_a_runtime_error() {
    for expires in [
        "2027-01-01T00:00:00",
        "2027-01-01T00:00:00Z",
        "12:00:00",
        "20270101",
    ] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!("{ALLOW_ALL}{}", waiver_expiring(expires)))
            .check()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "waiver #1 (`ms`): `expires` `{expires}` is not a calendar date written `YYYY-MM-DD`"
            )));
    }
}

#[test]
fn waiver_expiry_toml_local_date_that_does_not_exist_is_a_runtime_error() {
    // The TOML parser rejects it before the Waiver is validated.
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{ALLOW_ALL}{}", waiver_expiring("2027-02-30")))
        .check()
        .code(2)
        .stderr(predicate::str::contains("invalid date"));
}

#[test]
fn two_waivers_of_the_same_package_version_are_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}{}{}{}",
            waiver("ms", None, "MIT"),
            waiver("ms", Some("2.1.3"), "MIT"),
            waiver("ms", Some("2.1.3"), "ISC"),
            waiver("ms", None, "ISC")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains(
            "waiver #3 (`ms`): same package and version as waiver #2",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waiver_with_an_unknown_field_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}approved_by = \"legal\"\n",
            waiver("ms", None, "MIT")
        ))
        .check()
        .code(2)
        .stderr(predicate::str::contains("unknown field `approved_by`"));
}

#[test]
fn waiver_without_a_version_applies_to_all_versions() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_MIT}{}", waiver("ms", None, "MIT")))
        .check()
        .code(1)
        .stdout(predicate::str::contains(" ms@2.1").not())
        .stdout(predicate::str::contains(
            "2 deny · 0 review · 4 allow (2 waived)\n",
        ));
}

#[test]
fn waiver_never_applies_to_an_unresolved_package() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "[policy]\nallow = [\"ISC\"]\n{}",
            waiver("ms", Some("2.1.3"), "MIT")
        ))
        .declare_ms_license("SEE LICENSE IN LICENSE")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    ms@2.1.3  via app > ms\n",
        ));
}

/// `npm-workspaces` is a real monorepo (`npm install --package-lock-only`):
/// the root `app` with the Workspace members `apps/web` and `packages/ui`,
/// plus the independent sub-project `tools/scripts` with its own lockfile.
#[test]
fn inventory_source_in_a_subdirectory_is_detected() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ISC)
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    ISC             inherits@2.0.4  via scripts > inherits",
        ));
}

const DENY_ALL: &str = r#"
    [policy]
    deny = ["MIT", "ISC"]
"#;

#[test]
fn workspace_members_are_roots_of_introduction_paths_but_not_packages() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             debug@4.3.4  via web > debug",
        ))
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.2  via web > debug > ms",
        ))
        // Reached from both `web` and `ui`: the first root, by key, wins.
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0  via web > once",
        ))
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.3  via app > ms",
        ))
        .stdout(predicate::str::contains("web@").not())
        .stdout(predicate::str::contains("ui@").not());
}

#[test]
fn workspace_member_is_shown_by_its_name_when_it_differs_from_its_directory() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .edit_lockfile(|packages| {
            packages["apps/web"]["name"] = "@acme/web".into();
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             debug@4.3.4  via @acme/web > debug",
        ));
}

#[test]
fn package_in_several_inventory_sources_is_evaluated_once() {
    let output = Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .check()
        .code(1)
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains("6 deny · 0 review · 0 allow"))
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.matches("ms@2.1.3").count(), 1, "{stdout}");
}

const SCRIPTS_LOCKFILE: &str = "tools/scripts/package-lock.json";

#[test]
fn package_is_prod_if_any_inventory_source_has_it_prod() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .edit_lockfile_in(SCRIPTS_LOCKFILE, |packages| {
            // `dev` in the root lockfile, `prod` here.
            packages[""]["dependencies"]["@types/ms"] = "^0.7.34".into();
            packages["node_modules/@types/ms"] = serde_json::json!({
                "version": "0.7.34",
                "license": "MIT",
            });
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             @types/ms@0.7.34  via scripts > @types/ms",
        ))
        // `prod` in the root lockfile, `dev` in `tools/scripts`.
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0  via web > once",
        ));
}

#[test]
fn declared_license_comes_from_the_first_inventory_source_that_declares_one() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .edit_lockfile(|packages| {
            packages["node_modules/ms"]
                .as_object_mut()
                .unwrap()
                .remove("license");
        })
        .edit_lockfile_in(SCRIPTS_LOCKFILE, |packages| {
            packages["node_modules/once"]["license"] = "MIT".into();
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.3  via app > ms",
        ))
        .stdout(predicate::str::contains(
            "DENY    ISC             once@1.4.0  via web > once",
        ));
}

#[test]
fn report_names_the_inventory_sources_of_each_package() {
    let output = Project::from_fixture("npm-workspaces")
        .with_policy("[policy]\ndeny = [\"ISC\"]\n")
        .check()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("DENY") || l.starts_with("REVIEW"))
        .collect();
    assert_eq!(
        lines,
        [
            "DENY    ISC             inherits@2.0.4  via scripts > inherits  in tools/scripts/package-lock.json",
            "DENY    ISC             once@1.4.0  via web > once  in package-lock.json, tools/scripts/package-lock.json",
            "DENY    ISC             wrappy@1.0.2  via web > once > wrappy  in package-lock.json, tools/scripts/package-lock.json",
            "REVIEW  MIT             debug@4.3.4  via web > debug  in package-lock.json  (unlisted)",
            "REVIEW  MIT             ms@2.1.2  via web > debug > ms  in package-lock.json  (unlisted)",
            "REVIEW  MIT             ms@2.1.3  via app > ms  in package-lock.json, tools/scripts/package-lock.json  (unlisted)",
        ]
    );
}

#[test]
fn installed_copy_next_to_a_subdirectory_lockfile_is_a_license_origin() {
    Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .write(
            "tools/scripts/node_modules/inherits/package.json",
            r#"{ "name": "inherits", "version": "2.0.4", "license": "MIT" }"#,
        )
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    MIT             inherits@2.0.4  via scripts > inherits",
        ));
}

#[test]
fn package_installed_twice_in_one_lockfile_is_one_line_with_the_shortest_path() {
    let output = Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .edit_lockfile(|packages| {
            packages[""]["dependencies"]["helper"] = "^2.0.0".into();
            packages["node_modules/helper"] = serde_json::json!({
                "version": "2.0.0",
                "license": "ISC",
            });
            // `helper@1` is installed under `debug > ms` and under `once`.
            let helper = serde_json::json!({ "version": "1.0.0", "license": "ISC" });
            packages["node_modules/debug/node_modules/ms"]["dependencies"] =
                serde_json::json!({ "helper": "1" });
            packages["node_modules/debug/node_modules/ms/node_modules/helper"] = helper.clone();
            packages["node_modules/once"]["dependencies"]["helper"] = "1".into();
            packages["node_modules/once/node_modules/helper"] = helper;
        })
        .check()
        .code(1)
        .stdout(predicate::str::contains("8 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY    ISC             helper@1.0.0  via app > once > helper\n",
        ))
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(stdout.matches("helper@1.0.0").count(), 1, "{stdout}");
}

/// A lockfile whose only Dependency is `gpl-lib@1.0.0`, licensed `GPL-3.0-only`.
const GPL_LOCKFILE: &str = r#"{
  "name": "other",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "other", "dependencies": { "gpl-lib": "^1.0.0" } },
    "node_modules/gpl-lib": { "version": "1.0.0", "license": "GPL-3.0-only" }
  }
}"#;

#[test]
fn lockfiles_in_node_modules_and_hidden_directories_are_ignored() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .write("node_modules/once/package-lock.json", GPL_LOCKFILE)
        .write(".cache/old/package-lock.json", GPL_LOCKFILE)
        .check()
        .success()
        .stdout(predicate::str::contains("6 packages (npm)"));
}

/// The Verdict lines of a `check` report, in order.
fn verdict_lines(assert: assert_cmd::assert::Assert) -> Vec<String> {
    String::from_utf8(assert.get_output().stdout.clone())
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("DENY") || l.starts_with("REVIEW") || l.starts_with("ALLOW"))
        .map(str::to_string)
        .collect()
}

/// `yarn-v1` and `yarn-berry` are the same real Yarn Project (`yarn install`
/// with Yarn 1.22 and Yarn 4 with `nodeLinker: node-modules`): the root `app`
/// depends on `debug@4.3.4` (which pulls a nested `ms@2.1.2`), `ms` and
/// `once` (which pulls `wrappy`), with the dev Dependency `@types/ms`; its
/// Workspace member `packages/ui` depends on `inherits`, with the dev
/// Dependency `wrappy`.
const YARN_FIXTURES: [&str; 2] = ["yarn-v1", "yarn-berry"];

#[test]
fn yarn_lockfile_is_an_inventory_source_with_workspace_members_as_roots() {
    for fixture in YARN_FIXTURES {
        let lines = verdict_lines(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .check()
                .code(1),
        );
        assert_eq!(
            lines,
            [
                "DENY    MIT             debug@4.3.4  via app > debug",
                "DENY    ISC             inherits@2.0.4  via ui > inherits",
                "DENY    MIT             ms@2.1.2  via app > debug > ms",
                "DENY    MIT             ms@2.1.3  via app > ms",
                "DENY    ISC             once@1.4.0  via app > once",
                // `ui > wrappy` is shorter, but `dev`.
                "DENY    ISC             wrappy@1.0.2  via app > once > wrappy",
            ],
            "{fixture}"
        );
    }
}

#[test]
fn yarn_dependency_reached_only_through_dev_dependencies_is_dev() {
    for fixture in YARN_FIXTURES {
        let project = Project::from_fixture(fixture).with_policy(DENY_ALL);
        project
            .check()
            .code(1)
            .stdout(predicate::str::contains("6 packages (npm)"))
            .stdout(predicate::str::contains("@types/ms").not());
        let json = stdout_json(
            project
                .list_with(&["--format", "json", "--include-dev"])
                .success(),
        );
        let types = json_package(&json["packages"], "@types/ms", "0.7.34");
        assert_eq!(types["scope"], "dev", "{fixture}");
        assert_eq!(
            types["introduction_path"],
            serde_json::json!(["app", "@types/ms"]),
            "{fixture}"
        );
        // Also a dev Dependency of `ui`, but reached through `app > once`.
        let wrappy = json_package(&json["packages"], "wrappy", "1.0.2");
        assert_eq!(wrappy["scope"], "prod", "{fixture}");
    }
}

#[test]
fn yarn_package_without_an_installed_copy_of_its_version_is_unresolved() {
    for fixture in YARN_FIXTURES {
        let json = stdout_json(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .replace_in(
                    "node_modules/once/package.json",
                    r#""version": "1.4.0""#,
                    r#""version": "1.3.3""#,
                )
                .remove("node_modules/wrappy")
                .list_with(&["--format", "json"])
                .success(),
        );
        for name in ["once", "wrappy"] {
            let package = json_package(
                &json["packages"],
                name,
                if name == "once" { "1.4.0" } else { "1.0.2" },
            );
            assert_eq!(
                package["declared_license"],
                serde_json::Value::Null,
                "{fixture}"
            );
            assert_eq!(package["reason"], "unresolved", "{fixture}");
            assert_eq!(package["origin"], serde_json::Value::Null, "{fixture}");
        }
        let ms = json_package(&json["packages"], "ms", "2.1.2");
        assert_eq!(ms["declared_license"], "MIT", "{fixture}");
        assert_eq!(ms["origin"], "installed", "{fixture}");
    }
}

#[test]
fn yarn_installed_copy_in_a_workspace_member_is_a_license_origin() {
    for fixture in YARN_FIXTURES {
        Project::from_fixture(fixture)
            .with_policy(DENY_ALL)
            .remove("node_modules/inherits")
            .write(
                "packages/ui/node_modules/inherits/package.json",
                r#"{ "name": "inherits", "version": "2.0.4", "license": "MIT" }"#,
            )
            .check()
            .code(1)
            .stdout(predicate::str::contains(
                "DENY    MIT             inherits@2.0.4  via ui > inherits",
            ));
    }
}

#[test]
fn yarn_berry_project_in_plug_n_play_mode_takes_its_licenses_from_the_registry() {
    Project::from_fixture("yarn-berry")
        .with_policy(DENY_ALL)
        .remove("node_modules")
        .write(".pnp.cjs", "")
        .with_registry_response("/ms/2.1.3", &[(200, r#"{"license":"MIT"}"#)])
        .check()
        .code(1)
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY    MIT             ms@2.1.3  via app > ms",
        ))
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    once@1.4.0",
        ))
        .stdout(predicate::str::contains("6 deny · 0 review · 0 allow"));
}

#[test]
fn pnpm_package_not_in_the_virtual_store_takes_its_license_from_the_registry() {
    for fixture in ["pnpm-v6", "pnpm-v9"] {
        let json = stdout_json(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .remove("node_modules")
                .with_registry_response("/@types%2Fms/0.7.34", &[(200, r#"{"license":"MIT"}"#)])
                .list_with(&["--format", "json", "--include-dev"])
                .success(),
        );
        let types = json_package(&json["packages"], "@types/ms", "0.7.34");
        assert_eq!(types["declared_license"], "MIT", "{fixture}");
        assert_eq!(types["origin"], "registry", "{fixture}");
    }
}

#[test]
fn yarn_lockfile_in_no_known_format_is_a_runtime_error() {
    let not_yarn = "hello: world\n";
    let bad_v1 = "# yarn lockfile v1\n\nms@^2.1.3:\n      version \"2.1.3\"\n";
    let bad_berry = "__metadata:\n  version: 10\n\n\"ms@npm:^2.1.3\":\n  resolution: [\n";
    for (contents, reason) in [
        (
            not_yarn,
            "neither the `# yarn lockfile v1` header nor a `__metadata` key",
        ),
        (bad_v1, "line 4: unexpected indentation"),
        (bad_berry, ""),
    ] {
        Project::from_fixture("yarn-v1")
            .with_policy(DENY_ALL)
            .write("yarn.lock", contents)
            .check()
            .code(2)
            .stderr(predicate::str::contains(
                "yarn.lock is not a Yarn lockfile licguard can read",
            ))
            .stderr(predicate::str::contains(reason))
            .stderr(predicate::str::contains(
                "hint: regenerate it with `yarn install`",
            ));
    }
}

#[test]
fn yarn_lockfile_without_its_package_json_is_a_runtime_error() {
    for (fixture, manifest) in [
        ("yarn-v1", vec!["package.json"]),
        ("yarn-berry", vec!["packages", "ui", "package.json"]),
    ] {
        // The path is shown with the platform's separators.
        let shown: PathBuf = manifest.iter().collect();
        Project::from_fixture(fixture)
            .with_policy(DENY_ALL)
            .remove(&manifest.join("/"))
            .check()
            .code(2)
            .stderr(predicate::str::contains("cannot read"))
            .stderr(predicate::str::contains(shown.display().to_string()))
            .stderr(predicate::str::contains("hint:"));
    }
}

#[test]
fn yarn_v1_workspace_pattern_other_than_dir_star_is_a_runtime_error() {
    Project::from_fixture("yarn-v1")
        .with_policy(DENY_ALL)
        .replace_in("package.json", r#""packages/*""#, r#""packages/**""#)
        .check()
        .code(2)
        .stderr(predicate::str::contains("workspace pattern `packages/**`"))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn yarn_v1_workspace_members_can_be_listed_as_directories_or_in_an_object() {
    for workspaces in [r#"["packages/ui"]"#, r#"{ "packages": ["./packages/*/"] }"#] {
        Project::from_fixture("yarn-v1")
            .with_policy(DENY_ALL)
            .replace_in("package.json", r#"["packages/*"]"#, workspaces)
            .check()
            .code(1)
            .stdout(predicate::str::contains(
                "DENY    ISC             inherits@2.0.4  via ui > inherits",
            ));
    }
}

#[test]
fn yarn_lockfile_is_aggregated_with_the_other_inventory_sources() {
    Project::from_fixture("yarn-v1")
        .with_policy(DENY_GPL)
        .write("tools/other/package-lock.json", GPL_LOCKFILE)
        // Neither is an Inventory source.
        .write("node_modules/once/yarn.lock", "not a lockfile")
        .write(".cache/yarn.lock", "not a lockfile")
        .list_with(&[])
        .success()
        .stdout(format!(
            "licguard {} — 7 packages (npm)\n\n\
             ALLOW  MIT           debug@4.3.4     listed  installed  via app > debug          in yarn.lock\n\
             DENY   GPL-3.0-only  gpl-lib@1.0.0   listed  lockfile   via other > gpl-lib      in tools/other/package-lock.json\n\
             ALLOW  ISC           inherits@2.0.4  listed  installed  via ui > inherits        in yarn.lock\n\
             ALLOW  MIT           ms@2.1.2        listed  installed  via app > debug > ms     in yarn.lock\n\
             ALLOW  MIT           ms@2.1.3        listed  installed  via app > ms             in yarn.lock\n\
             ALLOW  ISC           once@1.4.0      listed  installed  via app > once           in yarn.lock\n\
             ALLOW  ISC           wrappy@1.0.2    listed  installed  via app > once > wrappy  in yarn.lock\n",
            env!("CARGO_PKG_VERSION")
        ));
}

/// `pnpm-v9` and `pnpm-v6` are the same real pnpm Project, installed with
/// pnpm 9 (`lockfileVersion: '9.0'`) and pnpm 8 (`'6.0'`), their
/// `node_modules` trimmed to the `package.json` files: the root `app`
/// depends on `debug@4.3.4` (which pulls a nested `ms@2.1.2`), `ms`, `once`
/// (which pulls `wrappy`), `supports-color` (which pulls `has-flag`, and is
/// an optional peer of `debug`, hence `debug@4.3.4(supports-color@7.2.0)`)
/// and the Workspace member `ui`, with the dev Dependency `@types/ms`; `ui`
/// (`packages/ui`) depends on `debug` and `inherits`, with the dev
/// Dependency `wrappy`.
const PNPM_FIXTURES: [&str; 2] = ["pnpm-v9", "pnpm-v6"];

#[test]
fn pnpm_lockfile_is_an_inventory_source_with_workspace_members_as_roots() {
    for fixture in PNPM_FIXTURES {
        let lines = verdict_lines(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .check()
                .code(1),
        );
        assert_eq!(
            lines,
            [
                "DENY    MIT             debug@4.3.4  via app > debug",
                "DENY    MIT             has-flag@4.0.0  via app > supports-color > has-flag",
                "DENY    ISC             inherits@2.0.4  via ui > inherits",
                "DENY    MIT             ms@2.1.2  via app > debug > ms",
                "DENY    MIT             ms@2.1.3  via app > ms",
                "DENY    ISC             once@1.4.0  via app > once",
                "DENY    MIT             supports-color@7.2.0  via app > supports-color",
                // `ui > wrappy` is shorter, but `dev`.
                "DENY    ISC             wrappy@1.0.2  via app > once > wrappy",
            ],
            "{fixture}"
        );
    }
}

#[test]
fn pnpm_dependency_reached_only_through_dev_dependencies_is_dev() {
    for fixture in PNPM_FIXTURES {
        let project = Project::from_fixture(fixture).with_policy(DENY_ALL);
        project
            .check()
            .code(1)
            .stdout(predicate::str::contains("8 packages (npm)"))
            .stdout(predicate::str::contains("@types/ms").not());
        let json = stdout_json(
            project
                .list_with(&["--format", "json", "--include-dev"])
                .success(),
        );
        assert_eq!(json["packages"].as_array().unwrap().len(), 9, "{fixture}");
        let types = json_package(&json["packages"], "@types/ms", "0.7.34");
        assert_eq!(types["scope"], "dev", "{fixture}");
        assert_eq!(
            types["introduction_path"],
            serde_json::json!(["app", "@types/ms"]),
            "{fixture}"
        );
        // Also a dev Dependency of `ui`, but reached through `app > once`.
        let wrappy = json_package(&json["packages"], "wrappy", "1.0.2");
        assert_eq!(wrappy["scope"], "prod", "{fixture}");
    }
}

#[test]
fn pnpm_package_takes_its_license_from_its_installed_copy_in_the_virtual_store() {
    for fixture in PNPM_FIXTURES {
        let json = stdout_json(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .replace_in(
                    "node_modules/.pnpm/once@1.4.0/node_modules/once/package.json",
                    r#""version": "1.4.0""#,
                    r#""version": "1.3.3""#,
                )
                .remove("node_modules/.pnpm/wrappy@1.0.2")
                .list_with(&["--format", "json", "--include-dev"])
                .success(),
        );
        for (name, version) in [("once", "1.4.0"), ("wrappy", "1.0.2")] {
            let package = json_package(&json["packages"], name, version);
            assert_eq!(
                package["declared_license"],
                serde_json::Value::Null,
                "{fixture}"
            );
            assert_eq!(package["reason"], "unresolved", "{fixture}");
            assert_eq!(package["origin"], serde_json::Value::Null, "{fixture}");
        }
        // A scoped Package, and one installed with its peers.
        for (name, version) in [("@types/ms", "0.7.34"), ("debug", "4.3.4")] {
            let package = json_package(&json["packages"], name, version);
            assert_eq!(package["declared_license"], "MIT", "{fixture}");
            assert_eq!(package["origin"], "installed", "{fixture}");
        }
    }
}

/// The `pnpm-lock.yaml` pnpm 8 writes for a Project without workspaces: its
/// root dependencies are at the top level, not under `importers`.
const PNPM_V6_SINGLE_PROJECT_LOCKFILE: &str = "lockfileVersion: '6.0'

settings:
  autoInstallPeers: true
  excludeLinksFromLockfile: false

dependencies:
  once:
    specifier: ^1.4.0
    version: 1.4.0

devDependencies:
  '@types/ms':
    specifier: ^0.7.34
    version: 0.7.34

packages:

  /@types/ms@0.7.34:
    resolution: {integrity: sha512-nG96G3Wp6acyAgJqGasjODb+acrI7KltPiRxzHPXnP3NgI28bpQDRv53olbqGXbfcgF5aiiHmO3xpwEpS5Ld9g==}
    dev: true

  /once@1.4.0:
    resolution: {integrity: sha512-lNaJgI+2Q5URQBkccEKHTQOPaXdUxnZZElQTZY0MFUAuaEqe1E+Nyvgdz/aIyNi6Z9MzO5dv1H8n58/GELp3+w==}
    dependencies:
      wrappy: 1.0.2
    dev: false

  /wrappy@1.0.2:
    resolution: {integrity: sha512-l4Sp/DRseor9wL6EvV2+TuQn63dMkPjZ/sp9XkghTEbV9KlPS1xUsZ3u7/IQO4wxtcFB4bgpQPRcR3QCvezPcQ==}
    dev: false
";

#[test]
fn pnpm_v6_lockfile_without_workspaces_has_its_root_dependencies_at_the_top_level() {
    let json = stdout_json(
        Project::from_fixture("pnpm-v6")
            .with_policy(DENY_ALL)
            .remove("pnpm-workspace.yaml")
            .remove("packages")
            .write("pnpm-lock.yaml", PNPM_V6_SINGLE_PROJECT_LOCKFILE)
            .list_with(&["--format", "json", "--include-dev"])
            .success(),
    );
    let paths: Vec<(&str, &serde_json::Value, &str)> = json["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|package| {
            (
                package["name"].as_str().unwrap(),
                &package["introduction_path"],
                package["scope"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        paths,
        [
            ("@types/ms", &serde_json::json!(["app", "@types/ms"]), "dev"),
            ("once", &serde_json::json!(["app", "once"]), "prod"),
            (
                "wrappy",
                &serde_json::json!(["app", "once", "wrappy"]),
                "prod"
            ),
        ]
    );
}

#[test]
fn pnpm_package_resolved_with_different_peers_is_one_package() {
    let ui_debug = "  packages/ui:\n    dependencies:\n      debug:\n        specifier: 4.3.4\n        version: 4.3.4";
    for (fixture, entry, before) in [
        (
            "pnpm-v9",
            "  debug@4.3.4:\n    dependencies:\n      ms: 2.1.2\n\n",
            "  has-flag@4.0.0: {}",
        ),
        (
            "pnpm-v6",
            "  /debug@4.3.4:\n    dependencies:\n      ms: 2.1.2\n    dev: false\n\n",
            "  /has-flag@4.0.0:",
        ),
    ] {
        // `ui` gets `debug` without its optional peer `supports-color`.
        let lines = verdict_lines(
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .replace_in(
                    "pnpm-lock.yaml",
                    &format!("{ui_debug}(supports-color@7.2.0)"),
                    ui_debug,
                )
                .replace_in("pnpm-lock.yaml", before, &format!("{entry}{before}"))
                .check()
                .code(1),
        );
        let debug: Vec<&String> = lines.iter().filter(|l| l.contains("debug@")).collect();
        assert_eq!(debug.len(), 1, "{fixture}: {lines:?}");
        assert_eq!(lines.len(), 8, "{fixture}: {lines:?}");
    }
}

#[test]
fn pnpm_aliased_dependency_is_the_package_it_names() {
    let ms = "      ms:\n        specifier: ^2.1.3\n        version: ";
    let alias = "      tiny-ms:\n        specifier: npm:ms@^2.1.3\n        version: ";
    for (fixture, reference) in [("pnpm-v9", "ms@2.1.3"), ("pnpm-v6", "/ms@2.1.3")] {
        Project::from_fixture(fixture)
            .with_policy(DENY_ALL)
            .replace_in(
                "pnpm-lock.yaml",
                &format!("{ms}2.1.3\n"),
                &format!("{alias}{reference}\n"),
            )
            .check()
            .code(1)
            .stdout(predicate::str::contains(
                "DENY    MIT             ms@2.1.3  via app > tiny-ms\n",
            ));
    }
}

#[test]
fn pnpm_tarball_dependency_takes_its_name_and_version_from_its_entry() {
    let tarball = "https://example.com/once-1.4.0.tgz";
    for (fixture, edits) in [
        (
            "pnpm-v9",
            vec![
                (
                    "        version: 1.4.0\n",
                    format!("        version: {tarball}\n"),
                ),
                (
                    "  once@1.4.0:\n",
                    format!("  'once@{tarball}':\n    version: 1.4.0\n"),
                ),
                ("  once@1.4.0:\n", format!("  'once@{tarball}':\n")),
            ],
        ),
        (
            "pnpm-v6",
            vec![
                (
                    "        version: 1.4.0\n",
                    "        version: '@example.com/once-1.4.0.tgz'\n".to_string(),
                ),
                (
                    "  /once@1.4.0:\n",
                    "  '@example.com/once-1.4.0.tgz':\n    name: once\n    version: 1.4.0\n"
                        .to_string(),
                ),
            ],
        ),
    ] {
        let mut project = Project::from_fixture(fixture).with_policy(DENY_ALL);
        for (from, to) in edits {
            project = project.replace_in("pnpm-lock.yaml", from, &to);
        }
        let lines = verdict_lines(project.check().code(1));
        assert_eq!(lines.len(), 8, "{fixture}: {lines:?}");
        for line in [
            "DENY    ISC             once@1.4.0  via app > once",
            "DENY    ISC             wrappy@1.0.2  via app > once > wrappy",
        ] {
            assert!(lines.contains(&line.to_string()), "{fixture}: {lines:?}");
        }
    }
}

#[test]
fn pnpm_local_directory_dependency_is_followed_but_is_no_package() {
    let once = "      once:\n        specifier: ^1.4.0\n        version: 1.4.0\n";
    let local = "      local:\n        specifier: file:./local\n        version: file:local\n";
    let directory = "    resolution: {directory: local, type: directory}\n";
    let wrappy = "    dependencies:\n      wrappy: 1.0.2\n";
    // As pnpm 9 and pnpm 8 write a `file:./local` dependency.
    for (fixture, entries) in [
        (
            "pnpm-v9",
            vec![
                (
                    "  ms@2.1.2:\n",
                    format!("  local@file:local:\n{directory}\n"),
                ),
                ("  ms@2.1.2: {}", format!("  local@file:local:\n{wrappy}\n")),
            ],
        ),
        (
            "pnpm-v6",
            vec![(
                "  /ms@2.1.2:\n",
                format!("  file:local:\n{directory}    name: local\n{wrappy}    dev: false\n\n"),
            )],
        ),
    ] {
        let mut project = Project::from_fixture(fixture)
            .with_policy(DENY_ALL)
            .write(
                "local/package.json",
                r#"{ "name": "local", "version": "0.1.0" }"#,
            )
            .replace_in("pnpm-lock.yaml", once, &format!("{local}{once}"));
        for (before, entry) in entries {
            project = project.replace_in("pnpm-lock.yaml", before, &format!("{entry}{before}"));
        }
        let lines = verdict_lines(project.check().code(1));
        assert_eq!(lines.len(), 8, "{fixture}: {lines:?}");
        assert!(
            lines.contains(
                &"DENY    ISC             wrappy@1.0.2  via app > local > wrappy".to_string()
            ),
            "{fixture}: {lines:?}"
        );
    }
}

#[test]
fn pnpm_workspace_member_is_named_after_its_package_json_else_its_directory() {
    for fixture in PNPM_FIXTURES {
        for (name, shown) in [(r#""name": "ui-kit","#, "ui-kit"), ("", "ui")] {
            Project::from_fixture(fixture)
                .with_policy(DENY_ALL)
                .replace_in("packages/ui/package.json", r#""name": "ui","#, name)
                .check()
                .code(1)
                .stdout(predicate::str::contains(format!(
                    "DENY    ISC             inherits@2.0.4  via {shown} > inherits"
                )));
        }
    }
}

#[test]
fn pnpm_lockfile_version_other_than_6_or_9_is_rejected_with_a_fix() {
    // pnpm 7 wrote a number.
    for (version, shown) in [("5.4", "5.4"), ("'7.0'", "7.0")] {
        Project::from_fixture("pnpm-v9")
            .with_policy(DENY_ALL)
            .replace_in(
                "pnpm-lock.yaml",
                "lockfileVersion: '9.0'",
                &format!("lockfileVersion: {version}"),
            )
            .check()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "pnpm-lock.yaml uses lockfileVersion {shown}, which is not supported"
            )))
            .stderr(predicate::str::contains(
                "hint: regenerate it with pnpm 8 or later",
            ));
    }
}

#[test]
fn pnpm_lockfile_that_cannot_be_read_is_a_runtime_error() {
    for contents in [
        "lockfileVersion: '9.0'\nimporters: [\n",
        "lockfileVersion: '9.0'\nimporters: 3\n",
    ] {
        Project::from_fixture("pnpm-v9")
            .with_policy(DENY_ALL)
            .write("pnpm-lock.yaml", contents)
            .check()
            .code(2)
            .stderr(predicate::str::contains(
                "pnpm-lock.yaml is not a pnpm lockfile licguard can read",
            ))
            .stderr(predicate::str::contains(
                "hint: regenerate it with `pnpm install`",
            ));
    }
}

#[test]
fn pnpm_lockfile_is_aggregated_with_the_other_inventory_sources() {
    Project::from_fixture("pnpm-v9")
        .with_policy(DENY_GPL)
        .write("tools/other/package-lock.json", GPL_LOCKFILE)
        // Neither is an Inventory source.
        .write("node_modules/.pnpm/pnpm-lock.yaml", "not a lockfile")
        .write(".cache/pnpm-lock.yaml", "not a lockfile")
        .list_with(&[])
        .success()
        .stdout(format!(
            "licguard {} — 9 packages (npm)\n\n\
             ALLOW  MIT           debug@4.3.4           listed  installed  via app > debug                      in pnpm-lock.yaml\n\
             DENY   GPL-3.0-only  gpl-lib@1.0.0         listed  lockfile   via other > gpl-lib                  in tools/other/package-lock.json\n\
             ALLOW  MIT           has-flag@4.0.0        listed  installed  via app > supports-color > has-flag  in pnpm-lock.yaml\n\
             ALLOW  ISC           inherits@2.0.4        listed  installed  via ui > inherits                    in pnpm-lock.yaml\n\
             ALLOW  MIT           ms@2.1.2              listed  installed  via app > debug > ms                 in pnpm-lock.yaml\n\
             ALLOW  MIT           ms@2.1.3              listed  installed  via app > ms                         in pnpm-lock.yaml\n\
             ALLOW  ISC           once@1.4.0            listed  installed  via app > once                       in pnpm-lock.yaml\n\
             ALLOW  MIT           supports-color@7.2.0  listed  installed  via app > supports-color             in pnpm-lock.yaml\n\
             ALLOW  ISC           wrappy@1.0.2          listed  installed  via app > once > wrappy              in pnpm-lock.yaml\n",
            env!("CARGO_PKG_VERSION")
        ));
}

/// `npm-basic` with one Package per Verdict reason and License origin, under
/// [`DENY_ISC`]: `debug` is clarified, `ms@2.1.3` is Unresolved, `wrappy` is
/// not installed.
fn audited_project() -> Project {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_ISC}{}",
            clarification("debug", None, "Apache-2.0")
        ))
        .declare_ms_license("SEE LICENSE IN LICENSE")
        .remove("node_modules/wrappy")
}

#[test]
fn list_shows_every_package_with_its_license_verdict_reason_and_origin() {
    audited_project().list_with(&[]).success().stdout(format!(
        "licguard {} — 6 packages (npm)\n\n\
         ALLOW   MIT           @types/ms@0.7.34  listed      installed      via app > @types/ms\n\
         REVIEW  Apache-2.0    debug@4.3.4       unlisted    clarification  via app > debug\n\
         ALLOW   MIT           ms@2.1.2          listed      installed      via app > debug > ms\n\
         DENY    (unresolved)  ms@2.1.3          unresolved  installed      via app > ms\n\
         DENY    ISC           once@1.4.0        listed      installed      via app > once\n\
         DENY    ISC           wrappy@1.0.2      listed      lockfile       via app > once > wrappy\n",
        env!("CARGO_PKG_VERSION")
    ));
}

#[test]
fn list_names_the_inventory_sources_of_each_package_when_there_are_several() {
    let output = Project::from_fixture("npm-workspaces")
        .with_policy("[policy]\ndeny = [\"ISC\"]\n")
        .list_with(&[])
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert_eq!(
        stdout.lines().skip(2).collect::<Vec<_>>(),
        [
            "REVIEW  MIT  debug@4.3.4     unlisted  lockfile  via web > debug          in package-lock.json",
            "DENY    ISC  inherits@2.0.4  listed    lockfile  via scripts > inherits   in tools/scripts/package-lock.json",
            "REVIEW  MIT  ms@2.1.2        unlisted  lockfile  via web > debug > ms     in package-lock.json",
            "REVIEW  MIT  ms@2.1.3        unlisted  lockfile  via app > ms             in package-lock.json, tools/scripts/package-lock.json",
            "DENY    ISC  once@1.4.0      listed    lockfile  via web > once           in package-lock.json, tools/scripts/package-lock.json",
            "DENY    ISC  wrappy@1.0.2    listed    lockfile  via web > once > wrappy  in package-lock.json, tools/scripts/package-lock.json",
        ]
    );
}

#[test]
fn list_includes_dev_dependencies_only_when_asked() {
    let project = Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .edit_lockfile(add_dev_dependency);
    project
        .list_with(&[])
        .success()
        .stdout(predicate::str::contains("6 packages (npm)"))
        .stdout(predicate::str::contains("test-kit").not());
    project
        .list_with(&["--include-dev"])
        .success()
        .stdout(predicate::str::contains("7 packages (npm)"))
        .stdout(predicate::str::contains(
            "DENY   GPL-3.0-only  test-kit@1.0.0    listed  lockfile   via app > test-kit\n",
        ));
}

#[test]
fn list_groups_packages_by_verdict_most_severe_first() {
    audited_project()
        .list_with(&["--group-by", "verdict"])
        .success()
        .stdout(format!(
            "licguard {} — 6 packages (npm)\n\n\
             DENY\n\
             DENY    (unresolved)  ms@2.1.3          unresolved  installed      via app > ms\n\
             DENY    ISC           once@1.4.0        listed      installed      via app > once\n\
             DENY    ISC           wrappy@1.0.2      listed      lockfile       via app > once > wrappy\n\
             \n\
             REVIEW\n\
             REVIEW  Apache-2.0    debug@4.3.4       unlisted    clarification  via app > debug\n\
             \n\
             ALLOW\n\
             ALLOW   MIT           @types/ms@0.7.34  listed      installed      via app > @types/ms\n\
             ALLOW   MIT           ms@2.1.2          listed      installed      via app > debug > ms\n",
            env!("CARGO_PKG_VERSION")
        ));
}

#[test]
fn list_grouped_by_verdict_skips_empty_verdicts() {
    let output = Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .list_with(&["--group-by", "verdict"])
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    let titles: Vec<&str> = stdout
        .lines()
        .filter(|l| !l.is_empty() && !l.contains('@') && !l.starts_with("licguard"))
        .collect();
    assert_eq!(titles, ["DENY", "ALLOW"], "{stdout}");
}

#[test]
fn list_groups_packages_by_license_with_unresolved_last() {
    audited_project()
        .list_with(&["--group-by", "license"])
        .success()
        .stdout(format!(
            "licguard {} — 6 packages (npm)\n\n\
             Apache-2.0\n\
             REVIEW  Apache-2.0    debug@4.3.4       unlisted    clarification  via app > debug\n\
             \n\
             ISC\n\
             DENY    ISC           once@1.4.0        listed      installed      via app > once\n\
             DENY    ISC           wrappy@1.0.2      listed      lockfile       via app > once > wrappy\n\
             \n\
             MIT\n\
             ALLOW   MIT           @types/ms@0.7.34  listed      installed      via app > @types/ms\n\
             ALLOW   MIT           ms@2.1.2          listed      installed      via app > debug > ms\n\
             \n\
             (unresolved)\n\
             DENY    (unresolved)  ms@2.1.3          unresolved  installed      via app > ms\n",
            env!("CARGO_PKG_VERSION")
        ));
}

#[test]
fn list_json_has_a_fixed_shape() {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .write("package-lock.json", GPL_LOCKFILE)
        .list_with(&["--format", "json"])
        .success()
        .stdout(
            r#"{
  "packages": [
    {
      "ecosystem": "npm",
      "name": "gpl-lib",
      "version": "1.0.0",
      "scope": "prod",
      "declared_license": "GPL-3.0-only",
      "license": "GPL-3.0-only",
      "elected": null,
      "verdict": "deny",
      "reason": "listed",
      "origin": "lockfile",
      "introduction_path": [
        "other",
        "gpl-lib"
      ],
      "sources": [
        "package-lock.json"
      ]
    }
  ]
}
"#,
        );
}

/// The JSON object of the Package `name@version` in `packages`.
fn json_package<'a>(
    packages: &'a serde_json::Value,
    name: &str,
    version: &str,
) -> &'a serde_json::Value {
    packages
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name && p["version"] == version)
        .unwrap_or_else(|| panic!("no {name}@{version} in {packages}"))
}

#[test]
fn list_json_gives_each_package_its_declared_and_normalized_license_and_origin() {
    let json = stdout_json(
        audited_project()
            .remove("node_modules/debug/node_modules/ms")
            .edit_lockfile(|packages| {
                packages["node_modules/wrappy"]
                    .as_object_mut()
                    .unwrap()
                    .remove("license");
            })
            .list_with(&["--format", "json"])
            .success(),
    );
    let packages = &json["packages"];
    let ids: Vec<String> = packages
        .as_array()
        .unwrap()
        .iter()
        .map(|p| format!("{} {}@{}", p["ecosystem"], p["name"], p["version"]))
        .collect();
    assert_eq!(
        ids,
        [
            r#""npm" "@types/ms"@"0.7.34""#,
            r#""npm" "debug"@"4.3.4""#,
            r#""npm" "ms"@"2.1.2""#,
            r#""npm" "ms"@"2.1.3""#,
            r#""npm" "once"@"1.4.0""#,
            r#""npm" "wrappy"@"1.0.2""#,
        ]
    );
    let fields = |name, version| {
        let p = json_package(packages, name, version);
        [
            &p["declared_license"],
            &p["license"],
            &p["verdict"],
            &p["reason"],
            &p["origin"],
        ]
        .map(|v| v.clone())
    };
    use serde_json::json;
    assert_eq!(
        fields("debug", "4.3.4"),
        [
            json!("MIT"),
            json!("Apache-2.0"),
            json!("review"),
            json!("unlisted"),
            json!("clarification")
        ]
    );
    assert_eq!(
        fields("ms", "2.1.2"),
        [
            json!("MIT"),
            json!("MIT"),
            json!("allow"),
            json!("listed"),
            json!("lockfile")
        ]
    );
    assert_eq!(
        fields("ms", "2.1.3"),
        [
            json!("SEE LICENSE IN LICENSE"),
            json!(null),
            json!("deny"),
            json!("unresolved"),
            json!("installed")
        ]
    );
    assert_eq!(
        fields("wrappy", "1.0.2"),
        [
            json!(null),
            json!(null),
            json!("deny"),
            json!("unresolved"),
            json!(null)
        ]
    );
}

#[test]
fn list_json_gives_each_package_its_scope_elected_license_and_introduction_path() {
    let json = stdout_json(
        Project::from_fixture("npm-basic")
            .with_policy(ALLOW_ALL)
            .declare_ms_license("GPL-3.0-only OR MIT")
            .edit_lockfile(|packages| {
                add_dev_dependency(packages);
                // Required by nothing.
                packages["node_modules/orphan"] =
                    serde_json::json!({ "version": "1.0.0", "license": "MIT" });
            })
            .list_with(&["--format", "json", "--include-dev"])
            .success(),
    );
    let packages = &json["packages"];
    let ms = json_package(packages, "ms", "2.1.3");
    assert_eq!(ms["scope"], "prod");
    assert_eq!(ms["license"], "GPL-3.0-only OR MIT");
    assert_eq!(ms["elected"], "MIT");
    assert_eq!(ms["introduction_path"], serde_json::json!(["app", "ms"]));
    assert_eq!(ms["sources"], serde_json::json!(["package-lock.json"]));
    let test_kit = json_package(packages, "test-kit", "1.0.0");
    assert_eq!(test_kit["scope"], "dev");
    assert_eq!(test_kit["elected"], serde_json::Value::Null);
    let orphan = json_package(packages, "orphan", "1.0.0");
    assert_eq!(orphan["introduction_path"], serde_json::Value::Null);
}

#[test]
fn license_origin_is_that_of_the_inventory_source_that_declares_the_license() {
    let json = stdout_json(
        Project::from_fixture("npm-workspaces")
            .with_policy(DENY_ALL)
            .edit_lockfile(|packages| {
                packages["node_modules/ms"]
                    .as_object_mut()
                    .unwrap()
                    .remove("license");
            })
            .write(
                "tools/scripts/node_modules/ms/package.json",
                r#"{ "name": "ms", "version": "2.1.3", "license": "ISC" }"#,
            )
            .list_with(&["--format", "json"])
            .success(),
    );
    let ms = json_package(&json["packages"], "ms", "2.1.3");
    assert_eq!(ms["declared_license"], "ISC");
    assert_eq!(ms["origin"], "installed");
    assert_eq!(
        ms["sources"],
        serde_json::json!(["package-lock.json", "tools/scripts/package-lock.json"])
    );
}

#[test]
fn check_json_reports_violations_and_warnings() {
    let project = Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}{}",
        clarification("left-pad", Some("1.3.0"), "MIT"),
        clarification("left-pad", None, "MIT"),
    ));
    let json = stdout_json(project.check_with(&["--format", "json"]).code(1));
    let list = stdout_json(project.list_with(&["--format", "json"]).success());
    assert_eq!(json["violated"], true);
    assert_eq!(
        json["summary"],
        serde_json::json!({ "deny": 2, "review": 0, "allow": 4, "waived": 0 })
    );
    // The same Package objects as `list`.
    assert_eq!(
        json["violations"],
        serde_json::json!([
            json_package(&list["packages"], "once", "1.4.0"),
            json_package(&list["packages"], "wrappy", "1.0.2"),
        ])
    );
    assert_eq!(
        json["warnings"],
        serde_json::json!([
            {
                "kind": "unmatched_clarification",
                "message": "clarification for left-pad matches no Package",
                "package": "left-pad",
                "version": null,
            },
            {
                "kind": "unmatched_clarification",
                "message": "clarification for left-pad@1.3.0 matches no Package",
                "package": "left-pad",
                "version": "1.3.0",
            },
        ])
    );
}

#[test]
fn check_json_violations_honour_strict_mode_and_keep_the_exit_code() {
    let project = Project::from_fixture("npm-basic").with_policy(REVIEW_ISC);
    let violations = |json: &serde_json::Value| -> Vec<String> {
        json["violations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                format!(
                    "{}@{}",
                    p["name"].as_str().unwrap(),
                    p["version"].as_str().unwrap()
                )
            })
            .collect()
    };

    let json = stdout_json(project.check_with(&["--format", "json"]).success());
    assert_eq!(json["violated"], false);
    assert_eq!(json["summary"]["review"], 2);
    assert!(violations(&json).is_empty(), "{json}");

    let json = stdout_json(
        project
            .check_with(&["--format", "json", "--strict"])
            .code(1),
    );
    assert_eq!(json["violated"], true);
    assert_eq!(violations(&json), ["once@1.4.0", "wrappy@1.0.2"]);
}

#[test]
fn check_output_option_writes_the_report_to_a_file_instead_of_stdout() {
    let project = outside_github_actions(Project::from_fixture("npm-basic").with_policy(DENY_ISC));
    for format in ["text", "json", "github"] {
        let file = project.path().join(format!("report.{format}"));
        let expected = project
            .check_with(&["--format", format])
            .code(1)
            .get_output()
            .stdout
            .clone();
        project
            .check_with(&["--format", format, "--output", file.to_str().unwrap()])
            .code(1)
            .stdout("");
        assert_eq!(fs::read(&file).unwrap(), expected, "{format}");
    }
}

#[test]
fn list_output_option_writes_the_table_to_a_file_instead_of_stdout() {
    let project = audited_project();
    let file = project.path().join("inventory.txt");
    let expected = project
        .list_with(&["--group-by", "license"])
        .success()
        .get_output()
        .stdout
        .clone();
    project
        .list_with(&["--group-by", "license", "--output", file.to_str().unwrap()])
        .success()
        .stdout("");
    assert_eq!(fs::read(&file).unwrap(), expected);
}

#[test]
fn unwritable_output_file_is_a_runtime_error() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    let file = project.path().join("missing").join("report.txt");
    for command in ["check", "list"] {
        project
            .run(command, &["--output", file.to_str().unwrap()])
            .code(2)
            .stdout("")
            .stderr(predicate::str::contains("cannot write"))
            .stderr(predicate::str::contains("report.txt"))
            .stderr(predicate::str::contains("hint:"));
    }
}

#[test]
fn group_by_option_with_the_json_format_is_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .list_with(&["--format", "json", "--group-by", "license"])
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "`--group-by` applies only to `--format table`",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn list_exits_2_only_on_a_runtime_error() {
    Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .remove("package-lock.json")
        .list_with(&[])
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "no package-lock.json, yarn.lock or pnpm-lock.yaml found under",
        ))
        .stderr(predicate::str::contains("hint:"));
}

#[test]
fn waived_package_is_reported_as_such_in_json() {
    let project = Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}",
        waiver("once", Some("1.4.0"), "ISC")
    ));

    let list = stdout_json(project.list_with(&["--format", "json"]).success());
    let once = list["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "once")
        .unwrap();
    assert_eq!(once["verdict"], "allow");
    assert_eq!(once["reason"], "waived");
    assert_eq!(once["license"], "ISC");

    let check = stdout_json(project.check_with(&["--format", "json"]).code(1));
    assert_eq!(
        check["summary"],
        serde_json::json!({"deny": 1, "review": 0, "allow": 5, "waived": 1})
    );
}

#[test]
fn init_writes_a_policy_that_check_and_list_accept() {
    let project = Project::from_fixture("npm-basic");

    project.init_with(&[]).success();

    project.check().success();
    project.list_with(&[]).success();
}

#[test]
fn init_says_the_template_is_not_legal_advice() {
    let project = Project::from_fixture("npm-basic");
    let written = project.path().join("licguard.toml");

    project
        .init_with(&[])
        .success()
        .stdout(predicate::str::contains(format!(
            "wrote {}",
            written.display()
        )))
        .stdout(predicate::str::contains("run `licguard check`"))
        .stdout(predicate::str::contains("not legal advice"))
        .stdout(predicate::str::contains("your own legal counsel"));

    let policy = project.policy_file();
    assert!(policy.starts_with('#'), "the disclaimer heads the file");
    assert!(policy.contains("not legal advice"));
    assert!(policy.contains("your own legal counsel"));
}

#[test]
fn init_template_sets_every_list_and_setting_with_a_comment() {
    let project = Project::from_fixture("npm-basic");
    project.init_with(&[]).success();
    let text = project.policy_file();

    let config: toml::Table = toml::from_str(&text).unwrap();
    let expected: toml::Table = toml::from_str(
        r#"
        allow = ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "0BSD", "Unlicense", "CC0-1.0"]
        review = ["MPL-2.0", "LGPL-2.1-only", "LGPL-3.0-only", "EPL-2.0", "CDDL-1.0"]
        deny = ["GPL-2.0-only", "GPL-3.0-only", "AGPL-3.0-only", "SSPL-1.0", "BUSL-1.1"]
        unresolved = "deny"
        unlisted = "review"
        include_dev = false
        waiver_expiry_warning_days = 30
        "#,
    )
    .unwrap();
    assert_eq!(config["policy"], toml::Value::Table(expected));

    let lines: Vec<&str> = text.lines().collect();
    for key in [
        "allow",
        "review",
        "deny",
        "unresolved",
        "unlisted",
        "include_dev",
        "waiver_expiry_warning_days",
    ] {
        let at = lines
            .iter()
            .position(|line| line.starts_with(&format!("{key} =")))
            .unwrap_or_else(|| panic!("`{key}` is set"));
        assert!(lines[at - 1].starts_with('#'), "a comment explains `{key}`");
    }
}

/// Uncomments the commented-out TOML lines of `text`: a `# [[table]]` header
/// or a `# key = value` pair.
fn uncomment_examples(text: &str) -> String {
    text.lines()
        .map(|line| match line.strip_prefix("# ") {
            Some(toml)
                if (toml.starts_with("[[") && toml.ends_with("]]"))
                    || toml.split_once(" =").is_some_and(|(key, _)| {
                        key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    }) =>
            {
                toml
            }
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn init_template_examples_are_valid_once_uncommented() {
    let project = Project::from_fixture("npm-basic");
    project.init_with(&[]).success();
    let uncommented = uncomment_examples(&project.policy_file());

    let config: toml::Table = toml::from_str(&uncommented).unwrap();
    let keys = |array: &str| -> Vec<Vec<String>> {
        config
            .get(array)
            .and_then(toml::Value::as_array)
            .unwrap_or_else(|| panic!("an example `[[{array}]]` entry"))
            .iter()
            .map(|entry| {
                let mut keys: Vec<String> = entry.as_table().unwrap().keys().cloned().collect();
                keys.sort();
                keys
            })
            .collect()
    };
    assert_eq!(
        keys("clarifications"),
        [["evidence", "license", "package", "version"]]
    );
    assert_eq!(
        keys("waivers"),
        [["expires", "license", "package", "reason", "version"]]
    );
    // A placeholder that never looks like a real date that has passed.
    assert_eq!(config["waivers"][0]["expires"].as_str(), Some("2099-12-31"));

    project
        .with_policy(&uncommented)
        .check()
        .success()
        .stderr("");
}

#[test]
fn init_refuses_to_overwrite_an_existing_policy_without_force() {
    let project = Project::from_fixture("npm-basic").with_policy(ALLOW_ALL);

    project
        .init_with(&[])
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("licguard.toml already exists"))
        .stderr(predicate::str::contains("hint: pass `--force`"));
    assert_eq!(project.policy_file(), ALLOW_ALL);

    project.init_with(&["--force"]).success();
    assert!(project.policy_file().contains("not legal advice"));
    project.check().success();
}

#[test]
fn init_in_a_missing_directory_is_a_runtime_error() {
    let project = Project::from_fixture("npm-basic");

    Command::cargo_bin("licguard")
        .unwrap()
        .arg("init")
        .arg(project.path().join("missing"))
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("cannot write"))
        .stderr(predicate::str::contains(
            "hint: check that the directory exists and is writable",
        ));
}

#[test]
fn waiver_expiring_within_the_warning_days_still_applies_and_is_a_warning() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_MIT}{}", waiver_expiring("\"2026-06-11\"")))
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "\n\nwarning: waiver for ms expires in 10 days (2026-06-11)\n\n2 deny · 0 review · 4 allow (2 waived)\n",
        ));
}

#[test]
fn waiver_expiry_warning_starts_waiver_expiry_warning_days_ahead_and_lasts_until_its_expiry_day() {
    for (expires, warning) in [
        ("2026-06-01", Some("expires today (2026-06-01)")),
        ("2026-06-02", Some("expires in 1 day (2026-06-02)")),
        ("2026-07-01", Some("expires in 30 days (2026-07-01)")),
        ("2026-07-02", None),
    ] {
        let assert = Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "{DENY_MIT}{}",
                waiver_expiring(&format!("\"{expires}\""))
            ))
            .check()
            .code(1)
            .stdout(predicate::str::contains("(2 waived)"));
        match warning {
            Some(warning) => assert.stdout(predicate::str::contains(format!(
                "warning: waiver for ms {warning}\n"
            ))),
            None => assert.stdout(predicate::str::contains("warning:").not()),
        };
    }
}

#[test]
fn waiver_matching_no_package_is_a_warning_that_does_not_fail_the_gate() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}{}{}",
            // The package was removed, its version or its license changed.
            waiver("left-pad", None, "MIT"),
            waiver("ms", Some("9.9.9"), "MIT"),
            waiver("once", Some("1.4.0"), "MIT"),
        ))
        .check()
        .success()
        .stdout(predicate::str::contains(
            "(npm)\n\n\
             warning: waiver for left-pad matches no Package\n\
             warning: waiver for ms@9.9.9 matches no Package\n\
             warning: waiver for once@1.4.0 matches no Package\n\n\
             0 deny · 0 review · 6 allow\n✓ Policy respected\n",
        ));
}

#[test]
fn waiver_both_unmatched_and_expired_is_only_reported_as_unmatched() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{ALLOW_ALL}{}",
            waiver_expiring("\"2026-05-31\"").replace("\"MIT\"", "\"ISC\"")
        ))
        .check()
        .success()
        .stdout(predicate::str::contains(
            "\n\nwarning: waiver for ms matches no Package\n\n",
        ));
}

#[test]
fn waiver_of_an_excluded_dev_dependency_is_not_a_warning() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{DENY_GPL}{}",
            waiver("test-kit", None, "GPL-3.0-only")
        ))
        .edit_lockfile(add_dev_dependency)
        .check()
        .success()
        .stdout(predicate::str::contains("warning:").not());
}

#[test]
fn waiver_expiry_warning_days_setting_sets_how_early_the_warning_starts() {
    for (days, expires, warns) in [
        (7, "2026-06-08", true),
        (7, "2026-06-09", false),
        (0, "2026-06-01", true),
        (0, "2026-06-02", false),
        (365, "2027-06-01", true),
    ] {
        let assert = Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "[policy]\nallow = [\"ISC\"]\ndeny = [\"MIT\"]\nwaiver_expiry_warning_days = {days}\n{}",
                waiver_expiring(&format!("\"{expires}\""))
            ))
            .check()
            .code(1);
        let warning = predicate::str::contains("warning: waiver for ms expires");
        if warns {
            assert.stdout(warning);
        } else {
            assert.stdout(warning.not());
        }
    }
}

#[test]
fn waiver_expiry_warning_days_that_is_not_a_non_negative_integer_is_a_runtime_error() {
    for days in ["-1", "\"30\"", "7.5", "true"] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "[policy]\nallow = [\"MIT\", \"ISC\"]\nwaiver_expiry_warning_days = {days}\n"
            ))
            .check()
            .code(2)
            .stderr(predicate::str::contains("licguard.toml is invalid"))
            .stderr(predicate::str::contains("waiver_expiry_warning_days"));
    }
}

#[test]
fn expired_waiver_no_longer_applies_and_is_a_warning() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_MIT}{}", waiver_expiring("\"2026-05-31\"")))
        .check()
        .code(1)
        .stdout(predicate::str::contains("DENY    MIT             ms@2.1.3"))
        .stdout(predicate::str::contains(
            "\n\nwarning: waiver for ms expired on 2026-05-31\n\n4 deny · 0 review · 2 allow\n✗ Policy violated (exit 1)\n",
        ));
}

#[test]
fn days_before_a_waiver_expiry_are_counted_across_months_years_and_leap_days() {
    for (today, expires, days) in [
        ("2026-12-31", "2027-01-01", "in 1 day"),
        ("2028-02-28", "2028-03-01", "in 2 days"),
        ("2027-02-28", "2027-03-01", "in 1 day"),
        ("2100-02-28", "2100-03-01", "in 1 day"),
        ("2400-02-28", "2400-03-01", "in 2 days"),
        ("2026-01-31", "2026-03-02", "in 30 days"),
        ("1999-12-31", "1999-12-31", "today"),
    ] {
        Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "{DENY_MIT}{}",
                waiver_expiring(&format!("\"{expires}\""))
            ))
            .with_env("LICGUARD_TODAY", today)
            .check()
            .code(1)
            .stdout(predicate::str::contains(format!(
                "warning: waiver for ms expires {days} ({expires})\n"
            )));
    }
}

#[test]
fn check_json_reports_waiver_warnings_with_their_expiry_date() {
    let project = Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}{}{}",
        waiver("once", Some("1.4.0"), "ISC").replace("2027-01-01", "2026-06-02"),
        waiver("wrappy", None, "ISC").replace("2027-01-01", "2026-05-01"),
        waiver("left-pad", None, "MIT"),
    ));
    let json = stdout_json(project.check_with(&["--format", "json"]).code(1));
    assert_eq!(
        json["warnings"],
        serde_json::json!([
            {
                "kind": "unmatched_waiver",
                "message": "waiver for left-pad matches no Package",
                "package": "left-pad",
                "version": null,
                "expires": "2027-01-01",
            },
            {
                "kind": "expired_waiver",
                "message": "waiver for wrappy expired on 2026-05-01",
                "package": "wrappy",
                "version": null,
                "expires": "2026-05-01",
            },
            {
                "kind": "expiring_waiver",
                "message": "waiver for once@1.4.0 expires in 1 day (2026-06-02)",
                "package": "once",
                "version": "1.4.0",
                "expires": "2026-06-02",
                "days": 1,
            },
        ])
    );
    let wrappy = json["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "wrappy")
        .unwrap();
    assert_eq!(wrappy["verdict"], "deny");
}

#[test]
fn today_that_is_not_a_calendar_date_is_a_runtime_error() {
    for today in ["2026-02-30", "2026-6-1", "tomorrow", ""] {
        for command in ["check", "list"] {
            Project::from_fixture("npm-basic")
                .with_policy(ALLOW_ALL)
                .with_env("LICGUARD_TODAY", today)
                .run(command, &[])
                .code(2)
                .stdout("")
                .stderr(predicate::str::contains(format!(
                    "`LICGUARD_TODAY` `{today}` is not a calendar date written `YYYY-MM-DD`"
                )))
                .stderr(predicate::str::contains("hint:"));
        }
    }
}

#[test]
fn help_documents_how_to_pin_today() {
    for command in ["check", "list", "waive"] {
        Command::cargo_bin("licguard")
            .unwrap()
            .args([command, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("LICGUARD_TODAY=YYYY-MM-DD"));
    }
}

#[test]
fn today_is_the_current_date_when_licguard_today_is_not_set() {
    Project::from_fixture("npm-basic")
        .with_policy(&format!("{DENY_MIT}{}", waiver_expiring("\"2000-01-01\"")))
        .without_env("LICGUARD_TODAY")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "warning: waiver for ms expired on 2000-01-01\n",
        ));
}

#[test]
fn today_is_the_local_date_when_licguard_today_is_not_set() {
    // Kiritimati (UTC+14) and Pago Pago (UTC-11) are 25 hours apart, so their
    // local dates always differ, whatever the time of day.
    let days_until_expiry_in = |time_zone| {
        let project = Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "[policy]\nallow = [\"ISC\"]\ndeny = [\"MIT\"]\nwaiver_expiry_warning_days = 1000000\n{}",
                waiver_expiring("\"2999-12-31\"")
            ))
            .without_env("LICGUARD_TODAY")
            .with_env("TZ", time_zone);
        let json = stdout_json(project.check_with(&["--format", "json"]).code(1));
        assert_eq!(json["warnings"][0]["kind"], "expiring_waiver");
        json["warnings"][0]["days"].as_i64().unwrap()
    };
    let ahead = days_until_expiry_in("Pacific/Kiritimati");
    let behind = days_until_expiry_in("Pacific/Pago_Pago");
    assert!(
        behind - ahead >= 1,
        "{behind} days from Pago Pago is not more than {ahead} days from Kiritimati"
    );
}

#[test]
fn waiver_warnings_never_fail_the_gate_even_in_strict_mode_and_are_sorted_by_kind_then_package() {
    let expiring_soon = |package| waiver(package, None, "ISC").replace("2027-01-01", "2026-06-05");
    Project::from_fixture("npm-basic")
        .with_policy(&format!(
            "{REVIEW_ISC}{}{}{}{}{}{}",
            expiring_soon("wrappy"),
            expiring_soon("once"),
            // `ms` is allowed anyway, so its expiry changes no Verdict.
            waiver_expiring("\"2026-05-31\""),
            waiver("left-pad", Some("1.3.0"), "MIT"),
            waiver("left-pad", None, "MIT"),
            clarification("zlib", None, "Zlib"),
        ))
        .check_with(&["--strict"])
        .success()
        .stdout(predicate::str::contains(
            "(npm)\n\n\
             warning: clarification for zlib matches no Package\n\
             warning: waiver for left-pad matches no Package\n\
             warning: waiver for left-pad@1.3.0 matches no Package\n\
             warning: waiver for ms expired on 2026-05-31\n\
             warning: waiver for once expires in 4 days (2026-06-05)\n\
             warning: waiver for wrappy expires in 4 days (2026-06-05)\n\n\
             0 deny · 0 review · 6 allow (2 waived)\n✓ Policy respected\n",
        ));
}

/// The Project, without the GitHub Actions variables that `check --format
/// github` reads, so that no test depends on running in GitHub Actions.
fn outside_github_actions(project: Project) -> Project {
    project
        .without_env("GITHUB_WORKSPACE")
        .without_env("GITHUB_STEP_SUMMARY")
}

/// The Project in a GitHub Actions workspace: `GITHUB_WORKSPACE` is its
/// directory, and there is no job summary.
fn in_github_workspace(project: Project) -> Project {
    let workspace = project.path().to_str().unwrap().to_string();
    outside_github_actions(project).with_env("GITHUB_WORKSPACE", &workspace)
}

/// Runs `check --format github` on the Project, with `args` after it.
fn check_github(project: &Project, args: &[&str]) -> assert_cmd::assert::Assert {
    project.check_with(&[&["--format", "github"], args].concat())
}

#[test]
fn check_github_emits_an_error_annotation_per_violation_on_its_lockfile_entry() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(DENY_ISC));
    check_github(&project, &[]).code(1).stdout(
        "::error file=package-lock.json,line=52,title=licguard%3A DENY ISC once@1.4.0::ISC  via app > once\n\
         ::error file=package-lock.json,line=61,title=licguard%3A DENY ISC wrappy@1.0.2::ISC  via app > once > wrappy\n\
         2 deny · 0 review · 4 allow — ✗ Policy violated (exit 1)\n",
    );
}

#[test]
fn check_github_emits_a_warning_annotation_per_warning_on_the_policy_file() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(&format!(
        "{ALLOW_ALL}{}{}",
        clarification("left-pad", None, "MIT"),
        waiver("once", None, "ISC").replace("2027-01-01", "2026-05-31"),
    )));
    check_github(&project, &[]).success().stdout(
        "::warning file=licguard.toml,title=licguard%3A unmatched_clarification::clarification for left-pad matches no Package\n\
         ::warning file=licguard.toml,title=licguard%3A expired_waiver::waiver for once expired on 2026-05-31\n\
         0 deny · 0 review · 6 allow — ✓ Policy respected\n",
    );
}

/// The 1-based number of the first line of the Project's `file` that starts
/// with `prefix`, after its indentation.
fn line_starting_with(project: &Project, file: &str, prefix: &str) -> usize {
    let text = fs::read_to_string(project.path().join(file)).unwrap();
    let index = text
        .lines()
        .position(|line| line.trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("no line of {file} starts with {prefix}"));
    index + 1
}

#[test]
fn check_github_points_to_the_dependency_graph_entry_in_a_pnpm_lockfile() {
    for fixture in PNPM_FIXTURES {
        let project = in_github_workspace(Project::from_fixture(fixture).with_policy(DENY_ALL));
        // The v9 `snapshots` entry, after the `packages` one; the v6
        // `packages` entry. Scoped keys are quoted, and an entry without
        // dependencies is `{}` on the same line.
        let (after, header) = if fixture == "pnpm-v9" {
            ("snapshots:", "'@types/ms@0.7.34':")
        } else {
            ("packages:", "/@types/ms@0.7.34:")
        };
        let section = line_starting_with(&project, "pnpm-lock.yaml", after);
        let text = fs::read_to_string(project.path().join("pnpm-lock.yaml")).unwrap();
        let line = text
            .lines()
            .enumerate()
            .skip(section)
            .find(|(_, line)| line.trim_start().starts_with(header))
            .map(|(index, _)| index + 1)
            .unwrap();
        check_github(&project, &["--include-dev"])
            .code(1)
            .stdout(predicate::str::contains(format!(
                "::error file=pnpm-lock.yaml,line={line},title=licguard%3A DENY MIT @types/ms@0.7.34::"
            )));
    }
}

#[test]
fn check_github_points_to_the_entry_header_in_a_yarn_lockfile() {
    for fixture in YARN_FIXTURES {
        let project = in_github_workspace(Project::from_fixture(fixture).with_policy(DENY_ISC));
        // Berry quotes its entry headers.
        let header = if fixture == "yarn-v1" {
            "once@"
        } else {
            "\"once@"
        };
        let line = line_starting_with(&project, "yarn.lock", header);
        check_github(&project, &[])
            .code(1)
            .stdout(predicate::str::contains(format!(
                "::error file=yarn.lock,line={line},title=licguard%3A DENY ISC once@1.4.0::ISC  via app > once\n"
            )));
    }
}

#[test]
fn check_github_points_to_the_lockfile_entry_that_gives_the_introduction_path() {
    let project = in_github_workspace(
        Project::from_fixture("npm-basic")
            .with_policy(DENY_ISC)
            .edit_lockfile(|packages| {
                // `helper@1` is installed under `debug > ms`, first in key
                // order, and under `once`, the shortest path.
                let helper = serde_json::json!({ "version": "1.0.0", "license": "ISC" });
                packages["node_modules/debug/node_modules/ms"]["dependencies"] =
                    serde_json::json!({ "helper": "1" });
                packages["node_modules/debug/node_modules/ms/node_modules/helper"] = helper.clone();
                packages["node_modules/once"]["dependencies"]["helper"] = "1".into();
                packages["node_modules/once/node_modules/helper"] = helper;
            }),
    );
    let line = line_starting_with(
        &project,
        "package-lock.json",
        "\"node_modules/once/node_modules/helper\":",
    );
    check_github(&project, &[])
        .code(1)
        .stdout(predicate::str::contains(format!(
            "::error file=package-lock.json,line={line},title=licguard%3A DENY ISC helper@1.0.0::ISC  via app > once > helper\n"
        )));
}

#[test]
fn check_github_omits_the_line_of_an_entry_in_a_minified_lockfile() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    let path = project.path().join("package-lock.json");
    let lockfile: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    fs::write(&path, lockfile.to_string()).unwrap();
    check_github(&in_github_workspace(project), &[])
        .code(1)
        .stdout(predicate::str::starts_with(
            "::error file=package-lock.json,title=licguard%3A DENY ISC once@1.4.0::",
        ));
}

#[test]
fn check_github_points_to_the_first_inventory_source_of_a_package_and_names_them_all() {
    let project = in_github_workspace(
        Project::from_fixture("npm-workspaces").with_policy("[policy]\ndeny = [\"ISC\"]\n"),
    );
    let inherits = line_starting_with(&project, SCRIPTS_LOCKFILE, "\"node_modules/inherits\":");
    let once = line_starting_with(&project, "package-lock.json", "\"node_modules/once\":");
    check_github(&project, &[])
        .code(1)
        .stdout(predicate::str::contains(format!(
            "::error file=tools/scripts/package-lock.json,line={inherits},title=licguard%3A DENY ISC inherits@2.0.4::ISC  via scripts > inherits  in tools/scripts/package-lock.json\n\
             ::error file=package-lock.json,line={once},title=licguard%3A DENY ISC once@1.4.0::ISC  via web > once  in package-lock.json, tools/scripts/package-lock.json\n"
        )));
}

#[test]
fn check_github_escapes_messages_and_properties() {
    let project = in_github_workspace(
        Project::from_fixture("npm-basic")
            .with_policy(&format!(
                "[policy]\nallow = [\"MIT\"]\ndeny = [\"ISC\", \"GPL-3.0-only\"]\n{}",
                clarification("left%\\r\\npad", None, "MIT"),
            ))
            .write("a,b%/package-lock.json", GPL_LOCKFILE)
            .edit_lockfile(|packages| {
                packages["node_modules/once"]["version"] = "1.4.0-a,b:c%d\r\ne".into();
            }),
    );
    check_github(&project, &[])
        .code(1)
        .stdout(predicate::str::contains(
            "::error file=a%2Cb%25/package-lock.json,line=6,title=licguard%3A DENY GPL-3.0-only gpl-lib@1.0.0::GPL-3.0-only  via other > gpl-lib  in a,b%25/package-lock.json\n",
        ))
        .stdout(predicate::str::contains(
            ",title=licguard%3A DENY ISC once@1.4.0-a%2Cb%3Ac%25d%0D%0Ae::ISC  via app > once  in package-lock.json\n",
        ))
        .stdout(predicate::str::contains(
            "::clarification for left%25%0D%0Apad matches no Package\n",
        ));
}

#[test]
fn check_github_annotates_reviews_only_in_strict_mode_with_the_text_format_exit_code() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(REVIEW_ISC));
    check_github(&project, &[])
        .success()
        .stdout("0 deny · 2 review · 4 allow — ✓ Policy respected\n");
    check_github(&project, &["--strict"])
        .code(1)
        .stdout(predicate::str::starts_with(
            "::error file=package-lock.json,line=52,title=licguard%3A REVIEW ISC once@1.4.0::ISC  via app > once\n",
        ))
        .stdout(predicate::str::ends_with(
            "0 deny · 2 review · 4 allow — ✗ Policy violated (exit 1)\n",
        ));
}

#[test]
fn check_github_names_files_from_the_current_directory_or_the_github_workspace() {
    let project = outside_github_actions(Project::from_fixture("npm-basic").with_policy(DENY_ISC));
    let dir = project.path();
    let (parent, name) = (dir.parent().unwrap(), dir.file_name().unwrap());
    let name = name.to_str().unwrap();
    let args = ["--format", "github"];
    let annotation = |file: &str| {
        predicate::str::starts_with(format!(
            "::error file={file},line=52,title=licguard%3A DENY ISC once@1.4.0::"
        ))
    };

    // A relative Project path is kept, since GitHub Actions runs steps from
    // the workspace.
    project
        .run_from(&dir, Path::new("."), "check", &args)
        .code(1)
        .stdout(annotation("package-lock.json"));
    project
        .run_from(parent, Path::new(name), "check", &args)
        .code(1)
        .stdout(annotation(&format!("{name}/package-lock.json")));
    let project = project.with_env("GITHUB_WORKSPACE", parent.to_str().unwrap());
    project
        .run_from(&dir, Path::new("."), "check", &args)
        .code(1)
        .stdout(annotation("package-lock.json"));

    // An absolute one is relative to the workspace when it is under it.
    check_github(&project, &[])
        .code(1)
        .stdout(annotation(&format!("{name}/package-lock.json")));
    let project = project.with_env("GITHUB_WORKSPACE", dir.join("elsewhere").to_str().unwrap());
    let absolute = dir
        .join("package-lock.json")
        .to_str()
        .unwrap()
        .replace('\\', "/")
        .replace(':', "%3A");
    check_github(&project, &[])
        .code(1)
        .stdout(annotation(&absolute));
}

#[test]
fn check_github_appends_violations_and_warnings_to_the_job_summary() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}",
        clarification("left-pad", None, "MIT"),
    )))
    .write("summary.md", "Previous step\n");
    let summary = project.path().join("summary.md");
    let project = project.with_env("GITHUB_STEP_SUMMARY", summary.to_str().unwrap());
    check_github(&project, &[])
        .code(1)
        .stdout(predicate::str::ends_with(
            "::warning file=licguard.toml,title=licguard%3A unmatched_clarification::clarification for left-pad matches no Package\n\
             2 deny · 0 review · 4 allow — ✗ Policy violated (exit 1)\n",
        ));
    assert_eq!(
        fs::read_to_string(&summary).unwrap(),
        "Previous step\n\
         ## licguard\n\
         \n\
         2 deny · 0 review · 4 allow — ✗ Policy violated (exit 1)\n\
         \n\
         | Verdict | License | Package | Via |\n\
         | --- | --- | --- | --- |\n\
         | DENY | `ISC` | `once@1.4.0` | app > once |\n\
         | DENY | `ISC` | `wrappy@1.0.2` | app > once > wrappy |\n\
         \n\
         ### Warnings\n\
         \n\
         - clarification for left-pad matches no Package\n"
    );
}

#[test]
fn check_github_job_summary_says_when_there_is_no_violation() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(REVIEW_ISC));
    let summary = project.path().join("summary.md");
    let project = project.with_env("GITHUB_STEP_SUMMARY", summary.to_str().unwrap());
    check_github(&project, &[]).success();
    assert_eq!(
        fs::read_to_string(&summary).unwrap(),
        "## licguard\n\
         \n\
         0 deny · 2 review · 4 allow — ✓ Policy respected\n\
         \n\
         No Violation.\n"
    );
}

#[test]
fn unwritable_job_summary_is_a_runtime_error() {
    let project = in_github_workspace(Project::from_fixture("npm-basic").with_policy(ALLOW_ALL));
    let summary = project.path().join("missing").join("summary.md");
    let project = project.with_env("GITHUB_STEP_SUMMARY", summary.to_str().unwrap());
    check_github(&project, &[])
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains("cannot write the job summary"))
        .stderr(predicate::str::contains("summary.md"))
        .stderr(predicate::str::contains("hint:"));
}

/// The arguments of a `waive` run that waives every Violation until
/// 2027-01-01.
const WAIVE_ALL: [&str; 5] = [
    "--all-violations",
    "--reason",
    "Accepted at onboarding, ticket LEGAL-7",
    "--expires",
    "2027-01-01",
];

#[test]
fn waive_turns_every_violation_into_a_waiver_so_that_check_passes() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    project.waive_with(&WAIVE_ALL).success().stdout(format!(
        "added 2 Waivers, renewed 0 in {}\n",
        project.path().join("licguard.toml").display()
    ));
    project.check().success();
}

/// A Policy full of comments, with a Waiver and a License clarification, as a
/// Project owner would write it.
const COMMENTED_POLICY: &str = r#"# The license rules of this Project.

[policy]
allow = ["MIT"]   # permissive only
# ISC is denied here to exercise `waive`.
deny = ["ISC"]

# Waivers approved by legal.
[[waivers]]
package = "left-pad"  # removed since, kept for the record
license = "MIT"
reason = "Approved by legal, ticket LEGAL-1"
expires = "2027-01-01"

# Clarifications, with evidence.
[[clarifications]]
package = "zlib"
license = "Zlib"
evidence = "https://example.com/zlib/LICENSE"

# End of the Policy.
"#;

/// The `[[waivers]]` entry that `waive` writes with [`WAIVE_ALL`].
fn waived(package: &str, version: &str, license: &str) -> String {
    format!(
        "\n[[waivers]]\npackage = \"{package}\"\nversion = \"{version}\"\nlicense = \"{license}\"\nreason = \"Accepted at onboarding, ticket LEGAL-7\"\nexpires = \"2027-01-01\"\n"
    )
}

#[test]
fn waive_keeps_the_policy_byte_for_byte_and_adds_the_waivers_after_the_existing_ones() {
    let project = Project::from_fixture("npm-basic").with_policy(COMMENTED_POLICY);
    project.waive_with(&WAIVE_ALL).success();
    let (before, after) = COMMENTED_POLICY.split_once("\n# Clarifications").unwrap();
    assert_eq!(
        project.policy_file(),
        format!(
            "{before}{}{}\n# Clarifications{after}",
            waived("once", "1.4.0", "ISC"),
            waived("wrappy", "1.0.2", "ISC")
        )
    );
}

#[test]
fn waive_writes_the_waivers_sorted_by_package_then_version() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_MIT);
    project.waive_with(&WAIVE_ALL).success();
    assert_eq!(
        project.policy_file(),
        format!(
            "{DENY_MIT}{}{}{}{}",
            waived("@types/ms", "0.7.34", "MIT"),
            waived("debug", "4.3.4", "MIT"),
            waived("ms", "2.1.2", "MIT"),
            waived("ms", "2.1.3", "MIT")
        )
    );
}

#[test]
fn waive_renews_the_expired_waiver_of_a_violation_in_place() {
    let expired = |reason: &str, expires: &str| {
        format!(
            "\n[[waivers]]\npackage = \"once\"\nversion = \"1.4.0\"\nlicense = \"ISC\"\nreason = \"{reason}\"\nexpires = \"{expires}\"  # review yearly\n"
        )
    };
    let project = Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}",
        expired("Approved by legal, ticket LEGAL-1", "2026-01-01")
    ));
    project.waive_with(&WAIVE_ALL).success().stdout(format!(
        "added 1 Waiver, renewed 1 in {}\n",
        project.path().join("licguard.toml").display()
    ));
    assert_eq!(
        project.policy_file(),
        format!(
            "{DENY_ISC}{}{}",
            expired("Accepted at onboarding, ticket LEGAL-7", "2027-01-01"),
            waived("wrappy", "1.0.2", "ISC")
        )
    );
    project.check().success();
}

#[test]
fn waive_renews_the_waiver_of_a_package_whose_license_changed() {
    // The Waiver no longer matches: `once@1.4.0` is ISC, not MIT.
    let project = Project::from_fixture("npm-basic").with_policy(&format!(
        "{DENY_ISC}{}",
        waiver("once", Some("1.4.0"), "MIT")
    ));
    project.waive_with(&WAIVE_ALL).success();
    assert_eq!(
        project.policy_file(),
        format!(
            "{DENY_ISC}{}{}",
            waived("once", "1.4.0", "ISC"),
            waived("wrappy", "1.0.2", "ISC")
        )
    );
    project.check().success();
}

#[test]
fn waive_in_strict_mode_also_waives_reviewed_packages() {
    let project = Project::from_fixture("npm-basic").with_policy(REVIEW_ISC);
    project
        .waive_with(&[&WAIVE_ALL[..], &["--strict"]].concat())
        .success();
    assert_eq!(
        project.policy_file(),
        format!(
            "{REVIEW_ISC}{}{}",
            waived("once", "1.4.0", "ISC"),
            waived("wrappy", "1.0.2", "ISC")
        )
    );
    project.check_with(&["--strict"]).success();
}

#[test]
fn waive_without_violations_leaves_the_policy_untouched() {
    // Reviewed Packages are no Violation without `--strict`.
    let project = Project::from_fixture("npm-basic").with_policy(REVIEW_ISC);
    project
        .waive_with(&WAIVE_ALL)
        .success()
        .stdout("nothing to waive\n");
    assert_eq!(project.policy_file(), REVIEW_ISC);
}

#[test]
fn waive_waives_dev_dependencies_only_when_asked() {
    let project = Project::from_fixture("npm-basic")
        .with_policy(DENY_GPL)
        .edit_lockfile(add_dev_dependency);
    project
        .waive_with(&WAIVE_ALL)
        .success()
        .stdout("nothing to waive\n");
    project
        .waive_with(&[&WAIVE_ALL[..], &["--include-dev"]].concat())
        .success();
    assert_eq!(
        project.policy_file(),
        format!("{DENY_GPL}{}", waived("test-kit", "1.0.0", "GPL-3.0-only"))
    );
    project.check_with(&["--include-dev"]).success();
}

/// Makes `wrappy@1.0.2` Unresolved: no origin declares its license.
fn unresolve_wrappy(project: Project) -> Project {
    project
        .remove("node_modules/wrappy")
        .edit_lockfile(|packages| {
            packages["node_modules/wrappy"]
                .as_object_mut()
                .unwrap()
                .remove("license");
        })
}

#[test]
fn waive_skips_unresolved_violations_and_fails() {
    let project = unresolve_wrappy(Project::from_fixture("npm-basic").with_policy(DENY_ISC));
    project
        .waive_with(&WAIVE_ALL)
        .code(1)
        .stdout(format!(
            "added 1 Waiver, renewed 0 in {}\n",
            project.path().join("licguard.toml").display()
        ))
        .stderr(predicate::str::contains(
            "cannot waive wrappy@1.0.2: its license is Unresolved\n",
        ))
        .stderr(predicate::str::contains("hint:"))
        .stderr(predicate::str::contains("License clarification"));
    assert_eq!(
        project.policy_file(),
        format!("{DENY_ISC}{}", waived("once", "1.4.0", "ISC"))
    );
    project
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    (unresolved)    wrappy@1.0.2",
        ))
        .stdout(predicate::str::contains(
            "1 deny · 0 review · 5 allow (1 waived)\n",
        ));
}

#[test]
fn waive_with_only_unresolved_violations_leaves_the_policy_untouched_and_fails() {
    let project = unresolve_wrappy(Project::from_fixture("npm-basic").with_policy(ALLOW_ALL));
    project
        .waive_with(&WAIVE_ALL)
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains(
            "cannot waive wrappy@1.0.2: its license is Unresolved\n",
        ));
    assert_eq!(project.policy_file(), ALLOW_ALL);
}

/// Runs `waive` with `args` on a Project with Violations, expects it to fail
/// with a runtime error that contains `error` and a hint, and the Policy to
/// be left untouched.
fn assert_waive_rejects(args: &[&str], error: &str) {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    project
        .waive_with(args)
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(error))
        .stderr(predicate::str::contains("hint:"));
    assert_eq!(project.policy_file(), DENY_ISC);
}

#[test]
fn waive_without_all_violations_is_a_runtime_error() {
    assert_waive_rejects(
        &WAIVE_ALL[1..],
        "`--all-violations` is required\nhint: it is the only supported mode",
    );
}

#[test]
fn waive_without_a_reason_is_a_runtime_error() {
    let [all, _, _, expires, date] = WAIVE_ALL;
    assert_waive_rejects(&[all, expires, date], "`--reason` is required");
    for blank in ["", "  \t "] {
        assert_waive_rejects(
            &[all, "--reason", blank, expires, date],
            "`--reason` is blank",
        );
    }
}

#[test]
fn waive_writes_the_reason_trimmed() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    let [all, reason, text, expires, date] = WAIVE_ALL;
    project
        .waive_with(&[all, reason, &format!("  {text}\n"), expires, date])
        .success();
    assert_eq!(
        project.policy_file(),
        format!(
            "{DENY_ISC}{}{}",
            waived("once", "1.4.0", "ISC"),
            waived("wrappy", "1.0.2", "ISC")
        )
    );
}

#[test]
fn waive_without_an_expiry_date_is_a_runtime_error() {
    assert_waive_rejects(&WAIVE_ALL[..3], "`--expires` is required");
}

#[test]
fn waive_expiry_that_is_not_a_calendar_date_is_a_runtime_error() {
    for expires in ["2027-02-29", "2027-1-1", "01/01/2027", "tomorrow", ""] {
        assert_waive_rejects(
            &[&WAIVE_ALL[..4], &[expires]].concat(),
            &format!("`--expires` `{expires}` is not a calendar date written `YYYY-MM-DD`"),
        );
    }
}

#[test]
fn waive_expiry_before_today_is_a_runtime_error() {
    // `TODAY` is 2026-06-01.
    assert_waive_rejects(
        &[&WAIVE_ALL[..4], &["2026-05-31"]].concat(),
        "`--expires` `2026-05-31` is before today (2026-06-01)",
    );
}

#[test]
fn waive_into_an_inline_waivers_array_is_a_runtime_error() {
    let policy = format!("waivers = []\n{DENY_ISC}");
    let project = Project::from_fixture("npm-basic").with_policy(&policy);
    project
        .waive_with(&WAIVE_ALL)
        .code(2)
        .stderr(predicate::str::contains(
            "`waivers` is not written as `[[waivers]]` tables",
        ))
        .stderr(predicate::str::contains("hint:"));
    assert_eq!(project.policy_file(), policy);
}

#[test]
fn waive_expiry_can_be_today() {
    let project = Project::from_fixture("npm-basic").with_policy(DENY_ISC);
    project
        .waive_with(&[&WAIVE_ALL[..4], &[TODAY]].concat())
        .success();
    project.check().success();
}

/// npm-basic with no installed copy, and no license in the lockfile for
/// `ms@2.1.3` and `@types/ms@0.7.34`: only the registry can declare theirs.
fn project_licensed_by_the_registry() -> Project {
    Project::from_fixture("npm-basic")
        .with_policy(DENY_ISC)
        .remove("node_modules")
        .edit_lockfile(|packages| {
            for key in ["node_modules/ms", "node_modules/@types/ms"] {
                packages[key].as_object_mut().unwrap().remove("license");
            }
        })
}

#[test]
fn package_with_no_local_license_gets_its_declared_license_from_the_registry() {
    let json = stdout_json(
        project_licensed_by_the_registry()
            .with_registry_response("/ms/2.1.3", &[(200, r#"{"name":"ms","license":"MIT"}"#)])
            .with_registry_response(
                "/@types%2Fms/0.7.34",
                &[(200, r#"{"name":"@types/ms","license":"ISC"}"#)],
            )
            .list_with(&["--format", "json"])
            .success(),
    );
    let ms = json_package(&json["packages"], "ms", "2.1.3");
    assert_eq!(ms["declared_license"], "MIT");
    assert_eq!(ms["license"], "MIT");
    assert_eq!(ms["origin"], "registry");
    let types = json_package(&json["packages"], "@types/ms", "0.7.34");
    assert_eq!(types["declared_license"], "ISC");
    assert_eq!(types["verdict"], "deny");
    assert_eq!(types["origin"], "registry");
}

#[test]
fn package_the_registry_does_not_know_is_unresolved() {
    let project = project_licensed_by_the_registry()
        .with_registry_response("/ms/2.1.3", &[(200, r#"{"name":"ms"}"#)]);
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    // `@types/ms@0.7.34` gets a 404.
    for (name, version) in [("ms", "2.1.3"), ("@types/ms", "0.7.34")] {
        let package = json_package(&json["packages"], name, version);
        assert_eq!(package["declared_license"], serde_json::Value::Null);
        assert_eq!(package["reason"], "unresolved");
        assert_eq!(package["origin"], serde_json::Value::Null);
    }
    assert_eq!(
        project.registry.paths(),
        ["/@types%2Fms/0.7.34", "/ms/2.1.3"]
    );
}

#[test]
fn registry_legacy_licenses_array_offers_a_choice() {
    let json = stdout_json(
        project_licensed_by_the_registry()
            .with_registry_response(
                "/ms/2.1.3",
                &[(
                    200,
                    r#"{"licenses":[{"type":"ISC","url":"https://example.com"},{"type":"MIT"}]}"#,
                )],
            )
            .list_with(&["--format", "json"])
            .success(),
    );
    let ms = json_package(&json["packages"], "ms", "2.1.3");
    assert_eq!(ms["declared_license"], "ISC OR MIT");
    assert_eq!(ms["elected"], "MIT");
    assert_eq!(ms["verdict"], "allow");
    assert_eq!(ms["origin"], "registry");
}

#[test]
fn clarified_package_is_not_requested_from_the_registry() {
    let project = project_licensed_by_the_registry()
        .with_policy(&format!("{DENY_ISC}{}", clarification("ms", None, "MIT")));
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    assert_eq!(
        json_package(&json["packages"], "ms", "2.1.3")["origin"],
        "clarification"
    );
    assert_eq!(project.registry.paths(), ["/@types%2Fms/0.7.34"]);
}

#[test]
fn each_distinct_package_is_requested_once_dev_ones_included() {
    let remove_license = |key: &'static str| {
        move |packages: &mut serde_json::Value| {
            packages[key].as_object_mut().unwrap().remove("license");
        }
    };
    let project = Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .edit_lockfile(remove_license("node_modules/ms"))
        .edit_lockfile(remove_license("node_modules/@types/ms"))
        .edit_lockfile_in(
            "tools/scripts/package-lock.json",
            remove_license("node_modules/ms"),
        );
    project.check().code(1);
    assert_eq!(
        project.registry.paths(),
        ["/@types%2Fms/0.7.34", "/ms/2.1.3"]
    );
}

#[test]
fn registry_requests_send_only_the_package_and_the_allowed_headers() {
    let project = project_licensed_by_the_registry();
    project.check().code(1);
    let requests = project.registry.requests();
    assert_eq!(requests.len(), 2);
    let user_agent = format!("licguard/{}", env!("CARGO_PKG_VERSION"));
    for request in requests {
        let mut headers = request.headers.clone();
        headers.sort();
        assert_eq!(
            headers,
            [
                ("accept".to_string(), "application/json".to_string()),
                (
                    "host".to_string(),
                    project.registry.url["http://".len()..].to_string()
                ),
                ("user-agent".to_string(), user_agent.clone()),
            ],
            "{}",
            request.path
        );
    }
}

#[test]
fn registry_request_is_retried_until_it_succeeds_on_the_third_attempt() {
    let project = project_licensed_by_the_registry().with_registry_response(
        "/ms/2.1.3",
        &[(503, ""), (429, ""), (200, r#"{"license":"MIT"}"#)],
    );
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    assert_eq!(
        json_package(&json["packages"], "ms", "2.1.3")["license"],
        "MIT"
    );
    let attempts = project
        .registry
        .paths()
        .iter()
        .filter(|p| *p == "/ms/2.1.3")
        .count();
    assert_eq!(attempts, 3);
}

const OFFLINE_HINT: &str =
    "hint: check your network or proxy, or run with --offline to use only local License origins";

#[test]
fn registry_failing_after_the_retries_is_a_runtime_error() {
    let project =
        project_licensed_by_the_registry().with_registry_response("/ms/2.1.3", &[(503, "")]);
    project
        .check()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "cannot fetch the license of ms@2.1.3 from the npm registry {}: status 503",
            project.registry.url
        )))
        .stderr(predicate::str::contains(OFFLINE_HINT));
    let attempts = project
        .registry
        .paths()
        .iter()
        .filter(|p| *p == "/ms/2.1.3")
        .count();
    assert_eq!(attempts, 3);
}

/// The URL of a local port that nothing listens on.
fn closed_port_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

#[test]
fn unreachable_registry_is_a_runtime_error() {
    let url = closed_port_url();
    project_licensed_by_the_registry()
        .with_env("LICGUARD_NPM_REGISTRY", &url)
        .check()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "cannot fetch the license of @types/ms@0.7.34 from the npm registry {url}: "
        )))
        .stderr(predicate::str::contains(OFFLINE_HINT));
}

#[test]
fn registry_request_is_retried_after_a_connection_error() {
    let project = project_licensed_by_the_registry()
        .with_registry_response("/ms/2.1.3", &[(0, ""), (200, r#"{"license":"MIT"}"#)]);
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    assert_eq!(
        json_package(&json["packages"], "ms", "2.1.3")["license"],
        "MIT"
    );
}

#[test]
fn registry_answering_another_status_is_a_runtime_error_without_retry() {
    let project =
        project_licensed_by_the_registry().with_registry_response("/ms/2.1.3", &[(403, "")]);
    project
        .check()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "the npm registry {} answered status 403 for ms@2.1.3",
            project.registry.url
        )))
        .stderr(predicate::str::contains("hint: "));
    let attempts = project
        .registry
        .paths()
        .iter()
        .filter(|p| *p == "/ms/2.1.3")
        .count();
    assert_eq!(attempts, 1);
}

#[test]
fn registry_answering_an_invalid_document_is_a_runtime_error() {
    let project = project_licensed_by_the_registry()
        .with_registry_response("/ms/2.1.3", &[(200, "<html>proxy login</html>")]);
    project
        .check()
        .code(2)
        .stderr(predicate::str::contains(format!(
            "the npm registry {} answered an invalid document for ms@2.1.3",
            project.registry.url
        )))
        .stderr(predicate::str::contains("hint: "));
}

#[test]
fn offline_makes_no_request_and_leaves_packages_unresolved() {
    let project = project_licensed_by_the_registry()
        .with_registry_response("/ms/2.1.3", &[(200, r#"{"license":"MIT"}"#)]);
    project
        .check_with(&["--offline"])
        .code(1)
        .stdout(predicate::str::contains("DENY    (unresolved)    ms@2.1.3"));
    let json = stdout_json(
        project
            .list_with(&["--offline", "--format", "json"])
            .success(),
    );
    let ms = json_package(&json["packages"], "ms", "2.1.3");
    assert_eq!(ms["reason"], "unresolved");
    project
        .waive_with(&[&WAIVE_ALL[..], &["--offline"]].concat())
        .code(1)
        .stderr(predicate::str::contains(
            "cannot waive ms@2.1.3: its license is Unresolved",
        ));
    assert_eq!(project.registry.paths(), Vec::<String>::new());
}

#[test]
fn registry_requests_are_concurrent_up_to_16_in_flight() {
    let project = project_licensed_by_the_registry().edit_lockfile(|packages| {
        for i in 0..40 {
            packages[format!("node_modules/pkg-{i:02}")] =
                serde_json::json!({ "version": "1.0.0" });
        }
    });
    project.registry.delay(Duration::from_millis(50));
    project.check().code(1);
    assert_eq!(project.registry.requests().len(), 42);
    let max = project.registry.max_in_flight();
    assert!((2..=16).contains(&max), "{max} requests in flight");
}

#[test]
fn registry_url_may_end_with_a_slash() {
    let project = project_licensed_by_the_registry()
        .with_registry_response("/ms/2.1.3", &[(200, r#"{"license":"MIT"}"#)]);
    let url = format!("{}/", project.registry.url);
    let json = stdout_json(
        project
            .with_env("LICGUARD_NPM_REGISTRY", &url)
            .list_with(&["--format", "json"])
            .success(),
    );
    assert_eq!(
        json_package(&json["packages"], "ms", "2.1.3")["license"],
        "MIT"
    );
}

#[test]
fn help_documents_the_registry_override_and_offline() {
    for command in ["check", "list", "waive"] {
        Command::cargo_bin("licguard")
            .unwrap()
            .args([command, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains("--offline"))
            .stdout(predicate::str::contains("LICGUARD_NPM_REGISTRY=URL"));
    }
}

#[test]
fn registry_override_that_is_not_an_http_url_is_a_runtime_error() {
    for url in ["", "registry.example.com", "ftp://registry.example.com"] {
        let project = Project::from_fixture("npm-basic")
            .with_policy(ALLOW_ALL)
            .with_env("LICGUARD_NPM_REGISTRY", url);
        project
            .check()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "LICGUARD_NPM_REGISTRY `{url}` is not an http:// or https:// URL"
            )))
            .stderr(predicate::str::contains("hint: "));
        project.check_with(&["--offline"]).success();
    }
}

/// Where a dependency that is not from an npm registry comes from: a git
/// repository, a tarball URL outside the registry layout, a local file.
const GIT_URL: &str =
    "git+ssh://git@github.com/isaacs/once.git#0e614d9f5a7e6f0305c625f6b581f6d80b33b8a6";
const CODELOAD_URL: &str =
    "https://codeload.github.com/isaacs/once/tar.gz/0e614d9f5a7e6f0305c625f6b581f6d80b33b8a6";
const TARBALL_URL: &str = "https://example.com/once-1.4.0.tgz";
const FILE_URL: &str = "file:../once-1.4.0.tgz";

/// Asserts that `once@1.4.0` was not requested and is Unresolved, while the
/// registry Package `wrappy@1.0.2` was requested.
fn assert_once_is_not_from_the_registry(project: &Project, case: &str) {
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    let once = json_package(&json["packages"], "once", "1.4.0");
    assert_eq!(once["reason"], "unresolved", "{case}");
    assert_eq!(once["origin"], serde_json::Value::Null, "{case}");
    let paths = project.registry.paths();
    assert!(
        !paths.contains(&"/once/1.4.0".to_string()),
        "{case}: {paths:?}"
    );
    assert!(
        paths.contains(&"/wrappy/1.0.2".to_string()),
        "{case}: {paths:?}"
    );
}

#[test]
fn npm_package_not_from_a_registry_is_not_requested_and_stays_unresolved() {
    for resolved in [GIT_URL, CODELOAD_URL, TARBALL_URL, FILE_URL] {
        let project = Project::from_fixture("npm-basic")
            .with_policy(ALLOW_ALL)
            .remove("node_modules")
            .edit_lockfile(|packages| {
                for key in ["node_modules/once", "node_modules/wrappy"] {
                    packages[key].as_object_mut().unwrap().remove("license");
                }
                packages["node_modules/once"]["resolved"] = resolved.into();
            });
        assert_once_is_not_from_the_registry(&project, resolved);
    }
}

#[test]
fn npm_package_from_any_registry_or_without_resolved_is_requested() {
    let project = Project::from_fixture("npm-basic")
        .with_policy(ALLOW_ALL)
        .remove("node_modules")
        .edit_lockfile(|packages| {
            for key in ["node_modules/once", "node_modules/wrappy"] {
                packages[key].as_object_mut().unwrap().remove("license");
            }
            packages["node_modules/once"]["resolved"] =
                "https://npm.example.com/repository/npm/once/-/once-1.4.0.tgz".into();
            packages["node_modules/wrappy"]
                .as_object_mut()
                .unwrap()
                .remove("resolved");
        });
    project.check().code(1);
    assert_eq!(project.registry.paths(), ["/once/1.4.0", "/wrappy/1.0.2"]);
}

#[test]
fn yarn_package_not_from_a_registry_is_not_requested_and_stays_unresolved() {
    let v1 = r#"  resolved "https://registry.npmjs.org/once/-/once-1.4.0.tgz#583b1aa775961d4b113ac17d9c50baef9dd76bd1"
"#;
    let berry = "  resolution: \"once@npm:1.4.0\"\n";
    let cases = [
        ("yarn-v1", v1, format!("  resolved \"{GIT_URL}\"\n")),
        ("yarn-v1", v1, format!("  resolved \"{CODELOAD_URL}\"\n")),
        ("yarn-v1", v1, format!("  resolved \"{TARBALL_URL}\"\n")),
        ("yarn-v1", v1, format!("  resolved \"{FILE_URL}\"\n")),
        ("yarn-v1", v1, String::new()),
        (
            "yarn-berry",
            berry,
            "  resolution: \"once@https://github.com/isaacs/once.git#commit=0e614d9\"\n".into(),
        ),
        (
            "yarn-berry",
            berry,
            format!("  resolution: \"once@{TARBALL_URL}\"\n"),
        ),
        (
            "yarn-berry",
            berry,
            "  resolution: \"once@file:../once-1.4.0.tgz::locator=app%40workspace%3A.\"\n".into(),
        ),
    ];
    for (fixture, from, to) in cases {
        let project = Project::from_fixture(fixture)
            .with_policy(ALLOW_ALL)
            .remove("node_modules")
            .replace_in("yarn.lock", from, &to);
        assert_once_is_not_from_the_registry(&project, &format!("{fixture}: {to}"));
    }
}

/// The `resolution` of `once@1.4.0` in the pnpm fixtures.
const PNPM_ONCE_RESOLUTION: &str = "resolution: {integrity: sha512-lNaJgI+2Q5URQBkccEKHTQOPaXdUxnZZElQTZY0MFUAuaEqe1E+Nyvgdz/aIyNi6Z9MzO5dv1H8n58/GELp3+w==}";

#[test]
fn pnpm_package_not_from_a_registry_is_not_requested_and_stays_unresolved() {
    for fixture in ["pnpm-v6", "pnpm-v9"] {
        for resolution in [
            "{type: git, repo: 'https://github.com/isaacs/once.git', commit: 0e614d9}".to_string(),
            format!("{{tarball: '{CODELOAD_URL}'}}"),
            format!("{{integrity: sha512-x, tarball: '{TARBALL_URL}'}}"),
            format!("{{integrity: sha512-x, tarball: '{FILE_URL}'}}"),
        ] {
            let project = Project::from_fixture(fixture)
                .with_policy(ALLOW_ALL)
                .remove("node_modules")
                .replace_in(
                    "pnpm-lock.yaml",
                    PNPM_ONCE_RESOLUTION,
                    &format!("resolution: {resolution}"),
                );
            assert_once_is_not_from_the_registry(&project, &format!("{fixture}: {resolution}"));
        }
    }
}

#[test]
fn pnpm_package_with_a_registry_tarball_is_requested() {
    for fixture in ["pnpm-v6", "pnpm-v9"] {
        let project = Project::from_fixture(fixture)
            .with_policy(ALLOW_ALL)
            .remove("node_modules")
            .replace_in(
                "pnpm-lock.yaml",
                PNPM_ONCE_RESOLUTION,
                "resolution: {integrity: sha512-x, tarball: 'https://npm.example.com/once/-/once-1.4.0.tgz'}",
            );
        project.check().code(1);
        assert!(
            project
                .registry
                .paths()
                .contains(&"/once/1.4.0".to_string()),
            "{fixture}"
        );
    }
}

#[test]
fn package_is_from_the_registry_only_if_every_inventory_source_says_so() {
    let remove_ms_license = |packages: &mut serde_json::Value| {
        packages["node_modules/ms"]
            .as_object_mut()
            .unwrap()
            .remove("license");
    };
    let project = Project::from_fixture("npm-workspaces")
        .with_policy(DENY_ALL)
        .edit_lockfile(remove_ms_license)
        .edit_lockfile_in("tools/scripts/package-lock.json", |packages| {
            remove_ms_license(packages);
            packages["node_modules/ms"]["resolved"] = GIT_URL.into();
        });
    let json = stdout_json(project.list_with(&["--format", "json"]).success());
    assert_eq!(
        json_package(&json["packages"], "ms", "2.1.3")["reason"],
        "unresolved"
    );
    assert_eq!(project.registry.paths(), Vec::<String>::new());
}
