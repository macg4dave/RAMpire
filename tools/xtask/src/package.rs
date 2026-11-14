//! `cargo xtask package`: the host and guest packages from this checkout,
//! the same ones a release ships. It stamps both binaries with the checkout's
//! build identity, builds them, and runs `packaging/build.sh` (nfpm) for the
//! RPMs, `.deb`s and, with `--static`, the tarballs. A development host
//! installs what it builds with its package manager, like any other host.
//!
//! The version is the `rampire` crate's. On a commit tagged `vVERSION` with
//! a clean tree it is exactly that; otherwise `VERSION~dev.BUILD`, which
//! sorts before the release it leads to.

use std::path::Path;
use std::process::Command;

use rampire_ops::process;

pub const HELP: &str = "Packages:
  cargo xtask package [--output DIR] [--static]
  cargo xtask package --print-version

  Builds the host and guest packages (RPM and .deb, nfpm) from this
  checkout into DIR (default dist), stamped with its build identity.
  --static also builds the musl tarballs. --print-version prints the
  version the packages would get and builds nothing.
";

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Build {
        output: String,
        static_tarballs: bool,
    },
    PrintVersion,
}

pub fn parse(arguments: &[String]) -> Result<Action, String> {
    let mut output = None;
    let mut static_tarballs = false;
    let mut print_version = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--output" if output.is_none() => {
                index += 1;
                output = Some(
                    arguments
                        .get(index)
                        .filter(|value| !value.trim().is_empty())
                        .ok_or("--output requires a directory")?
                        .clone(),
                );
            }
            "--static" if !static_tarballs => static_tarballs = true,
            "--print-version" if !print_version => print_version = true,
            other => return Err(format!("package: unexpected argument {other}\n\n{HELP}")),
        }
        index += 1;
    }
    if print_version {
        if output.is_some() || static_tarballs {
            return Err("--print-version takes no other option".to_owned());
        }
        return Ok(Action::PrintVersion);
    }
    Ok(Action::Build {
        output: output.unwrap_or_else(|| "dist".to_owned()),
        static_tarballs,
    })
}

pub fn run(action: &Action, repo: &Path) -> Result<(), String> {
    let identity = crate::windows::checkout_identity(repo)?;
    let version = version(
        crate_version(repo)?.as_str(),
        exact_tag(repo).as_deref(),
        &identity.build_id,
        identity.dirty,
    )?;
    let (output, static_tarballs) = match action {
        Action::PrintVersion => {
            println!("{version}");
            return Ok(());
        }
        Action::Build {
            output,
            static_tarballs,
        } => (output, *static_tarballs),
    };
    let mut targets = vec![None];
    if static_tarballs {
        targets.push(Some("x86_64-unknown-linux-musl"));
    }
    for target in targets {
        let mut cargo = Command::new("cargo");
        cargo.args([
            "build",
            "--release",
            "--locked",
            "-p",
            "rampire",
            "-p",
            "rampire-guest-linux",
        ]);
        if let Some(target) = target {
            cargo.args(["--target", target]);
        }
        run_status(
            cargo
                .env("RAMPIRE_BUILD_ID", &identity.build_id)
                .current_dir(repo),
            "cargo build",
        )?;
    }
    run_status(
        Command::new("bash")
            .arg("packaging/build.sh")
            .arg(&version)
            .arg(output)
            .current_dir(repo),
        "packaging/build.sh",
    )?;
    println!(
        "Built version {version} (build {}) in {output}",
        identity.build_id
    );
    Ok(())
}

fn run_status(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("run {what}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed: {status}"))
    }
}

/// The `rampire` crate's version, from its manifest.
fn crate_version(repo: &Path) -> Result<String, String> {
    let manifest = std::fs::read_to_string(repo.join("rampire/Cargo.toml"))
        .map_err(|error| format!("read rampire/Cargo.toml: {error}"))?;
    manifest
        .lines()
        .find_map(|line| {
            let value = line
                .trim()
                .strip_prefix("version")?
                .trim()
                .strip_prefix('=')?;
            Some(value.trim().trim_matches('"').to_owned())
        })
        .ok_or_else(|| "rampire/Cargo.toml has no version".to_owned())
}

/// The tag on HEAD, if there is exactly one.
fn exact_tag(repo: &Path) -> Option<String> {
    process::checked_text(
        "git",
        &[
            process::os("describe"),
            process::os("--exact-match"),
            process::os("--tags"),
            process::os("HEAD"),
        ],
        repo,
    )
    .ok()
    .map(|tag| tag.trim().to_owned())
    .filter(|tag| !tag.is_empty())
}

/// The package version: the release on a clean commit tagged for it, a
/// development version sorting before it otherwise. A release tag that
/// doesn't match the crate is refused.
fn version(
    crate_version: &str,
    tag: Option<&str>,
    build_id: &str,
    dirty: bool,
) -> Result<String, String> {
    if let Some(tag) = tag.filter(|tag| tag.starts_with('v')) {
        if &tag[1..] != crate_version {
            return Err(format!(
                "HEAD is tagged {tag} but rampire is {crate_version}"
            ));
        }
        if !dirty {
            return Ok(crate_version.to_owned());
        }
    }
    // '-' separates the Debian revision and the RPM release, so a dirty
    // build's "-dirty." becomes ".dirty.".
    Ok(format!(
        "{crate_version}~dev.{}",
        build_id.replace('-', ".")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn options_are_parsed() {
        assert_eq!(
            parse(&[]),
            Ok(Action::Build {
                output: "dist".to_owned(),
                static_tarballs: false
            })
        );
        assert_eq!(
            parse(&args(&["--output", "out", "--static"])),
            Ok(Action::Build {
                output: "out".to_owned(),
                static_tarballs: true
            })
        );
        assert_eq!(parse(&args(&["--print-version"])), Ok(Action::PrintVersion));
        assert!(parse(&args(&["--print-version", "--static"])).is_err());
        assert!(parse(&args(&["--output"])).is_err());
        assert!(parse(&args(&["--bogus"])).is_err());
    }

    #[test]
    fn a_clean_tagged_commit_is_the_release() {
        assert_eq!(
            version("0.1.0", Some("v0.1.0"), "abc123def456", false),
            Ok("0.1.0".to_owned())
        );
    }

    #[test]
    fn anything_else_is_a_development_version() {
        assert_eq!(
            version("0.1.0", None, "abc123def456", false),
            Ok("0.1.0~dev.abc123def456".to_owned())
        );
        assert_eq!(
            version(
                "0.1.0",
                Some("v0.1.0"),
                "abc123def456-dirty.0123456789ab",
                true
            ),
            Ok("0.1.0~dev.abc123def456.dirty.0123456789ab".to_owned())
        );
        assert_eq!(
            version("0.1.0", Some("other"), "abc123def456", false),
            Ok("0.1.0~dev.abc123def456".to_owned())
        );
    }

    #[test]
    fn a_release_tag_for_another_version_is_refused() {
        assert!(version("0.1.0", Some("v0.2.0"), "abc123def456", false).is_err());
    }
}
