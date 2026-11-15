//! The licenses of the third-party crates built into each binary the
//! packages install. MIT, Apache-2.0 and BSD licenses ask that their
//! copyright and license notices go with the binary, so each package
//! installs one file holding every crate's notice, made at build time from
//! `cargo metadata` and the license files the crates ship.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The binaries' crates, and the file written for each.
pub const BINARIES: [(&str, &str); 2] = [
    ("rampire", "rampire.THIRD_PARTY_LICENSES"),
    ("rampire-guest-linux", "rampire-guest.THIRD_PARTY_LICENSES"),
];

/// Code copied into the repository from other projects, by the binary it
/// is built into: the project, where its code is, and its license text with
/// the project's copyright notice. THIRD_PARTY_NOTICES.md records each.
const COPIED: [(&str, &str, &str); 1] = [(
    "rampire",
    "Yazi, https://github.com/sxyazi/yazi, at commit d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: its notifications, adapted in crates/rampire-ops/src/tui/notify.rs",
    "LICENSES/MIT.txt",
)];

/// The platform the packages are built for: only its dependencies count.
const PLATFORM: &str = "x86_64-unknown-linux-gnu";

/// A crate built into a binary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Crate {
    name: String,
    version: String,
    license: String,
    repository: String,
    directory: PathBuf,
}

/// Write every binary's file into `output`.
pub fn write(repo: &Path, output: &Path) -> Result<(), String> {
    let metadata = metadata(repo)?;
    std::fs::create_dir_all(output)
        .map_err(|error| format!("create {}: {error}", output.display()))?;
    for (root, file) in BINARIES {
        let crates = dependencies(&metadata, root)?;
        let mut missing = Vec::new();
        let mut text = render(root, &crates, &mut missing)?;
        text.push_str(&copied(repo, root)?);
        if !missing.is_empty() {
            return Err(format!(
                "these crates ship no license file, so their notices can't be included: {}",
                missing.join(", ")
            ));
        }
        let path = output.join(file);
        std::fs::write(&path, text)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        println!("wrote {} ({} crates)", path.display(), crates.len());
    }
    Ok(())
}

fn metadata(repo: &Path) -> Result<Value, String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            PLATFORM,
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("read cargo metadata: {error}"))
}

/// The third-party crates `root` is built from: its normal dependencies,
/// and theirs, not build scripts' or tests'.
fn dependencies(metadata: &Value, root: &str) -> Result<Vec<Crate>, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?;
    let by_id: BTreeMap<&str, &Value> = packages
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package)))
        .collect();
    let workspace: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();
    let start = packages
        .iter()
        .find(|package| {
            package["name"] == root
                && package["id"]
                    .as_str()
                    .is_some_and(|id| workspace.contains(id))
        })
        .and_then(|package| package["id"].as_str())
        .ok_or_else(|| format!("{root} is not in the workspace"))?;
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|kind| kind["kind"].is_null());
            if let (true, Some(pkg)) = (normal, dep["pkg"].as_str()) {
                stack.push(pkg);
            }
        }
    }
    let mut crates: Vec<Crate> = seen
        .into_iter()
        .filter(|id| !workspace.contains(id))
        .filter_map(|id| by_id.get(id))
        .map(|package| Crate {
            name: package["name"].as_str().unwrap_or("?").to_owned(),
            version: package["version"].as_str().unwrap_or("?").to_owned(),
            license: package["license"]
                .as_str()
                .unwrap_or("see the license file")
                .to_owned(),
            repository: package["repository"].as_str().unwrap_or("").to_owned(),
            directory: Path::new(package["manifest_path"].as_str().unwrap_or(""))
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
        })
        .collect();
    crates.sort();
    Ok(crates)
}

/// A crate's own license and notice files, by name.
fn license_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(is_license_name)
        })
        .collect();
    files.sort();
    files
}

fn is_license_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "COPYRIGHT",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// The notices of code copied into `root`'s source, each with its license.
fn copied(repo: &Path, root: &str) -> Result<String, String> {
    let mut text = String::new();
    for (binary, what, license) in COPIED {
        if binary != root {
            continue;
        }
        let path = repo.join(license);
        let content = std::fs::read_to_string(&path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        text.push_str(&format!(
            "\n{}\nCode copied into {root}: {what}\n{}\n\n{}\n",
            "=".repeat(78),
            "=".repeat(78),
            content.trim_end()
        ));
    }
    Ok(text)
}

fn render(root: &str, crates: &[Crate], missing: &mut Vec<String>) -> Result<String, String> {
    let mut text = format!(
        "Third-party software built into {root}\n\n\
         {root} is GPL-3.0-or-later. It is built with the crates below, each under\n\
         its own license, whose notices follow in full.\n\n"
    );
    for krate in crates {
        text.push_str(&format!(
            "  {} {}  {}  {}\n",
            krate.name, krate.version, krate.license, krate.repository
        ));
    }
    for krate in crates {
        let files = license_files(&krate.directory);
        if files.is_empty() {
            missing.push(format!("{} {}", krate.name, krate.version));
            continue;
        }
        text.push_str(&format!(
            "\n{}\n{} {} ({})\n{}\n",
            "=".repeat(78),
            krate.name,
            krate.version,
            krate.license,
            "=".repeat(78)
        ));
        for file in files {
            let content = std::fs::read_to_string(&file)
                .map_err(|error| format!("read {}: {error}", file.display()))?;
            text.push_str(&format!(
                "\n--- {} ---\n\n{}\n",
                file.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?"),
                content.trim_end()
            ));
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_code_carries_its_projects_notice() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let host = copied(&repo, "rampire").expect("the host's copied code");
        assert!(host.contains("Copyright (c) 2023 - sxyazi"), "{host}");
        assert!(
            host.contains("crates/rampire-ops/src/tui/notify.rs"),
            "{host}"
        );
        assert_eq!(copied(&repo, "rampire-guest-linux").unwrap(), "");
        // Every file named here says where it came from.
        let notify =
            std::fs::read_to_string(repo.join("crates/rampire-ops/src/tui/notify.rs")).unwrap();
        assert!(notify.starts_with("// Copyright (c) 2023 - sxyazi"));
        assert!(notify.contains("SPDX-License-Identifier: MIT"));
    }

    #[test]
    fn license_files_are_found_by_their_usual_names() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "license.txt",
            "COPYING",
            "NOTICE",
            "UNLICENSE",
        ] {
            assert!(is_license_name(name), "{name}");
        }
        for name in ["README.md", "Cargo.toml", "src"] {
            assert!(!is_license_name(name), "{name}");
        }
    }

    #[test]
    fn the_host_binary_includes_its_terminal_crates_and_not_the_workspace() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let metadata = metadata(&repo).expect("cargo metadata");
        let crates = dependencies(&metadata, "rampire").expect("rampire's crates");
        let names: BTreeSet<&str> = crates.iter().map(|krate| krate.name.as_str()).collect();
        for expected in ["ratatui", "crossterm", "clap", "serde"] {
            assert!(names.contains(expected), "{expected}");
        }
        assert!(!names.contains("rampire-ops"));
        let guest = dependencies(&metadata, "rampire-guest-linux").expect("the guest's crates");
        assert!(!guest.iter().any(|krate| krate.name == "ratatui"));
    }
}
