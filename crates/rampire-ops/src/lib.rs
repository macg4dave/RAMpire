//! What the `rampire` operator commands do, apart from their command line:
//! checking, changing and applying the configuration, adding and attesting
//! VMs, checking the host, and the live views of the pool and of one VM.
//! Nothing here needs a source checkout; commands run from the working
//! directory the caller passes.

pub mod applied;
pub mod attest;
pub mod check;
pub mod dashboard;
pub mod doctor;
pub mod evidence;
pub mod host_readings;
pub mod init;
pub mod process;
pub mod reports;
pub mod set;
pub mod systemd_unit;
pub mod top;
pub mod vm;
pub mod watch;
