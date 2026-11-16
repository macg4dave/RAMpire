//! Running the commands the operator tools call (virsh, systemctl,
//! journalctl and the like), with their output and failures as text.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

pub fn run(program: &str, args: &[&str], cwd: &Path) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|error| format!("failed to start {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed with status {status}"))
    }
}

pub fn checked_output(program: &str, args: &[OsString], cwd: &Path) -> Result<Vec<u8>, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to start {program}: {error}"))?;
    succeeded(program, output)
}

pub fn checked_text(program: &str, args: &[OsString], cwd: &Path) -> Result<String, String> {
    text(program, checked_output(program, args, cwd)?)
}

/// Like [`checked_text`], with the output and descendant lifetime bounds of
/// the host's own commands.
pub fn bounded_text(
    program: &str,
    args: &[OsString],
    cwd: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let output = bounded_output(program, args, cwd, timeout)?;
    text(program, succeeded(program, output)?)
}

fn bounded_output(
    program: &str,
    args: &[OsString],
    cwd: &Path,
    timeout: Duration,
) -> Result<Output, String> {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    rampire_host::virsh::bounded_output(&mut command, timeout)
        .map_err(|error| format!("{program}: {error}"))
}

/// Whether `error`, from a command here, says it exited with code 1, as
/// `journalctl --grep` does when nothing matches. Rust prints the status
/// as `exit status: 1`.
pub fn exited_one(error: &str) -> bool {
    error.contains("failed with status exit status: 1:") || error.ends_with("exit status: 1")
}

/// The output's stdout, or the failure with its stderr.
fn succeeded(program: &str, output: Output) -> Result<Vec<u8>, String> {
    if !output.status.success() {
        return Err(format!(
            "{program} failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn text(program: &str, bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|error| format!("{program} returned invalid UTF-8: {error}"))
}

pub fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_os_string()
}

/// Wall-clock Unix time in milliseconds, for evidence and log names.
pub fn unix_millis() -> Result<u64, String> {
    use rampire_host::guest_channel::{SystemUnixClock, UnixClock};
    SystemUnixClock.now_unix_millis()
}

/// The caller's effective user ID; `u32::MAX` where there is none.
#[cfg(unix)]
pub fn effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: `geteuid` has no arguments and returns the caller's effective UID.
    unsafe { geteuid() }
}

#[cfg(not(unix))]
pub fn effective_uid() -> u32 {
    u32::MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_preserves_plain_arguments() {
        assert_eq!(os("value"), OsString::from("value"));
    }
}

#[cfg(test)]
mod exit_tests {
    #[test]
    fn exit_code_one_is_told_from_other_failures() {
        use std::os::unix::process::ExitStatusExt;
        let one = std::process::ExitStatus::from_raw(1 << 8);
        let error = format!("journalctl failed with status {one}: ");
        assert!(super::exited_one(&error), "{error}");
        let two = std::process::ExitStatus::from_raw(2 << 8);
        assert!(!super::exited_one(&format!(
            "journalctl failed with status {two}: x"
        )));
        assert!(!super::exited_one(&format!(
            "journalctl failed with status {one}0: x"
        )));
    }
}
