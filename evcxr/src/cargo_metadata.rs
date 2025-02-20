// Copyright 2020 The Evcxr Authors.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{bail, Context, Result};
use json::{self, JsonValue};
use once_cell::sync::OnceCell;
use regex::Regex;
use std::collections::HashMap;

use crate::eval_context::Config;

/// Returns the library names for the direct dependencies of the crate rooted at
/// the specified path.
pub(crate) fn get_library_names(config: &Config) -> Result<Vec<String>> {
    let output = config
        .cargo_command("metadata")
        .arg("--format-version")
        .arg("1")
        .output()
        .with_context(|| "Error running cargo metadata")?;
    if output.status.success() {
        library_names_from_metadata(std::str::from_utf8(&output.stdout)?)
    } else {
        bail!(
            "cargo metadata failed with output:\n{}{}",
            std::str::from_utf8(&output.stdout)?,
            std::str::from_utf8(&output.stderr)?,
        )
    }
}

pub(crate) fn validate_dep(dep: &str, dep_config: &str, config: &Config) -> Result<()> {
    std::fs::write(
        config.crate_dir.join("Cargo.toml"),
        format!(
            r#"
    [package]
    name = "evcxr_dummy_validate_dep"
    version = "0.0.1"
    edition = "2021"

    [lib]
    path = "lib.rs"

    [dependencies]
    {} = {}
    "#,
            dep, dep_config
        ),
    )?;
    let mut cmd = config.cargo_command("metadata");
    let output = cmd.arg("-q").arg("--format-version=1").output()?;
    if output.status.success() {
        Ok(())
    } else {
        static IGNORED_LINES_PATTERN: OnceCell<Regex> = OnceCell::new();
        let ignored_lines_pattern = IGNORED_LINES_PATTERN
            .get_or_init(|| Regex::new("required by package `evcxr_dummy_validate_dep.*").unwrap());
        static PRIMARY_ERROR_PATTERN: OnceCell<Regex> = OnceCell::new();
        let primary_error_pattern = PRIMARY_ERROR_PATTERN
            .get_or_init(|| Regex::new("(.*) as a dependency of package `[^`]*`").unwrap());
        let mut message = Vec::new();
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if let Some(captures) = primary_error_pattern.captures(line) {
                message.push(captures[1].to_string());
            } else if !ignored_lines_pattern.is_match(line) {
                message.push(line.to_owned());
            }
        }
        bail!(message.join("\n"));
    }
}

fn library_names_from_metadata(metadata: &str) -> Result<Vec<String>> {
    let metadata = json::parse(metadata)?;
    let mut direct_dependencies = Vec::new();
    let mut crate_to_library_names = HashMap::new();
    if let (JsonValue::Array(packages), Some(main_crate_id)) = (
        &metadata["packages"],
        metadata["workspace_members"][0].as_str(),
    ) {
        for package in packages {
            if let (Some(package_name), Some(id)) =
                (package["name"].as_str(), package["id"].as_str())
            {
                if id == main_crate_id {
                    if let JsonValue::Array(dependencies) = &package["dependencies"] {
                        for dependency in dependencies {
                            if let Some(dependency_name) = dependency["name"].as_str() {
                                direct_dependencies.push(dependency_name);
                            }
                        }
                    }
                }
                if let JsonValue::Array(targets) = &package["targets"] {
                    for target in targets {
                        if let JsonValue::Array(kinds) = &target["kind"] {
                            if kinds.iter().any(|kind| kind == "lib") {
                                if let Some(target_name) = target["name"].as_str() {
                                    crate_to_library_names
                                        .insert(package_name, target_name.to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut library_names = Vec::new();
    for dep_name in direct_dependencies {
        if let Some(lib_name) = crate_to_library_names.get(dep_name) {
            library_names.push(lib_name.replace('-', "_"));
        }
    }
    Ok(library_names)
}

#[cfg(test)]
mod tests {
    use crate::eval_context::Config;

    use super::{get_library_names, library_names_from_metadata};
    use anyhow::Result;
    use std::path::Path;
    use tempfile;

    #[test]
    fn test_library_names_from_metadata() {
        assert_eq!(
            library_names_from_metadata(include_str!("testdata/sample_metadata.json")).unwrap(),
            vec!["crate1"]
        );
    }

    fn create_crate(path: &Path, name: &str, deps: &str) -> Result<()> {
        let src_dir = path.join("src");
        std::fs::create_dir_all(&src_dir)?;
        std::fs::write(
            path.join("Cargo.toml"),
            format!(
                r#"
            [package]
            name = "{}"
            version = "0.0.1"
            edition = "2021"

            [dependencies]
            {}
        "#,
                name, deps
            ),
        )?;
        std::fs::write(src_dir.join("lib.rs"), "")?;
        Ok(())
    }

    fn path_to_string(path: &Path) -> String {
        path.to_string_lossy().replace("\\", "\\\\")
    }

    #[test]
    fn valid_dependency() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let crate1 = tempdir.path().join("crate1");
        let crate2 = tempdir.path().join("crate2");
        create_crate(&crate1, "crate1", "")?;
        create_crate(
            &crate2,
            "crate2",
            &format!(r#"crate1 = {{ path = "{}" }}"#, path_to_string(&crate1)),
        )?;
        assert_eq!(
            get_library_names(&Config::new(crate2.to_owned())).unwrap(),
            vec!["crate1".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn invalid_feature() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let crate1 = tempdir.path().join("crate1");
        let crate2 = tempdir.path().join("crate2");
        create_crate(&crate1, "crate1", "")?;
        create_crate(
            &crate2,
            "crate2",
            &format!(
                r#"crate1 = {{ path = "{}", features = ["no_such_feature"] }}"#,
                path_to_string(&crate1)
            ),
        )?;
        // Make sure that the problematic feature "no_such_feature" is mentioned
        // somewhere in the error message.
        assert!(get_library_names(&Config::new(crate2.to_owned()))
            .unwrap_err()
            .to_string()
            .contains("no_such_feature"));
        Ok(())
    }
}
