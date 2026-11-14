//! `rampire`: the controller daemon and every operator command, defined once
//! here with clap so `--help`, completions and man pages share one source
//! (decision D16).

mod controller;
mod daemon;
mod reporting;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rampire_ops::context::Context;
use rampire_ops::flows::{apply, controller as flows, lifecycle};
use rampire_ops::process;
use rampire_ops::ui::Terminal;

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

/// What the command line names: the configuration, the connection and the
/// build, for the flows in `rampire_ops`.
fn context(cli: &Cli) -> Context {
    Context {
        config: cli.config.clone(),
        connect: cli.connect.clone(),
        build: version(),
        via: "command",
    }
}

fn version() -> String {
    format!("{} (build {BUILD_ID})", env!("CARGO_PKG_VERSION"))
}

/// Share a pool of RAM between the VMs on a KVM host.
///
/// Run with no command on a terminal, `rampire` opens the TUI: the pool,
/// each VM, adding and removing VMs and the doctor, over SSH with
/// `ssh -t HOST sudo rampire`.
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
    command: Option<Command>,
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
    /// Check what the host needs for RAMpire, with the fix for each problem,
    /// and one line per managed VM; or everything about one VM.
    Doctor {
        /// The VM to check: its definition, attestation, latch, guest and
        /// last request.
        vm: Option<String>,
    },
    /// Show the controller's journal, and the changes people made.
    Logs {
        /// Only the lines about this VM.
        #[arg(long, value_name = "VM")]
        vm: Option<String>,
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
    /// Write the man pages or shell completions, as the packages are built.
    #[command(hide = true)]
    Generate {
        what: Generated,
        /// Where to write them.
        directory: PathBuf,
    },
}

/// What `generate` writes.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Generated {
    /// rampire(8) and a page per subcommand.
    Man,
    /// bash, zsh and fish completions.
    Completions,
}

#[derive(Debug, Subcommand)]
enum VmCommand {
    /// List every VM on the libvirt connection: managed, parked or not
    /// managed, with what `vm add` would change for one that isn't.
    List,
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
        Some(Command::Daemon | Command::ClearLatch { .. } | Command::Generate { .. }) => {}
        // The TUI, or the help when nothing is a terminal.
        None if process::effective_uid() != 0 => {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                eprintln!(
                    "rampire: the TUI runs as root: start it with sudo rampire, or over SSH with ssh -t HOST sudo rampire"
                );
                return ExitCode::from(exit::FAILURE);
            }
        }
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

