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
