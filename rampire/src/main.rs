//! `rampire`: the controller daemon and every operator command, defined once
//! here with clap so `--help`, completions and man pages share one source
//! (decision D16).

mod apply;
mod controller;
mod daemon;
mod reporting;
mod vm_lifecycle;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rampire_host::operator_actions::Target;
use rampire_ops::process;

/// The account the controller runs as. Recovery writes the controller's
/// state as this account, so the daemon can still write it afterwards.
const SERVICE_ACCOUNT: &str = "rampire";
/// The build this binary came from, stamped by the build (`RAMPIRE_BUILD_ID`).
const BUILD_ID: &str = match option_env!("RAMPIRE_BUILD_ID") {
    Some(id) => id,
    None => "unversioned",
};

/// Exit codes, the same for every command.
mod exit {
    /// Done, or nothing to report.
    pub const SUCCESS: u8 = 0;
    /// The command failed, or `check` found a problem.
    pub const FAILURE: u8 = 1;
    /// The command line was wrong (clap's own code).
    pub const USAGE: u8 = 2;
    /// `apply --detailed-exitcode`: the applied configuration changed.
    pub const CHANGED: u8 = 2;
}

/// The draft people and tools edit.
const DRAFT: &str = "/etc/rampire";

/// The configuration to change or check: `--config`, or the draft.
fn draft(cli: &Cli) -> PathBuf {
    cli.config.clone().unwrap_or_else(|| PathBuf::from(DRAFT))
}

/// The configuration the controller runs: `--config`, or the applied copy,
/// or the draft before anything has been applied.
fn running(cli: &Cli) -> PathBuf {
    if let Some(config) = &cli.config {
        return config.clone();
    }
    let current = applied_root().join(rampire_ops::applied::CURRENT);
    if current.exists() {
        current
    } else {
        PathBuf::from(DRAFT)
    }
}

fn version() -> String {
    format!("{} (build {BUILD_ID})", env!("CARGO_PKG_VERSION"))
}

