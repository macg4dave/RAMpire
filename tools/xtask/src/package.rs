//! `cargo xtask package`: the host and guest packages from this checkout,
//! the same ones a release ships. It stamps both binaries with the checkout's
//! build identity, builds them, and runs `packaging/build.sh` (nfpm) for the
//! RPMs, `.deb`s and, with `--static`, the tarballs. A development host
//! installs what it builds with its package manager, like any other host.
//!
//! The version is the `rampire` crate's. On a commit tagged `vVERSION` with
//! a clean tree it is exactly that; otherwise `VERSION~dev.COUNT.BUILD`,
//! which sorts before the release it leads to. COUNT is the number of
//! commits to HEAD, so a later commit's package is always the newer one and
//! a package manager upgrades to it (a commit hash alone sorts at random).

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
        commit_count(repo)?,
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
    clear_old_packages(&repo.join(output))?;
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

/// Remove packages and checksums an earlier build left in `output`, so it
/// holds one version and SHA256SUMS lists only it. Other files stay.
fn clear_old_packages(output: &Path) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(output) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_build_output(&name) {
            std::fs::remove_file(entry.path())
                .map_err(|error| format!("remove {}: {error}", entry.path().display()))?;
        }
    }
    Ok(())
}

fn is_build_output(name: &str) -> bool {
    name == "SHA256SUMS"
        || (name.starts_with("rampire")
            && [".rpm", ".deb", ".tar.gz"]
                .iter()
                .any(|suffix| name.ends_with(suffix)))
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

/// The number of commits to HEAD.
fn commit_count(repo: &Path) -> Result<u64, String> {
    let text = process::checked_text(
        "git",
        &[
            process::os("rev-list"),
            process::os("--count"),
            process::os("HEAD"),
        ],
        repo,
    )?;
    text.trim()
        .parse()
        .map_err(|_| format!("git rev-list --count gave {text:?}"))
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
    count: u64,
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
        "{crate_version}~dev.{count}.{}",
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
            version("0.1.0", Some("v0.1.0"), 42, "abc123def456", false),
            Ok("0.1.0".to_owned())
        );
    }

    #[test]
    fn anything_else_is_a_development_version() {
        assert_eq!(
            version("0.1.0", None, 42, "abc123def456", false),
            Ok("0.1.0~dev.42.abc123def456".to_owned())
        );
        assert_eq!(
            version(
                "0.1.0",
                Some("v0.1.0"),
                42,
                "abc123def456-dirty.0123456789ab",
                true
            ),
            Ok("0.1.0~dev.42.abc123def456.dirty.0123456789ab".to_owned())
        );
        assert_eq!(
            version("0.1.0", Some("other"), 42, "abc123def456", false),
            Ok("0.1.0~dev.42.abc123def456".to_owned())
        );
    }

    /// RPM and dpkg compare `~dev.42.fff` against `~dev.43.000` by the
    /// count first, so a later commit is the newer package whatever its
    /// hash.
    #[test]
    fn a_later_commit_sorts_after_an_earlier_one() {
        let earlier = version("0.1.0", None, 42, "ffffffffffff", false).unwrap();
        let later = version("0.1.0", None, 43, "000000000000", false).unwrap();
        let segment = |text: &str| -> u64 { text.split('.').nth(3).unwrap().parse().unwrap() };
        assert!(segment(&later) > segment(&earlier), "{earlier} {later}");
    }

    #[test]
    fn only_earlier_build_output_is_cleared() {
        for name in [
            "SHA256SUMS",
            "rampire_0.1.0~dev.abc_amd64.deb",
            "rampire-guest-0.1.0-1.x86_64.rpm",
            "rampire-0.1.0-x86_64-linux.tar.gz",
        ] {
            assert!(is_build_output(name), "{name}");
        }
        for name in ["rampire-0.1.0.src.rpm.asc", "notes.txt", "other.deb"] {
            assert!(!is_build_output(name), "{name}");
        }
    }

    #[test]
    fn a_release_tag_for_another_version_is_refused() {
        assert!(version("0.1.0", Some("v0.2.0"), 42, "abc123def456", false).is_err());
    }
}
