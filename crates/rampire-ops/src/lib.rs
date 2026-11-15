//! What the `rampire` operator commands do, apart from their command line:
//! checking, changing and applying the configuration, adding and attesting
//! VMs, checking the host, the live views of the pool and of one VM, and
//! the TUI. Nothing here needs a source checkout; commands run from the
//! working directory the caller passes.

pub mod applied;
pub mod attest;
pub mod audit;
pub mod check;
pub mod context;
pub mod dashboard;
pub mod doctor;
pub mod doctor_vm;
pub mod evidence;
pub mod flows;
pub mod guest;
pub mod host_readings;
pub mod init;
pub mod inventory;
pub mod journal;
pub mod process;
pub mod reports;
pub mod set;
pub mod systemd_unit;
pub mod tone;
pub mod top;
pub mod tui;
pub mod ui;
pub mod vm;
pub mod watch;