/// Share a pool of RAM between the VMs on a KVM host.
#[derive(Debug, Parser)]
#[command(name = "rampire", version = version(), propagate_version = true)]
struct Cli {
    /// The configuration directory. Default: /etc/rampire, the draft, for
    /// commands that change or check the configuration; the applied copy
    /// the controller runs for the views and recovery.
    #[arg(long, global = true, value_name = "DIR", env = "RAMPIRE_CONFIG")]
    config: Option<PathBuf>,
    /// The libvirt connection; default the configuration's `libvirt_uri`.
    #[arg(long, global = true, value_name = "URI")]
    connect: Option<String>,
    /// Answer yes to every question, as a script does.
    #[arg(long, global = true)]
    yes: bool,
    /// Write a versioned JSON document instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the controller. The systemd unit runs this.
    Daemon,
    /// Set up a new host: check it as doctor does, suggest global.toml from
    /// the host's memory and its VMs, create the configuration folders,
    /// apply, and enable and start the controller.
    ///
    /// A host problem stops init before anything is written. An existing
    /// global.toml is never changed; the suggestion is shown instead.
    Init,
    /// Show the controller's status for one VM, or for every VM.
    Status {
        /// The VM; default every VM.
        vm: Option<String>,
    },
    /// Watch the pool shared by every VM, live.
    Top {
        #[command(flatten)]
        view: View,
        /// Pool events shown.
        #[arg(long, value_name = "N", default_value_t = 8)]
        events: usize,
    },
    /// Watch one VM live.
    Watch {
        /// The VM; needed when there is more than one.
        vm: Option<String>,
        #[command(flatten)]
        view: View,
        /// Journal events shown.
        #[arg(long, value_name = "N", default_value_t = 6)]
        events: usize,
        /// Seconds between guest agent reads.
        #[arg(long, value_name = "SECONDS", default_value_t = 1)]
        guest_interval_seconds: u64,
        /// Skip guest agent reads.
        #[arg(long)]
        no_guest: bool,
    },
    /// Check the configuration and every VM's attestation against the live
    /// VMs; changes nothing.
    Check,
    /// Show what `apply` would change: the draft against the applied copy.
    Diff,
    /// Check the draft and, if it passes, make it what the controller runs.
    ///
    /// The draft (`--config`, normally /etc/rampire) is copied, the copy is
    /// checked like `check` while the controller keeps running, and only
    /// then does the controller restart on it. A draft that fails changes
    /// nothing.
    Apply {
        /// Exit 0 when nothing changed, 2 when something changed and 1 on
        /// failure, for configuration management.
        #[arg(long)]
        detailed_exitcode: bool,
    },
    /// Go back to the applied copy before the current one, through the same
    /// checks as `apply`.
    Rollback,
    /// Change settings in the draft, keeping its comments, then apply.
    ///
    /// TARGET is a VM's name, or `global` for global.toml. Each change is
    /// KEY=VALUE, for example `priority=150` or `max_ram=12GB`. Nothing is
    /// written unless the changed file is valid and the whole configuration
    /// still loads.
    Set {
        /// A VM's name, or `global`.
        target: String,
        /// KEY=VALUE, one or more.
        #[arg(required = true, value_name = "KEY=VALUE")]
        changes: Vec<String>,
        /// Change the draft only; apply later.
        #[arg(long)]
        no_apply: bool,
    },
    /// Check what the host needs for RAMpire, with the fix for each problem.
    Doctor,
    /// Show the controller's journal.
    Logs {
        /// Keep showing new entries.
        #[arg(short, long)]
        follow: bool,
        /// Start from this time, for example "1 hour ago".
        #[arg(long, value_name = "TIME")]
        since: Option<String>,
    },
    /// Clear a VM's latch once a person has checked it is safe to resume.
    ///
    /// Stops the controller, settles a stuck removal at what the guest
    /// kept, records the reason and starts the controller again.
    Recover {
        /// The latched VM.
        vm: String,
        /// Why it is safe to resume, recorded in the VM's state.
        reason: String,
    },
    /// Add, attest and remove the VMs RAMpire manages.
    Vm {
        #[command(subcommand)]
        command: VmCommand,
    },
    /// Recovery's own step, run as the service account.
    #[command(hide = true)]
    ClearLatch {
        vm: String,
        reason: String,
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Debug, Subcommand)]
enum VmCommand {
    /// Start managing a VM: check its definition, review it, write its
    /// settings and apply.
    ///
    /// Reports a missing or wrongly set virtio-mem device or control
    /// channel; --fix-xml shows the exact change and makes it once
    /// confirmed, keeping the old definition. Review answers the definition
    /// shows are filled in; only the rest are asked.
    Add {
        /// The libvirt domain.
        vm: String,
        /// Fix the domain's definition where it can be fixed.
        #[arg(long)]
        fix_xml: bool,
        /// Use this reviewed profile in reviews/ instead of asking.
        #[arg(long, value_name = "PROFILE")]
        review: Option<String>,
        /// The guest's kernel release (`uname -r`), for a new review.
        #[arg(long, value_name = "RELEASE")]
        kernel: Option<String>,
        /// Pool priority; higher is served first.
        #[arg(long, default_value_t = 100)]
        priority: u32,
        /// Smallest size; default the boot RAM.
        #[arg(long, value_name = "SIZE")]
        min_ram: Option<String>,
        /// Largest size; default the lower of the device and four times
        /// boot RAM.
        #[arg(long, value_name = "SIZE")]
        max_ram: Option<String>,
        /// Spare memory the guest keeps; default 512MB.
        #[arg(long, value_name = "SIZE")]
        free_ram: Option<String>,
    },
    /// Stop managing a VM at once. It keeps the memory it has until its next
    /// cold start, which begins on boot RAM alone. Its settings are kept in
    /// parked/, so `vm add` brings it back as it was.
    Remove {
        /// The VM.
        vm: String,
        /// Forget the VM instead of parking it: for a VM that is being
        /// deleted. Its settings, attestation, request state and definition
        /// backups go, and the review profile `vm add` wrote for it.
        #[arg(long)]
        forget: bool,
    },
    /// Attest a running VM again from its review profile, then apply.
    ///
    /// Needed after any change to the VM's definition: a VM whose live
    /// evidence has drifted from its attestation is never re-attested
    /// without a person (decision D11).
    Attest {
        /// The VM; or --drifted for every VM whose evidence has drifted.
        #[arg(required_unless_present = "drifted", conflicts_with = "drifted")]
        vm: Option<String>,
        /// Every running VM whose live evidence no longer matches.
        #[arg(long)]
        drifted: bool,
        /// Shut the VM down and start it first, so its saved definition
        /// takes effect.
        #[arg(long, conflicts_with = "drifted")]
        restart: bool,
        /// Write the attestation to the draft only; apply later.
        #[arg(long)]
        no_apply: bool,
    },
}

