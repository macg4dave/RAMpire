//! Identity of a build: the checkout's commit, whether the working tree
//! differs from it, and a digest of every file it would build from.
//! `cargo xtask package` stamps it into both binaries (`rampire --version`).

use std::path::Path;

use rampire_ops::process;

const SHORT_COMMIT_CHARS: usize = 12;
const SHORT_DIGEST_CHARS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    pub build_id: String,
    pub dirty: bool,
}

impl BuildIdentity {
    /// A clean tree is identified by its commit; a dirty tree also by the
    /// digest of the exact files built.
    pub fn new(commit: &str, dirty: bool, source_sha256: &str) -> Result<Self, String> {
        if !is_hex(commit, 40) {
            return Err(format!("expected a full Git commit ID, got {commit:?}"));
        }
        if !is_hex(source_sha256, 64) {
            return Err("source digest must be a SHA-256 value".to_owned());
        }
        let short_commit = &commit[..SHORT_COMMIT_CHARS];
        let build_id = if dirty {
            format!(
                "{short_commit}-dirty.{}",
                &source_sha256[..SHORT_DIGEST_CHARS]
            )
        } else {
            short_commit.to_owned()
        };
        Ok(Self { build_id, dirty })
    }
}

/// The build identity of this checkout as it stands: every tracked and
/// untracked, unignored file that exists.
pub fn checkout_identity(repo: &Path) -> Result<BuildIdentity, String> {
    let listed = process::checked_output(
        "git",
        &[
            process::os("ls-files"),
            process::os("--cached"),
            process::os("--others"),
            process::os("--exclude-standard"),
            process::os("-z"),
        ],
        repo,
    )?;
    let paths = existing_paths(repo, &listed)?;
    let commit = process::checked_text(
        "git",
        &[process::os("rev-parse"), process::os("HEAD")],
        repo,
    )?;
    let status = process::checked_output(
        "git",
        &[
            process::os("status"),
            process::os("--porcelain=v1"),
            process::os("--untracked-files=normal"),
        ],
        repo,
    )?;
    let mut args = vec![process::os("--")];
    args.extend(paths.iter().map(process::os));
    let manifest = process::checked_output("sha256sum", &args, repo)?;
    let manifest_path =
        std::env::temp_dir().join(format!("rampire-source-{}.sha256", std::process::id()));
    std::fs::write(&manifest_path, manifest)
        .map_err(|error| format!("write source manifest: {error}"))?;
    let digest = sha256(repo, &manifest_path);
    // Best effort: the manifest is a derived temporary and the digest is
    // already computed (or its error is returned below).
    let _ = std::fs::remove_file(&manifest_path);
    BuildIdentity::new(commit.trim(), !status.is_empty(), &digest?)
}

/// The paths Git listed that exist (a deleted, uncommitted file is listed
/// but can't be hashed).
fn existing_paths(repo: &Path, nul_separated: &[u8]) -> Result<Vec<String>, String> {
    let input = std::str::from_utf8(nul_separated)
        .map_err(|error| format!("git returned a non-UTF-8 path: {error}"))?;
    let paths: Vec<String> = input
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| std::fs::symlink_metadata(repo.join(path)).is_ok())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return Err("Git did not report any existing source files".to_owned());
    }
    Ok(paths)
}

fn sha256(repo: &Path, path: &Path) -> Result<String, String> {
    let output = process::checked_text("sha256sum", &[path.as_os_str().to_owned()], repo)?;
    output
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64)
        .map(str::to_owned)
        .ok_or_else(|| "sha256sum returned malformed output".to_owned())
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .chars()
            .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "c211d98b6ab8c29fb2a2df0f3c3da846538f144c";
    const DIGEST: &str = "1a2b3c4d5e6f1a2b3c4d5e6f1a2b3c4d5e6f1a2b3c4d5e6f1a2b3c4d5e6f1a2b";

    #[test]
    fn build_id_names_the_commit_and_marks_uncommitted_edits() {
        assert_eq!(
            BuildIdentity::new(COMMIT, false, DIGEST)
                .expect("clean")
                .build_id,
            "c211d98b6ab8"
        );
        assert_eq!(
            BuildIdentity::new(COMMIT, true, DIGEST)
                .expect("dirty")
                .build_id,
            "c211d98b6ab8-dirty.1a2b3c4d5e6f"
        );
        assert!(BuildIdentity::new("c211d98", false, DIGEST).is_err());
        assert!(BuildIdentity::new(COMMIT, false, "not-a-digest").is_err());
    }
}
