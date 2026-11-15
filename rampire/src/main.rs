//! `rampire`: the controller daemon and every operator command, defined once
//! here with clap so `--help`, completions and man pages share one source
//! and nothing else describes the command line.

mod controller;
mod daemon;
mod reporting;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::builder::RangedU64ValueParser;
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

/// The help heading of every option meant for scripts rather than people.
const SCRIPTS: &str = "For scripts";

/// What stays the same from release to release, for scripts: shown after
/// `rampire --help` and in rampire(8).
const STABLE: &str = "For scripts:
  Commands act at once and ask before a change; --yes answers those
  questions, never a review question about a VM's programs. These stay
  the same from the first release, and a change to any of them is named
  in the changelog:
  - the JSON documents of --json, each naming its schema and version:
    rampire.status-report, rampire.check, rampire.doctor, rampire.vm-list;
  - exit codes: 0 done, 1 failed or a problem found, 2 a wrong command
    line, and with apply --detailed-exitcode 2 when the configuration
    changed;
  - the journal's event names and keys, the controller's and
    event=operator_change for people's changes.
  Text output and the TUI are for people and may change.";

/// Share a pool of RAM between the VMs on a KVM host.
///
/// Run with no command on a terminal, `rampire` opens the TUI: the pool,
/// each VM, adding and removing VMs and the doctor, over SSH with
/// `ssh -t HOST sudo rampire`.
#[derive(Debug, Parser)]
#[command(
    name = "rampire",
    version = version(),
    propagate_version = true,
    after_long_help = STABLE
)]
struct Cli {
    /// The configuration directory. Default: /etc/rampire, the draft, for
    /// commands that change or check the configuration; the applied copy
    /// the controller runs for the views and recovery.
    #[arg(long, global = true, value_name = "DIR", env = "RAMPIRE_CONFIG")]
    config: Option<PathBuf>,
    /// The libvirt connection; default the configuration's `libvirt_uri`.
    #[arg(long, global = true, value_name = "URI")]
    connect: Option<String>,
    /// Answer yes to every question that may be answered for a person
    /// (never a review question), as a script does.
    #[arg(long, global = true, help_heading = SCRIPTS)]
    yes: bool,
    /// Write a versioned JSON document instead of text (status, check,
    /// doctor, vm list).
    #[arg(long, global = true, help_heading = SCRIPTS)]
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
        #[arg(
            long,
            value_name = "N",
            default_value_t = 8,
            value_parser = RangedU64ValueParser::<usize>::new().range(1..=30),
            help_heading = SCRIPTS
        )]
        events: usize,
    },
    /// Watch one VM live.
    Watch {
        /// The VM; needed when there is more than one.
        vm: Option<String>,
        #[command(flatten)]
        view: View,
        /// Journal events shown.
        #[arg(
            long,
            value_name = "N",
            default_value_t = 6,
            value_parser = RangedU64ValueParser::<usize>::new().range(1..=20),
            help_heading = SCRIPTS
        )]
        events: usize,
        /// Seconds between guest agent reads.
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = 1,
            value_parser = clap::value_parser!(u64).range(1..),
            help_heading = SCRIPTS
        )]
        guest_interval_seconds: u64,
        /// Skip guest agent reads.
        #[arg(long, help_heading = SCRIPTS)]
        no_guest: bool,
    },
    /// Check the configuration and every VM against the setup RAMpire
    /// recorded for it; changes nothing.
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
        #[arg(long, help_heading = SCRIPTS)]
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
        #[arg(long, help_heading = SCRIPTS)]
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
    /// Add, restart and remove the VMs RAMpire manages.
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
        #[arg(long, value_name = "PROFILE", help_heading = SCRIPTS)]
        review: Option<String>,
        /// The guest's kernel release (`uname -r`), for a new review.
        #[arg(long, value_name = "RELEASE", help_heading = SCRIPTS)]
        kernel: Option<String>,
        /// Pool priority; higher is served first.
        #[arg(long, default_value_t = 100, help_heading = SCRIPTS)]
        priority: u32,
        /// Smallest size; default the boot RAM.
        #[arg(long, value_name = "SIZE", help_heading = SCRIPTS)]
        min_ram: Option<String>,
        /// Largest size; default the lower of the device and four times
        /// boot RAM.
        #[arg(long, value_name = "SIZE", help_heading = SCRIPTS)]
        max_ram: Option<String>,
        /// Spare memory the guest keeps; default 512MB.
        #[arg(long, value_name = "SIZE", help_heading = SCRIPTS)]
        free_ram: Option<String>,
    },
    /// Stop managing a VM at once. It keeps the memory it has until its next
    /// cold start, which begins on boot RAM alone. Its settings are kept in
    /// parked/, so `vm add` brings it back as it was.
    Remove {
        /// The VM.
        vm: String,
        /// Forget the VM instead of parking it: for a VM that is being
        /// deleted. Its settings, recorded setup, request state and definition
        /// backups go, and the review profile `vm add` wrote for it.
        #[arg(long)]
        forget: bool,
    },
    /// Shut a VM down and start it again, so its saved definition takes
    /// effect, then apply.
    Restart {
        /// The VM.
        vm: String,
    },
}

