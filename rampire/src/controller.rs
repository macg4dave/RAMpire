//! `rampire logs`: the controller's journal, and the changes people made
//! with the commands and the TUI, which are logged beside it.

use std::process::ExitCode;

/// The journal of the controller's unit and of people's changes
/// (`rampire_ops::audit`), optionally only the lines about one VM.
pub(crate) fn logs(
    follow: bool,
    since: Option<&str>,
    vm: Option<&str>,
) -> Result<ExitCode, String> {
    let mut command = std::process::Command::new("journalctl");
    command.args(rampire_ops::journal::log_arguments(since, vm));
    if follow {
        command.arg("--follow");
    }
    let status = command
        .status()
        .map_err(|error| format!("run journalctl: {error}"))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
