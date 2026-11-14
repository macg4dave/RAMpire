//! Bounded reads of the files RAMpire's formats are kept in: settings,
//! attestations, publications and the kernel's text files. A file larger
//! than its format allows is an error, never a truncated read.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The file's contents, `None` when it does not exist. More than
/// `maximum_bytes` is an error rather than a truncated read.
pub fn read_bounded(path: &Path, maximum_bytes: usize) -> std::io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(maximum_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file exceeds {maximum_bytes} bytes"),
        ));
    }
    Ok(Some(bytes))
}

/// A UTF-8 file that must exist, with `path` in every error.
pub fn read_bounded_text(path: &Path, maximum_bytes: usize) -> Result<String, String> {
    let bytes = read_bounded(path, maximum_bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?
        .ok_or_else(|| format!("read {}: no such file", path.display()))?;
    String::from_utf8(bytes).map_err(|error| format!("read {} as UTF-8: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bounded_and_a_missing_file_is_none() {
        let directory =
            std::env::temp_dir().join(format!("rampire-core-files-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("state");
        assert_eq!(read_bounded(&path, 16).unwrap(), None);
        assert!(read_bounded_text(&path, 16)
            .unwrap_err()
            .contains("no such file"));
        std::fs::write(&path, b"second").unwrap();
        assert_eq!(read_bounded(&path, 6).unwrap().unwrap(), b"second");
        assert_eq!(read_bounded_text(&path, 6).unwrap(), "second");
        assert!(read_bounded(&path, 5).is_err());
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_bounded_text(&path, 6).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