/// Options shared by the live views.
#[derive(Debug, clap::Args)]
struct View {
    /// Print one plain snapshot and exit.
    #[arg(long, help_heading = SCRIPTS)]
    once: bool,
    /// Seconds between refreshes.
    #[arg(
        long,
        value_name = "SECONDS",
        default_value_t = 1,
        value_parser = clap::value_parser!(u64).range(1..),
        help_heading = SCRIPTS
    )]
    interval_seconds: u64,
}

impl View {
    /// The refresh period and whether to print one snapshot.
    fn settings(&self) -> (Duration, bool) {
        (Duration::from_secs(self.interval_seconds), self.once)
    }
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
            let mut top = rampire_ops::top::Command::load(&context)?;
            if top.vms.is_empty() {
                return Err("RAMpire manages no VMs yet: there is no pool to show".to_owned());
            }
            (top.interval, top.once) = view.settings();
            top.events = *events;
            rampire_ops::tui::live::top(top, cwd)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Watch {
            vm,
            view,
            events,
            guest_interval_seconds,
            no_guest,
        } => {
            let mut watch = rampire_ops::watch::Command::load(&context, vm.as_deref())?;
            (watch.interval, watch.once) = view.settings();
            watch.events = *events;
            watch.guest_interval = Duration::from_secs(*guest_interval_seconds);
            watch.guest = !no_guest;
            rampire_ops::tui::live::watch(&context, watch, cwd)?;
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
            VmCommand::Restart { vm } => {
                lifecycle::vm_restart(&context, ui, vm).map(|()| ExitCode::SUCCESS)
            }
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
            let path = directory.join("rampire.toml.5");
            std::fs::write(&path, toml_page())
                .map_err(|error| format!("write {}: {error}", path.display()))?;
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

/// Markdown from the settings' definitions as roff: code in bold, links as
/// their text, and roff's own characters escaped.
fn roff_text(markdown: &str) -> String {
    let mut text = String::new();
    let mut rest = markdown;
    // Links keep their text.
    while let Some(open) = rest.find('[') {
        match (rest[open..].find("]("), rest[open..].find(')')) {
            (Some(middle), Some(close)) if middle < close => {
                text.push_str(&rest[..open]);
                text.push_str(&rest[open + 1..open + middle]);
                rest = &rest[open + close + 1..];
            }
            _ => {
                text.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    text.push_str(rest);
    let escaped = text.replace('\\', "\\e").replace('-', "\\-");
    let mut bold = false;
    let mut out = String::new();
    for character in escaped.chars() {
        if character == '`' {
            out.push_str(if bold { "\\fR" } else { "\\fB" });
            bold = !bold;
        } else {
            out.push(character);
        }
    }
    // A line may not start with roff's control characters.
    out.lines()
        .map(|line| {
            if line.starts_with('.') || line.starts_with('\'') {
                format!("\\&{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// rampire.toml(5): the configuration files and every setting, from the
/// settings' definitions in rampire-core.
fn toml_page() -> String {
    use rampire_core::settings_reference::{FILES, SIZES};
    let mut page = format!(
        ".TH RAMPIRE.TOML 5 \"\" \"RAMpire {}\" \"RAMpire\"\n.SH NAME\nrampire.toml \\- the RAMpire controller's configuration\n.SH DESCRIPTION\n",
        env!("CARGO_PKG_VERSION")
    );
    page.push_str(&roff_text(
        "The draft in `/etc/rampire` holds `global.toml` for the host, one `vms.d/VM.toml` per VM and the review profiles in `reviews/`. People and tools edit the draft, or change it with `rampire set`; `rampire apply` checks a copy and makes it what the controller runs.",
    ));
    page.push('\n');
    for file in FILES {
        page.push_str(&format!(".SH {}\n", file.path.to_uppercase()));
        page.push_str(&roff_text(&file.intro.replace('\n', " ")));
        page.push('\n');
        for setting in file.settings {
            page.push_str(".TP\n");
            page.push_str(&roff_text(setting.names));
            if let Some(default) = setting.default {
                page.push_str(&format!(" (default {})", roff_text(default)));
            }
            page.push('\n');
            page.push_str(&roff_text(setting.meaning));
            page.push_str(".\n");
        }
    }
    page.push_str(".SH SIZES\n");
    for paragraph in SIZES.split("\n\n") {
        page.push_str(".PP\n");
        page.push_str(&roff_text(&paragraph.replace('\n', " ")));
        page.push('\n');
    }
    page.push_str(
        ".SH SEE ALSO\n\\fBrampire\\fR(8), \\fBrampire\\-set\\fR(8), \\fBrampire\\-apply\\fR(8)\n",
    );
    page
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use rampire_ops::reports::{CheckReport, CheckSchema};

    #[test]
    fn the_settings_page_names_every_setting_in_roff() {
        let page = toml_page();
        assert!(page.starts_with(".TH RAMPIRE.TOML 5"), "{page}");
        for file in rampire_core::settings_reference::FILES {
            for name in file.names() {
                assert!(page.contains(&roff_text(&format!("`{name}`"))), "{name}");
            }
        }
        assert!(
            !page.contains("](administration.md"),
            "links keep only their text"
        );
        assert_eq!(roff_text("`a-b` [x](y)"), "\\fBa\\-b\\fR x");
        assert_eq!(roff_text(".x"), "\\&.x");
    }

    /// Every `rampire` command line the docs give a person to type, in
    /// code, is one the command line takes once its placeholders are
    /// filled in. A bare mention such as `rampire set` may leave out what
    /// the command needs; nothing may be unknown or of the wrong kind.
    #[test]
    fn every_command_in_the_docs_parses() {
        use clap::error::ErrorKind;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut pages = vec![root.join("README.md")];
        for entry in std::fs::read_dir(root.join("docs")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "md") {
                pages.push(path);
            }
        }
        let mut failures = Vec::new();
        let mut checked = 0;
        for page in &pages {
            for code in code_in(&std::fs::read_to_string(page).unwrap()) {
                let Some(line) = code
                    .strip_prefix("sudo rampire ")
                    .or_else(|| code.strip_prefix("rampire "))
                else {
                    continue;
                };
                let line = line.split(" #").next().unwrap_or(line);
                let words = std::iter::once("rampire".to_owned()).chain(filled(line));
                match Cli::try_parse_from(words) {
                    Err(error)
                        if !matches!(
                            error.kind(),
                            ErrorKind::DisplayHelp
                                | ErrorKind::DisplayVersion
                                | ErrorKind::MissingRequiredArgument
                                | ErrorKind::MissingSubcommand
                                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
                        ) =>
                    {
                        failures.push(format!("{}: {code}: {}", page.display(), error.kind()));
                    }
                    _ => checked += 1,
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
        assert!(checked > 50, "checked only {checked} commands");
    }

    /// The code in a Markdown page: each inline code span, and each line
    /// of a fenced block.
    fn code_in(markdown: &str) -> Vec<String> {
        let mut code = Vec::new();
        let mut fenced = false;
        for line in markdown.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            } else if fenced {
                code.push(line.trim().to_owned());
            } else {
                code.extend(line.split('`').skip(1).step_by(2).map(str::to_owned));
            }
        }
        code
    }

    /// A documented command's words, its placeholders as values: `[VM]` and
    /// `VM` a name, `SIZE` and `N` a number, `a|b` the first, `"..."` one
    /// word.
    fn filled(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut rest = line.trim();
        while !rest.is_empty() {
            let (word, after) = match rest.strip_prefix('"') {
                Some(quoted) => match quoted.split_once('"') {
                    Some((word, after)) => (word, after),
                    None => (quoted, ""),
                },
                None => rest.split_once(' ').unwrap_or((rest, "")),
            };
            let word = word.trim_start_matches('[').trim_end_matches(']');
            let word = match word {
                "VM" | "NAME" | "TARGET" => "web".to_owned(),
                "COMMAND" => "status".to_owned(),
                "KEY=VALUE" | "KEY=VALUE..." => "priority=1".to_owned(),
                "N" | "SECONDS" => "1".to_owned(),
                // One of several: the first.
                _ if word.contains('|') => word.split('|').next().unwrap_or(word).to_owned(),
                _ => word.replace("=SIZE", "=1GB"),
            };
            if !word.is_empty() && word != "..." && word != "|" {
                words.push(word);
            }
            rest = after.trim_start();
        }
        words
    }

    #[test]
    fn script_options_are_under_their_own_heading() {
        let command = Cli::command();
        let json = command
            .get_arguments()
            .find(|arg| arg.get_id() == "json")
            .unwrap();
        assert_eq!(json.get_help_heading(), Some(SCRIPTS));
        let config = command
            .get_arguments()
            .find(|arg| arg.get_id() == "config")
            .unwrap();
        assert_eq!(config.get_help_heading(), None);
    }

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
        assert!(Cli::try_parse_from(["rampire", "vm", "restart", "web"]).is_ok());
        assert!(Cli::try_parse_from(["rampire", "vm", "restart"]).is_err());
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
        // The live views' ranges.
        assert!(Cli::try_parse_from(["rampire", "top", "--events", "30"]).is_ok());
        for bad in [
            &["top", "--events", "0"][..],
            &["top", "--events", "31"],
            &["top", "--interval-seconds", "0"],
            &["watch", "--events", "21"],
            &["watch", "--guest-interval-seconds", "0"],
        ] {
            let args = std::iter::once("rampire").chain(bad.iter().copied());
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
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