/// Options shared by the live views.
#[derive(Debug, clap::Args)]
struct View {
    /// Print one plain snapshot and exit.
    #[arg(long)]
    once: bool,
    /// Seconds between refreshes.
    #[arg(long, value_name = "SECONDS", default_value_t = 1)]
    interval_seconds: u64,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() {
                exit::USAGE
            } else {
                exit::SUCCESS
            };
            let _ = error.print();
            return ExitCode::from(code);
        }
    };
    match &cli.command {
        // The unit runs the daemon as the service account, and recovery
        // runs its own step as that account too.
        Command::Daemon => return daemon::run(&running(&cli)),
        Command::ClearLatch { .. } => {}
        _ if process::effective_uid() != 0 => {
            eprintln!("rampire: run this as root, for example with sudo");
            return ExitCode::from(exit::FAILURE);
        }
        _ => {}
    }
    match run(&cli) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rampire: {error}");
            ExitCode::from(exit::FAILURE)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let cwd = Path::new("/");
    match &cli.command {
        Command::Daemon => unreachable!("handled in main"),
        Command::Status { vm } => reporting::status(cli, vm.as_deref()),
        Command::Init => controller::init(cli),
        Command::Top { view, events } => {
            let mut args = view_args(cli, view)?;
            args.extend(["--events".to_owned(), events.to_string()]);
            rampire_ops::top::run(&rampire_ops::top::parse(&args)?, cwd)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Watch {
            vm,
            view,
            events,
            guest_interval_seconds,
            no_guest,
        } => {
            let mut args = view_args(cli, view)?;
            args.extend([
                "--events".to_owned(),
                events.to_string(),
                "--guest-interval-seconds".to_owned(),
                guest_interval_seconds.to_string(),
            ]);
            if let Some(vm) = vm {
                args.extend(["--vm".to_owned(), vm.clone()]);
            }
            if *no_guest {
                args.push("--no-guest".to_owned());
            }
            rampire_ops::watch::run(&rampire_ops::watch::parse(&args)?, cwd)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Check => reporting::check(cli),
        Command::Doctor => reporting::doctor(cli),
        Command::Diff => apply::diff(cli),
        Command::Apply { detailed_exitcode } => apply::apply(cli, *detailed_exitcode),
        Command::Rollback => apply::rollback(cli),
        Command::Set {
            target,
            changes,
            no_apply,
        } => {
            let changes = changes
                .iter()
                .map(|change| rampire_ops::set::parse_assignment(change))
                .collect::<Result<Vec<_>, _>>()?;
            let path = rampire_ops::set::set(&draft(cli), target, &changes)?;
            println!("changed {}", path.display());
            if *no_apply {
                println!("not applied: run rampire apply when ready");
                return Ok(ExitCode::SUCCESS);
            }
            apply::apply(cli, false)
        }
        Command::Logs { follow, since } => controller::logs(*follow, since.as_deref()),
        Command::Recover { vm, reason } => controller::recover(cli, vm, reason),
        Command::Vm { command } => match command {
            VmCommand::Attest {
                vm,
                drifted,
                restart,
                no_apply,
            } => vm_lifecycle::vm_attest(cli, vm.as_deref(), *drifted, *restart, *no_apply),
            VmCommand::Add {
                vm,
                fix_xml,
                review,
                kernel,
                priority,
                min_ram,
                max_ram,
                free_ram,
            } => vm_lifecycle::vm_add(
                cli,
                &vm_lifecycle::AddOptions {
                    vm,
                    fix_xml: *fix_xml,
                    review: review.as_deref(),
                    kernel: kernel.as_deref(),
                    priority: *priority,
                    min_ram: min_ram.as_deref(),
                    max_ram: max_ram.as_deref(),
                    free_ram: free_ram.as_deref(),
                },
            ),
            VmCommand::Remove { vm, forget } => vm_lifecycle::vm_remove(cli, vm, *forget),
        },
        Command::ClearLatch { vm, reason, apply } => {
            let target = target(cli, Some(vm))?;
            rampire_host::operator_actions::clear_latch(
                reason,
                &target,
                *apply,
                &mut std::io::stdout(),
            )?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// The views' own options, from the shared ones.
fn view_args(cli: &Cli, view: &View) -> Result<Vec<String>, String> {
    let mut args = vec![
        "--config".to_owned(),
        path_text(&running(cli))?,
        "--connect".to_owned(),
        connection(cli)?,
        "--interval-seconds".to_owned(),
        view.interval_seconds.to_string(),
    ];
    if view.once {
        args.push("--once".to_owned());
    }
    Ok(args)
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

/// `--connect`, or the configuration's libvirt connection.
fn connection(cli: &Cli) -> Result<String, String> {
    match &cli.connect {
        Some(uri) => Ok(uri.clone()),
        None => Ok(rampire_core::settings::load_globals(&running(cli))?.libvirt_uri),
    }
}

fn target(cli: &Cli, vm: Option<&str>) -> Result<Target, String> {
    Ok(Target {
        config: running(cli),
        vm: vm.map(str::to_owned),
        connection: connection(cli)?,
    })
}

fn applied_root() -> &'static Path {
    Path::new(rampire_ops::applied::APPLIED_ROOT)
}

fn libvirt(cli: &Cli) -> Result<rampire_ops::attest::Libvirt, String> {
    Ok(rampire_ops::attest::Libvirt {
        connection: connection(cli)?,
        timeout: std::time::Duration::from_secs(60),
    })
}

/// Ask on a terminal; with `--yes` the answer is yes. Without a terminal and
/// without `--yes` nothing is changed.
fn confirm(cli: &Cli, question: &str) -> Result<bool, String> {
    if cli.yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err("no terminal to ask on: pass --yes to go ahead".to_owned());
    }
    print!("{question} [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use rampire_ops::reports::{CheckReport, CheckSchema};

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn options_go_before_or_after_the_command() {
        let cli = Cli::try_parse_from(["rampire", "status", "--config", "/tmp/x", "web"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/x")));
        assert_eq!(running(&cli), PathBuf::from("/tmp/x"));
        assert!(matches!(cli.command, Command::Status { vm: Some(ref vm) } if vm == "web"));
        let cli = Cli::try_parse_from(["rampire", "--yes", "recover", "web", "checked"]).unwrap();
        assert!(cli.yes);
        assert!(Cli::try_parse_from(["rampire", "recover", "web"]).is_err());
        assert!(Cli::try_parse_from(["rampire"]).is_err());
        let cli =
            Cli::try_parse_from(["rampire", "set", "web", "priority=150", "max_ram=12GB"]).unwrap();
        assert!(matches!(cli.command, Command::Set { ref changes, .. } if changes.len() == 2));
        assert!(Cli::try_parse_from(["rampire", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["rampire", "vm", "attest", "web", "--restart"]).is_ok());
        assert!(Cli::try_parse_from(["rampire", "vm", "attest", "--drifted"]).is_ok());
        assert!(Cli::try_parse_from(["rampire", "vm", "attest"]).is_err());
        let cli = Cli::try_parse_from([
            "rampire",
            "vm",
            "add",
            "web",
            "--fix-xml",
            "--max-ram",
            "12GB",
            "--priority",
            "50",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Vm {
                command: VmCommand::Add {
                    fix_xml: true,
                    priority: 50,
                    ..
                }
            }
        ));
        assert!(Cli::try_parse_from(["rampire", "vm", "remove", "web"]).is_ok());
        assert!(Cli::try_parse_from(["rampire", "vm", "attest", "web", "--drifted"]).is_err());
    }

    #[test]
    fn json_documents_name_their_schema() {
        let report = CheckReport {
            schema: CheckSchema,
            version: 1,
            passed: true,
            vms: vec![],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.starts_with(r#"{"schema":"rampire.check","version":1,"#));
        let cli = Cli::try_parse_from(["rampire", "status", "--json"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn the_version_names_the_build() {
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
        assert!(version().contains("(build "));
    }

    #[test]
    fn the_default_configuration_is_etc_rampire() {
        let cli = Cli::try_parse_from(["rampire", "check"]).unwrap();
        if std::env::var_os("RAMPIRE_CONFIG").is_none() {
            assert_eq!(draft(&cli), PathBuf::from("/etc/rampire"));
        }
    }
}
