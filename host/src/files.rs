//! Whole-file replacement for the files the host writes: the control
//! checkpoint, the guest publication copies, metrics and attestations.
//! The guest's control files have their own, with Windows' sharing rules,
//! in the shared guest crate.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replace `path` with `bytes` through a temporary file beside it and a
/// rename, so a reader sees the old or the new contents whole, never part.
/// `mode` sets the new file's Unix permissions where the service's umask
/// would make it private. `durable` also flushes the data and the rename,
/// for state that must survive a crash. A failed replacement removes its
/// temporary file; the parent directory must exist.
pub fn replace_file(path: &Path, bytes: &[u8], mode: Option<u32>, durable: bool) -> io::Result<()> {
    let temporary = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    let result = write_then_rename(&temporary, path, bytes, mode, durable);
    if result.is_err() {
        // Best effort: the replacement error is what is reported.
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_then_rename(
    temporary: &Path,
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    durable: bool,
) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(temporary)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    if durable {
        file.sync_all()?;
    }
    drop(file);
    fs::rename(temporary, path)?;
    #[cfg(unix)]
    if durable {
        // The rename itself is durable only once its directory is.
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("rampire-files-{name}-{}", std::process::id()));
        // Best effort: a leftover from an earlier run is replaced.
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn replaces_the_whole_file_and_leaves_no_temporary() {
        let directory = directory("replace");
        let path = directory.join("state.json");
        for durable in [false, true] {
            fs::write(&path, b"old contents").unwrap();
            replace_file(&path, b"new", None, durable).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new");
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        }
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_failed_replacement_reports_and_cleans_up() {
        let directory = directory("failed");
        // A directory can't be renamed over by a file.
        let path = directory.join("taken");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), b"kept").unwrap();
        assert!(replace_file(&path, b"new", None, false).is_err());
        assert_eq!(fs::read(path.join("inside")).unwrap(), b"kept");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(&directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sets_the_requested_mode() {
        use std::os::unix::fs::PermissionsExt;
        let directory = directory("mode");
        let path = directory.join("shared");
        replace_file(&path, b"x", Some(0o640), false).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
        fs::remove_dir_all(&directory).unwrap();
    }
}
