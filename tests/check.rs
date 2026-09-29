use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// A copy of a fixture Project in a temporary directory, so each test can
/// write its own Policy or alter the installed packages.
struct Project {
    dir: TempDir,
}

impl Project {
    fn from_fixture(name: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        copy_dir(&fixture, dir.path());
        Project { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn with_policy(self, toml: &str) -> Self {
        fs::write(self.dir.path().join("licguard.toml"), toml).unwrap();
        self
    }

    fn replace_in(self, file: &str, from: &str, to: &str) -> Self {
        let path = self.dir.path().join(file);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{file} does not contain {from}");
        fs::write(path, text.replacen(from, to, 1)).unwrap();
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
        let path = self.dir.path().join("package-lock.json");
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
        Command::cargo_bin("licguard")
            .unwrap()
            .arg("check")
            .arg(self.path())
            .args(args)
            .assert()
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
            "DENY    ISC             once@1.4.0",
            "DENY    ISC             wrappy@1.0.2",
            "REVIEW  MIT             @types/ms@0.7.34  (unlisted)",
            "REVIEW  MIT             debug@4.3.4  (unlisted)",
            "REVIEW  MIT             ms@2.1.2  (unlisted)",
            "REVIEW  MIT             ms@2.1.3  (unlisted)",
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
        .stderr(predicate::str::contains("package-lock.json"))
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
            "REVIEW  ISC             once@1.4.0\n",
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
            "REVIEW  ISC             once@1.4.0\n",
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
            "REVIEW  ISC             once@1.4.0  (unlisted)\n",
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
            "REVIEW  MPL-2.0         ms@2.1.3  (elected from GPL-3.0-only OR MPL-2.0)\n",
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
            "REVIEW  EPL-2.0         ms@2.1.3  (elected from EPL-2.0 OR MPL-2.0)\n",
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
            "DENY    GPL-2.0-only WITH Classpath-exception-2.0 ms@2.1.3\n",
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
            "DENY    GPL-2.0-or-later ms@2.1.3\n",
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
            "DENY    Apache-2.0+     ms@2.1.3\n",
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
            deny = ["GPL-3.0"]
            "#,
        )
        .declare_ms_license("GPL-3.0")
        .check()
        .code(1)
        .stdout(predicate::str::contains(
            "DENY    GPL-3.0         ms@2.1.3\n",
        ));
}