/// The result of a flow that may change the applied configuration, as
/// `--detailed-exitcode` reports it.
fn changed_code(changed: bool, detailed: bool) -> ExitCode {
    ExitCode::from(if changed && detailed {
        exit::CHANGED
    } else {
        exit::SUCCESS
    })
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let cwd = Path::new("/");
    let context = context(cli);
    let mut terminal = Terminal::stdio(cli.yes);
    let ui = &mut terminal;
    let Some(command) = &cli.command else {
        return tui_or_help(cli);
    };
    match command {
        Command::Daemon => Ok(daemon::run(&context.running())),
        Command::Status { vm } => reporting::status(cli, vm.as_deref()),
        Command::Init => flows::init(&context, ui).map(|_| ExitCode::SUCCESS),
        Command::Top { view, events } => {
            let mut args = view_args(&context, view)?;
            args.extend(["--events".to_owned(), events.to_string()]);
            rampire_ops::tui::live::top(rampire_ops::top::parse(&args)?, cwd)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Watch {
            vm,
            view,
            events,
            guest_interval_seconds,
            no_guest,
        } => {
            let mut args = view_args(&context, view)?;
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
            rampire_ops::tui::live::watch(rampire_ops::watch::parse(&args)?, cwd)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Check => reporting::check(cli),
        Command::Doctor { vm } => reporting::doctor(cli, vm.as_deref()),
        Command::Diff => apply::diff(&context, ui).map(|()| ExitCode::SUCCESS),
        Command::Apply { detailed_exitcode } => {
            apply::apply(&context, ui).map(|changed| changed_code(changed, *detailed_exitcode))
        }
        Command::Rollback => apply::rollback(&context, ui).map(|()| ExitCode::SUCCESS),
        Command::Set {
            target,
            changes,
            no_apply,
        } => {
            let changes = changes
                .iter()
                .map(|change| rampire_ops::set::parse_assignment(change))
                .collect::<Result<Vec<_>, _>>()?;
            lifecycle::set(&context, ui, target, &changes, *no_apply).map(|_| ExitCode::SUCCESS)
        }
        Command::Logs { follow, since, vm } => {
            controller::logs(*follow, since.as_deref(), vm.as_deref())
        }
        Command::Recover { vm, reason } => {
            flows::recover(&context, ui, vm, reason).map(|()| ExitCode::SUCCESS)
        }
        Command::Vm { command } => match command {
            VmCommand::List => reporting::vm_list(cli),
            VmCommand::Attest {
                vm,
                drifted,
                restart,
                no_apply,
            } => lifecycle::vm_attest(&context, ui, vm.as_deref(), *drifted, *restart, *no_apply)
                .map(|()| ExitCode::SUCCESS),
            VmCommand::Add {
                vm,
                fix_xml,
                review,
                kernel,
                priority,
                min_ram,
                max_ram,
                free_ram,
            } => lifecycle::vm_add(
                &context,
                ui,
                &lifecycle::AddOptions {
                    vm: vm.clone(),
                    fix_xml: *fix_xml,
                    review: review.clone(),
                    kernel: kernel.clone(),
                    priority: *priority,
                    min_ram: min_ram.clone(),
                    max_ram: max_ram.clone(),
                    free_ram: free_ram.clone(),
                },
            )
            .map(|_| ExitCode::SUCCESS),
            VmCommand::Remove { vm, forget } => {
                lifecycle::vm_remove(&context, ui, vm, *forget).map(|()| ExitCode::SUCCESS)
            }
        },
        Command::ClearLatch { vm, reason, apply } => {
            let target = context.target(Some(vm))?;
            rampire_host::operator_actions::clear_latch(
                reason,
                &target,
                *apply,
                &mut std::io::stdout(),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Generate { what, directory } => generate(*what, directory),
    }
}

/// `rampire` alone: the TUI on a terminal, else the help.
fn tui_or_help(cli: &Cli) -> Result<ExitCode, String> {
    use clap::CommandFactory;
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let mut context = context(cli);
        context.via = "tui";
        rampire_ops::tui::run(context)?;
        return Ok(ExitCode::SUCCESS);
    }
    Cli::command()
        .print_long_help()
        .map_err(|error| error.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// `rampire generate`: what the packages install beside the binary, made
/// from the same command definitions as `--help`.
fn generate(what: Generated, directory: &Path) -> Result<ExitCode, String> {
    use clap::CommandFactory;
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let mut command = Cli::command();
    match what {
        Generated::Man => {
            command.build();
            man_pages(&command, "rampire", directory)?;
        }
        Generated::Completions => {
            for shell in [
                clap_complete::Shell::Bash,
                clap_complete::Shell::Zsh,
                clap_complete::Shell::Fish,
            ] {
                clap_complete::generate_to(shell, &mut command, "rampire", directory)
                    .map_err(|error| format!("write {shell} completion: {error}"))?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `name(8)` for `command`, and a page for each of its visible
/// subcommands, named after the words that run it (`rampire-vm-add`).
fn man_pages(command: &clap::Command, name: &str, directory: &Path) -> Result<(), String> {
    let page = command
        .clone()
        .name(name.to_owned())
        .bin_name(name.replace('-', " "));
    let path = directory.join(format!("{name}.8"));
    let mut text = Vec::new();
    clap_mangen::Man::new(page)
        .section("8")
        .manual("RAMpire")
        .source(format!("RAMpire {}", env!("CARGO_PKG_VERSION")))
        .render(&mut text)
        .map_err(|error| format!("render {}: {error}", path.display()))?;
    std::fs::write(&path, text).map_err(|error| format!("write {}: {error}", path.display()))?;
    for sub in command.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        if sub.get_name() == "help" {
            continue;
        }
        man_pages(sub, &format!("{name}-{}", sub.get_name()), directory)?;
    }
    Ok(())
}

/// The views' own options, from the shared ones.
fn view_args(context: &Context, view: &View) -> Result<Vec<String>, String> {
    let mut args = vec![
        "--config".to_owned(),
        path_text(&context.running())?,
        "--connect".to_owned(),
        context.connection()?,
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
        assert_eq!(context(&cli).running(), PathBuf::from("/tmp/x"));
        assert!(matches!(cli.command, Some(Command::Status { vm: Some(ref vm) }) if vm == "web"));
        let cli = Cli::try_parse_from(["rampire", "--yes", "recover", "web", "checked"]).unwrap();
        assert!(cli.yes);
        assert!(Cli::try_parse_from(["rampire", "recover", "web"]).is_err());
        // No command opens the TUI on a terminal, or prints the help.
        assert!(Cli::try_parse_from(["rampire"]).unwrap().command.is_none());
        let cli =
            Cli::try_parse_from(["rampire", "set", "web", "priority=150", "max_ram=12GB"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Set { ref changes, .. }) if changes.len() == 2)
        );
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
            Some(Command::Vm {
                command: VmCommand::Add {
                    fix_xml: true,
                    priority: 50,
                    ..
                }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["rampire", "doctor", "web"])
                .unwrap()
                .command,
            Some(Command::Doctor { vm: Some(_) })
        ));
        assert!(Cli::try_parse_from(["rampire", "vm", "list", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["rampire", "logs", "--vm", "web"]).is_ok());
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
            assert_eq!(context(&cli).draft(), PathBuf::from("/etc/rampire"));
        }
    }
}
