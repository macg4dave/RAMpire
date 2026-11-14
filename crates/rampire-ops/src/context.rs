//! What every operator command and the TUI share: which configuration they
//! work on, which libvirt connection they use, and the build they are.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rampire_host::operator_actions::Target;

use crate::attest::Libvirt;

/// The account the controller runs as. Recovery writes the controller's
/// state as this account, so the daemon can still write it afterwards.
pub const SERVICE_ACCOUNT: &str = "rampire";

/// The draft people and tools edit.
pub const DRAFT: &str = "/etc/rampire";

/// The copy the running controller started on, which `rampire apply` keeps
/// while it runs.
pub const RUNNING_COPY: &str = "/run/rampire/running-copy";

/// Where `vm add --fix-xml` keeps the definitions it replaced.
pub const DOMAIN_BACKUPS: &str = "/var/lib/rampire/domain-backups";

/// Where `vm remove` keeps a VM's settings, in the draft.
pub const PARKED_DIRECTORY: &str = "parked";

/// The configuration and connection a command works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// `--config`: one directory for every command instead of the draft
    /// and the applied copy.
    pub config: Option<PathBuf>,
    /// `--connect`: the libvirt connection instead of the configuration's.
    pub connect: Option<String>,
    /// The version and build, as `rampire --version` prints it.
    pub build: String,
    /// Where the change came from, for the journal: `command` or `tui`.
    pub via: &'static str,
}

impl Context {
    /// The configuration to change or check: `--config`, or the draft.
    pub fn draft(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(|| PathBuf::from(DRAFT))
    }

    /// The configuration the controller runs: `--config`, or the applied
    /// copy, or the draft before anything has been applied.
    pub fn running(&self) -> PathBuf {
        if let Some(config) = &self.config {
            return config.clone();
        }
        let current = applied_root().join(crate::applied::CURRENT);
        if current.exists() {
            current
        } else {
            PathBuf::from(DRAFT)
        }
    }

    /// `--connect`, or the configuration's libvirt connection.
    pub fn connection(&self) -> Result<String, String> {
        match &self.connect {
            Some(uri) => Ok(uri.clone()),
            None => Ok(rampire_core::settings::load_globals(&self.running())?.libvirt_uri),
        }
    }

    pub fn libvirt(&self) -> Result<Libvirt, String> {
        Ok(Libvirt {
            connection: self.connection()?,
            timeout: Duration::from_secs(60),
        })
    }

    /// One VM, or every VM, as the controller's own actions name it.
    pub fn target(&self, vm: Option<&str>) -> Result<Target, String> {
        Ok(Target {
            config: self.running(),
            vm: vm.map(str::to_owned),
            connection: self.connection()?,
        })
    }

    /// The command line that does what the TUI just did, so a person can
    /// learn it and script it: `rampire` with the global options this
    /// context was given, then `args`.
    pub fn command_line(&self, args: &[&str]) -> String {
        let mut words = vec!["rampire".to_owned()];
        if let Some(config) = &self.config {
            words.push("--config".to_owned());
            words.push(quote(&config.display().to_string()));
        }
        if let Some(connect) = &self.connect {
            words.push("--connect".to_owned());
            words.push(quote(connect));
        }
        words.extend(args.iter().map(|arg| quote(arg)));
        words.join(" ")
    }
}

/// A word as a shell needs it: unchanged when it is plain, else in single
/// quotes.
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_./=:,+@%".contains(character));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

pub fn applied_root() -> &'static Path {
    Path::new(crate::applied::APPLIED_ROOT)
}

/// A private temporary directory, removed when dropped. The commands run as
/// root, so it is always a new directory that only its owner can enter: no
/// other user can plant, read or swap a file in it.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        for attempt in 0..100_u32 {
            let path = std::env::temp_dir()
                .join(format!("rampire-{label}-{}-{attempt}", std::process::id()));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("create {}: {error}", path.display())),
            }
        }
        Err(format!(
            "no free temporary directory name for {label} in {}",
            std::env::temp_dir().display()
        ))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort: a leftover private directory harms nothing.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_directories_are_new_private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let first = TempDir::new("test").unwrap();
        let second = TempDir::new("test").unwrap();
        assert_ne!(first.path(), second.path());
        let mode = std::fs::metadata(first.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let path = first.path().to_path_buf();
        std::fs::write(path.join("file"), "x").unwrap();
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn command_lines_name_the_global_options_and_quote_what_needs_it() {
        let mut context = Context {
            config: None,
            connect: None,
            build: "0.1.0 (build x)".to_owned(),
            via: "tui",
        };
        assert_eq!(
            context.command_line(&["vm", "add", "webserver", "--fix-xml"]),
            "rampire vm add webserver --fix-xml"
        );
        context.config = Some(PathBuf::from("/srv/my config"));
        assert_eq!(
            context.command_line(&["set", "web", "max_ram=12GB"]),
            "rampire --config '/srv/my config' set web max_ram=12GB"
        );
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn the_default_configuration_is_etc_rampire() {
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "command",
        };
        assert_eq!(context.draft(), PathBuf::from("/etc/rampire"));
    }
}
