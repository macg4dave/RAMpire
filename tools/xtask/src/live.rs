//! `cargo xtask live`: the live suite ([testing](../../../docs/testing.md)).
//!
//! It installs this checkout's build on this host, makes its own small VMs
//! named `rampire-test-*` from cloud images, and works through what a
//! person does with RAMpire, in the TUI and with the commands, each run in
//! tmux and read off the screen as a person would see it. Every question
//! asked is checked against what is expected, and the screen against what
//! must never be on it. At the end it says what failed, with the screens,
//! in `target/live/report.md`, and takes its VMs out again.
//!
//! It only ever changes `rampire-test-*` VMs. It answers No to changing the
//! pool, so the host's own settings stay as they are.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

pub const HELP: &str = r#"
The live suite, on this host (docs/testing.md):
  cargo xtask live [--no-install] [--keep] [SCENARIO...]
                                   installs this build, makes rampire-test-*
                                   VMs, runs every scenario (or those named)
                                   and writes target/live/report.md.
                                   --keep leaves the VMs for a look after.
  Scenarios: add-declined, add-cancelled, add-running, add-off,
             restart-open, settings, upgrade, vm-gone, remove
  RAMPIRE_LIVE_DIR (or site/build.env) names a folder for the VMs' disks.
"#;

const PREFIX: &str = "rampire-test-";
const SCENARIOS: [&str; 9] = [
    "add-declined",
    "add-cancelled",
    "add-running",
    "add-off",
    "restart-open",
    "settings",
    "upgrade",
    "vm-gone",
    "remove",
];

/// The systems the test VMs run, from their cloud images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum System {
    /// Debian 13: apt, and a guest agent that runs commands.
    Debian,
    /// Rocky Linux 10: dnf, and a guest agent that doesn't run commands, as
    /// RHEL ships it, so the disk way is used.
    Rocky,
}

impl System {
    fn url(self) -> &'static str {
        match self {
            Self::Debian => {
                "https://cloud.debian.org/images/cloud/trixie/latest/debian-13-genericcloud-amd64.qcow2"
            }
            Self::Rocky => {
                "https://dl.rockylinux.org/pub/rocky/10/images/x86_64/Rocky-10-GenericCloud-Base.latest.x86_64.qcow2"
            }
        }
    }

    fn base(self) -> &'static str {
        match self {
            Self::Debian => "debian-13.qcow2",
            Self::Rocky => "rocky-10.qcow2",
        }
    }

    fn osinfo(self) -> &'static str {
        match self {
            Self::Debian => "debian13",
            Self::Rocky => "rocky9",
        }
    }

    fn vm(self) -> String {
        match self {
            Self::Debian => format!("{PREFIX}debian"),
            Self::Rocky => format!("{PREFIX}rocky"),
        }
    }
}

struct Options {
    install: bool,
    keep: bool,
    only: Vec<String>,
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options {
        install: true,
        keep: false,
        only: Vec::new(),
    };
    for argument in arguments {
        match argument.as_str() {
            "--no-install" => options.install = false,
            "--keep" => options.keep = true,
            name if SCENARIOS.contains(&name) => options.only.push(name.to_owned()),
            other => return Err(format!("unknown live argument {other}{HELP}")),
        }
    }
    Ok(options)
}

/// A failure the suite found: where, what, and the screen at the time.
struct Failure {
    scenario: String,
    what: String,
    screen: Option<String>,
}

/// What the run found, and where it is in the suite.
struct Run {
    repo: PathBuf,
    dir: PathBuf,
    scenario: String,
    failures: Vec<Failure>,
    log: String,
}

impl Run {
    fn fail(&mut self, what: impl Into<String>, screen: Option<String>) {
        let what = what.into();
        println!("  FAIL {what}");
        self.failures.push(Failure {
            scenario: self.scenario.clone(),
            what,
            screen,
        });
    }

    fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("  {line}");
        let _ = writeln!(self.log, "[{}] {line}", self.scenario);
    }
}

pub fn run(arguments: &[String], repo: &Path) -> Result<(), String> {
    let options = parse(arguments)?;
    let _lock = Lock::take(&repo.join("target/live/lock"))?;
    let dir = live_dir(repo)?;
    preflight(&dir)?;
    if options.install {
        println!("==> installing this build on this host");
        rampire_ops::process::run("bash", &["tools/build.sh", "install"], repo)?;
    }
    let mut run = Run {
        repo: repo.to_path_buf(),
        dir,
        scenario: "setup".to_owned(),
        failures: Vec::new(),
        log: String::new(),
    };
    clean_up(&mut run);
    let chosen: Vec<&str> = SCENARIOS
        .iter()
        .copied()
        .filter(|name| options.only.is_empty() || options.only.iter().any(|only| only == name))
        .collect();
    for scenario in &chosen {
        println!("==> {scenario}");
        run.scenario = (*scenario).to_owned();
        let result = match *scenario {
            "add-declined" => add_declined(&mut run),
            "add-cancelled" => add_cancelled(&mut run),
            "add-running" => add_running(&mut run),
            "add-off" => add_off(&mut run),
            "restart-open" => restart_open(&mut run),
            "settings" => settings(&mut run),
            "upgrade" => upgrade(&mut run),
            "vm-gone" => vm_gone(&mut run),
            "remove" => remove(&mut run),
            _ => Ok(()),
        };
        if let Err(error) = result {
            run.fail(format!("stopped: {error}"), None);
        }
    }
    run.scenario = "cleanup".to_owned();
    if !options.keep {
        clean_up(&mut run);
    }
    let report = write_report(&run)?;
    if run.failures.is_empty() {
        println!(
            "==> live suite passed ({} scenarios); {}",
            chosen.len(),
            report.display()
        );
        Ok(())
    } else {
        Err(format!(
            "{} failure(s) in the live suite: {}",
            run.failures.len(),
            report.display()
        ))
    }
}

/// One run at a time: two would drive the same VMs and the same
/// controller at once, and each would see the other's changes as faults.
struct Lock(PathBuf);

impl Lock {
    fn take(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        if let Ok(pid) = std::fs::read_to_string(path) {
            let pid = pid.trim();
            if !pid.is_empty() && Path::new(&format!("/proc/{pid}")).exists() {
                return Err(format!(
                    "another live suite is running (process {pid}); wait for it, or remove {} if it isn't",
                    path.display()
                ));
            }
        }
        std::fs::write(path, std::process::id().to_string())
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        Ok(Self(path.to_path_buf()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The folder for the VMs' disks: RAMPIRE_LIVE_DIR, from the environment or
/// site/build.env.
fn live_dir(repo: &Path) -> Result<PathBuf, String> {
    let from_site = std::fs::read_to_string(repo.join("site/build.env"))
        .ok()
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.trim().strip_prefix("RAMPIRE_LIVE_DIR="))
                .map(|value| value.trim().trim_matches('"').to_owned())
                .next_back()
        });
    std::env::var("RAMPIRE_LIVE_DIR")
        .ok()
        .or(from_site)
        .map(PathBuf::from)
        .ok_or_else(|| {
            "set RAMPIRE_LIVE_DIR (or put it in site/build.env): a folder you own for the test VMs' disks, which libvirt can read (docs/testing.md)".to_owned()
        })
}

/// Everything the suite needs, checked before anything is changed.
fn preflight(dir: &Path) -> Result<(), String> {
    for tool in ["tmux", "curl", "qemu-img", "xorriso"] {
        if !Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Err(format!("the live suite needs {tool}"));
        }
    }
    for (command, args) in [
        ("rampire", vec!["--version"]),
        ("virsh", vec!["-c", "qemu:///system", "version"]),
        ("virt-install", vec!["--version"]),
        ("journalctl", vec!["-n", "0"]),
    ] {
        sudo(command, &args).map_err(|error| {
            format!("sudo {command} without a password: {error}. The setup is in docs/testing.md")
        })?;
    }
    let probe = dir.join(".rampire-live");
    std::fs::create_dir_all(dir.join("base"))
        .and_then(|()| std::fs::write(&probe, b""))
        .map_err(|error| format!("{} isn't writable: {error}", dir.display()))?;
    let _ = std::fs::remove_file(probe);
    // The VMs reach the network through libvirt's default network.
    if !sudo("virsh", &["-c", "qemu:///system", "net-info", "default"]).is_ok_and(|info| {
        info.lines()
            .any(|line| line.starts_with("Active:") && line.ends_with("yes"))
    }) {
        sudo("virsh", &["-c", "qemu:///system", "net-start", "default"])
            .map_err(|error| format!("libvirt's default network isn't running: {error}"))?;
    }
    Ok(())
}

/// How many descriptors polkitd has open, or `None` without it.
fn polkit_descriptors() -> Option<usize> {
    let pid = local("pgrep", &["-x", "polkitd"]).ok()?;
    let listing = sudo("ls", &[&format!("/proc/{}/fd", pid.trim())]).ok()?;
    Some(listing.lines().count())
}

/// Run `program` as root with sudo, never asking for a password.
fn sudo(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("sudo")
        .arg("-n")
        .arg(program)
        .args(args)
        .output()
        .map_err(|error| format!("run sudo {program}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn virsh(args: &[&str]) -> Result<String, String> {
    let mut all = vec!["-c", "qemu:///system"];
    all.extend_from_slice(args);
    sudo("virsh", &all)
}

fn local(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("run {program}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

// ---------------------------------------------------------------------
// The test VMs

/// The test VM running `system`, made if it isn't there, running and with
/// its guest agent answering.
fn test_vm(run: &mut Run, system: System) -> Result<String, String> {
    let name = system.vm();
    if virsh(&["domstate", &name]).is_err() {
        make_vm(run, system)?;
    } else if virsh(&["domstate", &name])?.trim() != "running" {
        virsh(&["start", &name])?;
    }
    wait_for_agent(run, &name, Duration::from_secs(600))?;
    Ok(name)
}

fn make_vm(run: &mut Run, system: System) -> Result<(), String> {
    let name = system.vm();
    let base = run.dir.join("base").join(system.base());
    if !base.exists() {
        run.note(format!("downloading {}", system.url()));
        let partial = base.with_extension("part");
        local(
            "curl",
            &[
                "-fsSL",
                "--retry",
                "5",
                "-o",
                &text(&partial)?,
                system.url(),
            ],
        )?;
        std::fs::rename(&partial, &base).map_err(|error| error.to_string())?;
    }
    let disk = run.dir.join(format!("{name}.qcow2"));
    let _ = std::fs::remove_file(&disk);
    local(
        "qemu-img",
        &[
            "create",
            "-q",
            "-f",
            "qcow2",
            "-F",
            "qcow2",
            "-b",
            &text(&base)?,
            &text(&disk)?,
            "12G",
        ],
    )?;
    // cloud-init's NoCloud seed, a CD labelled cidata: the guest agent, and
    // nothing else. Made here, beside the disk, rather than by
    // virt-install, which keeps its own in a folder this host may not have.
    let seed_dir = run.dir.join(format!("{name}.seed"));
    std::fs::create_dir_all(&seed_dir).map_err(|error| error.to_string())?;
    std::fs::write(
        seed_dir.join("user-data"),
        "#cloud-config\npackages: [qemu-guest-agent]\nruncmd:\n  - [systemctl, enable, --now, qemu-guest-agent]\n",
    )
    .and_then(|()| {
        std::fs::write(
            seed_dir.join("meta-data"),
            format!("instance-id: {name}\nlocal-hostname: {name}\n"),
        )
    })
    .map_err(|error| error.to_string())?;
    let seed = run.dir.join(format!("{name}.seed.iso"));
    let _ = std::fs::remove_file(&seed);
    local(
        "xorriso",
        &[
            "-as",
            "mkisofs",
            "-quiet",
            "-output",
            &text(&seed)?,
            "-volid",
            "cidata",
            "-joliet",
            "-rock",
            &text(&seed_dir.join("user-data"))?,
            &text(&seed_dir.join("meta-data"))?,
        ],
    )?;
    run.note(format!("making {name}"));
    sudo(
        "virt-install",
        &[
            "--connect",
            "qemu:///system",
            "--name",
            &name,
            "--memory",
            "1536",
            "--vcpus",
            "2",
            "--cpu",
            "host-passthrough",
            "--import",
            "--disk",
            &format!("path={},format=qcow2,bus=virtio", text(&disk)?),
            "--network",
            "network=default,model=virtio",
            "--osinfo",
            &format!("name={}", system.osinfo()),
            "--disk",
            &format!("path={},device=cdrom", text(&seed)?),
            "--graphics",
            "none",
            "--noautoconsole",
        ],
    )?;
    Ok(())
}

fn wait_for_agent(run: &mut Run, name: &str, limit: Duration) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if virsh(&["qemu-agent-command", name, r#"{"execute":"guest-ping"}"#]).is_ok() {
            run.note(format!("{name}'s guest agent answers"));
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    Err(format!(
        "{name}'s guest agent didn't answer in {} s",
        limit.as_secs()
    ))
}

/// Take every test VM out of RAMpire and off the host.
fn clean_up(run: &mut Run) {
    let Ok(names) = virsh(&["list", "--all", "--name"]) else {
        return;
    };
    for name in names
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with(PREFIX))
    {
        if management(name).is_some_and(|state| state != "not_managed") {
            let mut term = Term::start(&format!("rampire vm remove {name}"));
            let _ = term.answer_until_exit(run, &answers_remove(), Duration::from_secs(300));
            // Deleting a VM RAMpire still has settings for would leave them
            // naming a VM that is gone, for the next run to trip over.
            if management(name).is_some_and(|state| state != "not_managed") {
                run.fail(
                    format!(
                        "vm remove {name} didn't take it out of RAMpire; it is kept, not deleted"
                    ),
                    Some(term.screen()),
                );
                continue;
            }
        }
        let _ = virsh(&["destroy", name]);
        match virsh(&["undefine", name, "--remove-all-storage", "--nvram"]) {
            Ok(_) => run.note(format!("deleted {name}")),
            Err(error) => run.fail(format!("couldn't delete {name}: {error}"), None),
        }
    }
}

// ---------------------------------------------------------------------
// What RAMpire says about a VM

fn vm_list() -> Option<serde_json::Value> {
    serde_json::from_str(&sudo("rampire", &["vm", "list", "--json"]).ok()?).ok()
}

fn listed(name: &str) -> Option<serde_json::Value> {
    vm_list()?["vms"]
        .as_array()?
        .iter()
        .find(|vm| vm["vm_name"] == name)
        .cloned()
}

fn management(name: &str) -> Option<String> {
    listed(name)?["management"].as_str().map(str::to_owned)
}

/// After a change: the controller runs, the VM is managed, its guest
/// publishes, and its doctor finds nothing wrong.
fn check_healthy(run: &mut Run, name: &str) {
    let active = local("systemctl", &["is-active", "rampire.service"]).unwrap_or_default();
    if active.trim() != "active" {
        run.fail(
            format!("the controller isn't running ({})", active.trim()),
            sudo(
                "journalctl",
                &["-u", "rampire.service", "-n", "15", "--no-pager"],
            )
            .ok(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut last = None;
    while Instant::now() < deadline {
        last = listed(name);
        let published = last
            .as_ref()
            .is_some_and(|vm| vm["guest"]["state"] == "published");
        if published {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    match &last {
        Some(vm) if vm["management"] == "managed" && vm["guest"]["state"] == "published" => {
            run.note(format!("{name} is managed and its guest publishes"));
        }
        Some(vm) => run.fail(format!("{name} isn't managed and publishing: {vm}"), None),
        None => run.fail(format!("vm list has no {name}"), None),
    }
    // doctor exits non-zero when it finds a problem; its JSON says which.
    let output = Command::new("sudo")
        .args(["-n", "rampire", "doctor", name, "--json"])
        .output();
    let Ok(output) = output else {
        run.fail("couldn't run rampire doctor", None);
        return;
    };
    let report: Option<serde_json::Value> = serde_json::from_slice(&output.stdout).ok();
    let problems: Vec<String> = report
        .as_ref()
        .and_then(|report| report["findings"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|finding| finding["ok"] == false)
        .map(|finding| format!("{}: {}", finding["check"], finding["detail"]))
        .collect();
    if report.is_none() {
        run.fail(
            format!(
                "rampire doctor {name} --json gave no report: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
        );
    } else if !problems.is_empty() {
        run.fail(format!("doctor {name}: {}", problems.join("; ")), None);
    }
}

// ---------------------------------------------------------------------
// The screen, through tmux

/// A command in its own tmux session, read and typed into as a person
/// would.
struct Term {
    session: String,
    /// Each question seen, and how often.
    asked: BTreeMap<String, usize>,
}

/// How the suite answers a question, found by how it starts.
#[derive(Clone, Copy)]
enum Answer {
    Yes,
    No,
    /// Yes, but asking it at all is a fault.
    YesButWrong(&'static str),
}

type Answers = Vec<(&'static str, Answer)>;

/// The review questions a VM without RDMA, NVMe or vhost-user passthrough
/// may be asked, and the pool: never changed.
fn answers_common() -> Answers {
    vec![
        ("Is this VM never live-migrated over RDMA", Answer::Yes),
        ("Is none of the VM's passed-through devices", Answer::Yes),
        ("Do all the VM's vhost-user devices", Answer::Yes),
        ("Is the VM's memory balloon never used", Answer::Yes),
        ("Change the pool", Answer::No),
    ]
}

fn answers_add() -> Answers {
    let mut answers = vec![
        ("Add ", Answer::Yes),
        (
            "Shut ",
            Answer::YesButWrong(
                "vm add asked for a restart after applying: the plan should have restarted it",
            ),
        ),
    ];
    answers.extend(answers_common());
    answers
}

fn answers_remove() -> Answers {
    let mut answers = vec![("Remove RAMpire from", Answer::Yes)];
    answers.extend(answers_common());
    answers
}

/// Lines that must never be on the screen: what something wrote to the
/// terminal over the TUI, or a crash.
const NEVER: [&str; 5] = [
    // libvirt's own printing, "libvirt: QEMU Driver error : ...", not an
    // error a task shows that quotes libvirt.
    " error : ",
    "panicked at",
    "Argument list too long",
    "RUST_BACKTRACE",
    "Segmentation fault",
];

impl Term {
    fn start(command: &str) -> Self {
        let session = format!("rampire-live-{}", std::process::id());
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .output();
        let _ = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-x",
                &WIDTH.to_string(),
                "-y",
                "45",
                &format!("sudo -n {command}; echo; echo \"EXITED $?\"; sleep 600"),
            ])
            .output();
        std::thread::sleep(Duration::from_secs(2));
        Self {
            session,
            asked: BTreeMap::new(),
        }
    }

    fn screen(&self) -> String {
        // -J: lines the terminal wrapped come back joined, as written.
        local("tmux", &["capture-pane", "-p", "-J", "-t", &self.session]).unwrap_or_default()
    }

    /// Keys as tmux names them: "Enter", "Escape", "y", or text.
    fn keys(&self, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", self.session.as_str()];
        args.extend_from_slice(keys);
        let _ = local("tmux", &args);
        std::thread::sleep(Duration::from_millis(400));
    }

    fn text(&self, text: &str) {
        let _ = local("tmux", &["send-keys", "-t", &self.session, "-l", text]);
        std::thread::sleep(Duration::from_millis(400));
    }

    /// Fail when the screen has what must never be on it. A row of a
    /// full-screen TUI starts with a frame, a space or the header; a row
    /// starting with anything else was written over it.
    fn check_clean(&self, run: &mut Run, tui: bool) -> bool {
        let screen = self.screen();
        if let Some(bad) = NEVER.iter().find(|never| screen.contains(*never)) {
            run.fail(format!("the screen shows \"{bad}\""), Some(screen));
            return false;
        }
        if tui {
            let rows: Vec<&str> = screen.lines().collect();
            let stray = rows.iter().enumerate().skip(1).find(|(index, row)| {
                *index + 1 < rows.len()
                    && row
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
            });
            if let Some((index, row)) = stray {
                run.fail(
                    format!("row {index} was written over the TUI: {}", row.trim()),
                    Some(screen),
                );
                return false;
            }
        }
        true
    }

    fn wait_for(&self, run: &mut Run, needle: &str, limit: Duration, tui: bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if self.screen().contains(needle) {
                return true;
            }
            if !self.check_clean(run, tui) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        run.fail(
            format!("\"{needle}\" didn't show in {} s", limit.as_secs()),
            Some(self.screen()),
        );
        false
    }

    /// The question on the screen now, if one is: the TUI's (a line marked
    /// ▍ above the No and Yes buttons) or a command's ([y/N]).
    fn question(&self) -> Option<(String, bool)> {
        let screen = self.screen();
        if screen.contains("Yes (y)") {
            return tui_question(&screen).map(|question| (question, true));
        }
        cli_question(&screen).map(|question| (question, false))
    }

    /// Answer what is asked now, from `answers`; whether a question was
    /// there. One asked twice, or not in `answers`, is a failure.
    fn answer(&mut self, run: &mut Run, answers: &Answers) -> bool {
        let Some((question, tui)) = self.question() else {
            return false;
        };
        let count = self.asked.entry(question.clone()).or_default();
        *count += 1;
        if *count == 2 {
            run.fail(format!("asked twice: {question}"), Some(self.screen()));
        }
        let answer = answers
            .iter()
            .find(|(start, _)| question.starts_with(start))
            .map(|(_, answer)| *answer);
        let yes = match answer {
            Some(Answer::Yes) => true,
            Some(Answer::No) => false,
            Some(Answer::YesButWrong(why)) => {
                run.fail(format!("{why}: {question}"), Some(self.screen()));
                true
            }
            None => {
                run.fail(
                    format!("unexpected question: {question}"),
                    Some(self.screen()),
                );
                false
            }
        };
        run.note(format!(
            "asked \"{question}\": {}",
            if yes { "yes" } else { "no" }
        ));
        if tui {
            self.keys(&[if yes { "y" } else { "n" }]);
        } else {
            self.keys(&[if yes { "y" } else { "n" }, "Enter"]);
        }
        std::thread::sleep(Duration::from_secs(1));
        true
    }

    /// A command: answer its questions until it exits; its exit code.
    fn answer_until_exit(
        &mut self,
        run: &mut Run,
        answers: &Answers,
        limit: Duration,
    ) -> Result<i32, String> {
        let started = Instant::now();
        while started.elapsed() < limit {
            let screen = self.screen();
            if let Some(code) = screen
                .lines()
                .find_map(|line| line.trim().strip_prefix("EXITED "))
            {
                return code.trim().parse().map_err(|_| "no exit code".to_owned());
            }
            self.check_clean(run, false);
            if !self.answer(run, answers) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        Err(format!("still running after {} s", limit.as_secs()))
    }

    /// A TUI task: answer its questions until its window says how it ended.
    /// The task window must stay open the whole time.
    fn answer_task(
        &mut self,
        run: &mut Run,
        title: &str,
        answers: &Answers,
        limit: Duration,
    ) -> Option<String> {
        let started = Instant::now();
        while started.elapsed() < limit {
            if !self.check_clean(run, true) {
                return None;
            }
            let screen = self.screen();
            let Some(header) = screen.lines().find(|line| line.contains(title)) else {
                run.fail(
                    format!("the task window for \"{title}\" closed while it ran"),
                    Some(screen),
                );
                return None;
            };
            for end in ["Done in", "Not done", "Failed after"] {
                if header.contains(end) {
                    return Some(end.to_owned());
                }
            }
            if !self.answer(run, answers) {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        run.fail(
            format!("\"{title}\" still running after {} s", limit.as_secs()),
            Some(self.screen()),
        );
        None
    }

    /// The TUI's errors list must be empty of what was written to stderr.
    fn check_errors_list(&self, run: &mut Run) {
        self.keys(&["Escape"]);
        self.keys(&["Escape"]);
        self.keys(&["e"]);
        let screen = self.screen();
        if screen.contains("a message on stderr") {
            run.fail(
                "something wrote to stderr while the TUI was open",
                Some(screen),
            );
        }
        self.keys(&["Escape"]);
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &self.session])
            .output();
    }
}

/// A command's question on `screen`: its last line, ending `[y/N]`.
/// The screen is captured with the lines the terminal wrapped joined, so a
/// long question is one line, and what was printed before it isn't.
fn cli_question(screen: &str) -> Option<String> {
    let line = screen.lines().rev().find(|line| !line.trim().is_empty())?;
    let line = line.trim();
    line.ends_with("[y/N]")
        .then(|| line.trim_end_matches("[y/N]").trim().to_owned())
}

/// The terminal's width in tmux, in columns.
const WIDTH: usize = 150;

/// The TUI's question on `screen`: the line marked ▍ nearest above the No
/// and Yes buttons. The status bar's "▍ 1 asks (w)", below them, isn't it.
fn tui_question(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let buttons = lines.iter().rposition(|line| line.contains("Yes (y)"))?;
    let line = lines[..buttons]
        .iter()
        .rev()
        .find(|line| line.contains("▍ "))?;
    // Up to the window's edge: the panel behind it goes on past.
    let text = line.split("▍ ").nth(1)?.split('│').next()?;
    Some(text.trim().to_owned())
}

/// Choose `vm` in the TUI's list, by filtering for its name, once the list
/// has it. Each key goes alone: Escape and the key after it, sent
/// together, read as Alt and that key.
fn choose(run: &mut Run, term: &Term, vm: &str) -> bool {
    if !term.wait_for(run, &format!(" {vm} "), Duration::from_secs(60), true) {
        return false;
    }
    term.keys(&["Escape"]);
    term.keys(&["/"]);
    term.text(vm);
    term.keys(&["Enter"]);
    true
}

/// Add `vm` in the TUI as a person does: its menu's Add, the form, then
/// the questions. The task's window must open at once.
fn add_in_tui(run: &mut Run, vm: &str, sizes: Option<(&str, &str)>) -> Result<(), String> {
    let (mut tui, title) = start_add(run, vm, sizes)?;
    match tui
        .answer_task(run, &title, &answers_add(), Duration::from_secs(900))
        .as_deref()
    {
        Some("Done in") => run.note(format!("{vm} added")),
        Some(end) => run.fail(format!("vm add {vm} ended {end}"), Some(tui.screen())),
        None => {}
    }
    tui.check_errors_list(run);
    tui.keys(&["q"]);
    Ok(())
}

/// Open the TUI, choose `vm`, press Add, type `sizes` (Minimum and
/// Maximum RAM) over the suggested ones when given, and save the form: the
/// task's window must open at once. The TUI and the task's title.
fn start_add(
    run: &mut Run,
    vm: &str,
    sizes: Option<(&str, &str)>,
) -> Result<(Term, String), String> {
    let tui = Term::start("rampire");
    if !tui.wait_for(run, "VMS", Duration::from_secs(30), true) {
        return Err("the TUI didn't open".to_owned());
    }
    if !choose(run, &tui, vm) {
        return Err(format!("the TUI's list never showed {vm}"));
    }
    tui.keys(&["a"]);
    if !tui.wait_for(run, &format!("Add {vm}"), Duration::from_secs(10), true) {
        return Err("the Add form didn't open".to_owned());
    }
    if let Some((min, max)) = sizes {
        // Priority, then Minimum RAM, then Maximum RAM.
        for size in [min, max] {
            tui.keys(&["Tab"]);
            tui.keys(&["C-a"]);
            tui.keys(&["C-k"]);
            tui.text(size);
        }
        run.note(format!("typed Minimum RAM {min} and Maximum RAM {max}"));
    }
    tui.keys(&["Enter"]);
    let title = format!("rampire vm add {vm}");
    if !tui.wait_for(run, &title, Duration::from_secs(3), true) {
        return Err("the task's window didn't open after the form".to_owned());
    }
    Ok((tui, title))
}

/// The VM's saved definition, to compare before and after.
fn saved_definition(vm: &str) -> Result<String, String> {
    virsh(&["dumpxml", "--inactive", vm])
}

// ---------------------------------------------------------------------
// The scenarios

/// No to adding: nothing changes, and the task says Not done.
fn add_declined(run: &mut Run) -> Result<(), String> {
    let vm = test_vm(run, System::Debian)?;
    let before = saved_definition(&vm)?;
    let (mut tui, title) = start_add(run, &vm, None)?;
    let mut answers = vec![("Add ", Answer::No)];
    answers.extend(answers_common());
    match tui
        .answer_task(run, &title, &answers, Duration::from_secs(300))
        .as_deref()
    {
        Some("Not done") => run.note("No added nothing, and the task says Not done"),
        Some(end) => run.fail(
            format!("vm add answered No ended {end}"),
            Some(tui.screen()),
        ),
        None => {}
    }
    tui.keys(&["q"]);
    if saved_definition(&vm)? != before {
        run.fail(
            format!("{vm}'s definition changed though the add was declined"),
            None,
        );
    }
    if management(&vm).as_deref() != Some("not_managed") {
        run.fail(format!("{vm} is managed though the add was declined"), None);
    }
    Ok(())
}

/// The add cancelled as soon as it is under way. However far it got, the
/// VM ends whole: managed and healthy, or not managed with its old
/// definition back and nothing of RAMpire's kept.
fn add_cancelled(run: &mut Run) -> Result<(), String> {
    let vm = test_vm(run, System::Debian)?;
    let before = saved_definition(&vm)?;
    let (mut tui, title) = start_add(run, &vm, None)?;
    let mut answers = answers_add();
    // Asked once the controller has the VM, when cancelling came too late.
    answers.push(("Shut ", Answer::Yes));
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if tui.answer(run, &answers) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !tui.wait_for(run, "Working:", Duration::from_secs(30), true) {
        return Err("the add didn't start working after Yes".to_owned());
    }
    tui.keys(&["x"]);
    run.note("cancelled the add while it was working");
    let end = tui.answer_task(run, &title, &answers, Duration::from_secs(900));
    tui.keys(&["q"]);
    match (end.as_deref(), management(&vm).as_deref()) {
        (Some(_), Some("managed")) => {
            run.note(format!("the add got as far as managing {vm}"));
            check_healthy(run, &vm);
            let mut term = Term::start(&format!("rampire vm remove {vm}"));
            term.answer_until_exit(run, &answers_remove(), Duration::from_secs(300))?;
        }
        (Some(_), Some("not_managed")) => {
            if saved_definition(&vm)? != before {
                run.fail(
                    format!("{vm} isn't managed after the cancelled add, but its definition wasn't put back"),
                    None,
                );
            } else {
                run.note(format!("{vm} is as it was before the add"));
            }
        }
        (Some(_), other) => run.fail(
            format!("{vm} is {other:?} after the cancelled add: half-added"),
            None,
        ),
        (None, _) => {}
    }
    Ok(())
}

/// Added with sizes of its own, not the VM's present RAM: it must come
/// back starting with its minimum, its device holding the rest.
fn add_running(run: &mut Run) -> Result<(), String> {
    let vm = test_vm(run, System::Debian)?;
    // The test VMs are made with 1536 MiB.
    add_in_tui(run, &vm, Some(("1GB", "3GB")))?;
    check_healthy(run, &vm);
    let saved = saved_definition(&vm)?;
    let boots_with_minimum = saved.contains("memory='1048576' unit='KiB'");
    let device_holds_rest = saved.contains("<size unit='KiB'>2097152</size>");
    if boots_with_minimum && device_holds_rest {
        run.note(format!("{vm} starts with 1 GiB and its device holds 2 GiB"));
    } else {
        run.fail(
            format!("{vm}'s definition isn't fitted to 1GB to 3GB (cell 1 GiB: {boots_with_minimum}, device 2 GiB: {device_holds_rest})"),
            Some(saved),
        );
    }
    Ok(())
}

fn add_off(run: &mut Run) -> Result<(), String> {
    let vm = test_vm(run, System::Rocky)?;
    virsh(&["shutdown", &vm])?;
    let deadline = Instant::now() + Duration::from_secs(180);
    while virsh(&["domstate", &vm])?.trim() != "shut off" {
        if Instant::now() > deadline {
            virsh(&["destroy", &vm])?;
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    add_in_tui(run, &vm, None)?;
    check_healthy(run, &vm);
    Ok(())
}

/// A managed VM restarted from outside while the TUI is open: nothing is
/// written over the screen, and it reports in again.
fn restart_open(run: &mut Run) -> Result<(), String> {
    let vm = System::Debian.vm();
    if management(&vm).as_deref() != Some("managed") {
        return Err(format!("{vm} isn't managed: run add-running first"));
    }
    let tui = Term::start("rampire");
    tui.wait_for(run, "VMS", Duration::from_secs(30), true);
    // Only RAMpire is at work here: with the TUI open it once asked polkit
    // several times a second, which ran polkit out of descriptors, and it
    // then refused the controller.
    let polkit_before = polkit_descriptors();
    virsh(&["reboot", &vm])?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(120) {
        if !tui.check_clean(run, true) {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if let (Some(before), Some(after)) = (polkit_before, polkit_descriptors()) {
        let grown = after.saturating_sub(before);
        run.note(format!(
            "polkitd's open descriptors grew by {grown} in 2 minutes of the TUI"
        ));
        if grown > 40 {
            run.fail(
                format!("polkitd's open descriptors grew by {grown} in 2 minutes with the TUI open: it asks polkit far too often"),
                None,
            );
        }
    }
    tui.check_errors_list(run);
    tui.keys(&["q"]);
    check_healthy(run, &vm);
    Ok(())
}

/// A new minimum set with the command: the definition is fitted to it,
/// the restart it takes is asked and done, and the VM comes back starting
/// with it, healthy.
fn settings(run: &mut Run) -> Result<(), String> {
    let vm = System::Debian.vm();
    let command = format!("rampire set {vm} min_ram=1536MB");
    let mut term = Term::start(&command);
    let mut answers = vec![
        ("Apply", Answer::Yes),
        ("That takes a restart", Answer::Yes),
    ];
    answers.extend(answers_common());
    let code = term.answer_until_exit(run, &answers, Duration::from_secs(600))?;
    if code != 0 {
        run.fail(format!("{command} exited {code}"), Some(term.screen()));
    }
    check_healthy(run, &vm);
    let saved = saved_definition(&vm)?;
    if saved.contains("memory='1572864' unit='KiB'") {
        run.note(format!("{vm} now starts with 1536 MiB"));
    } else {
        run.fail(
            format!("{vm}'s definition doesn't start it with 1536 MiB after {command}"),
            Some(saved),
        );
    }
    Ok(())
}

/// The guest service upgraded to this build in a VM that has it: already
/// so, nothing asked that shouldn't be.
fn upgrade(run: &mut Run) -> Result<(), String> {
    let vm = System::Debian.vm();
    let mut term = Term::start(&format!("rampire vm upgrade {vm}"));
    let mut answers = vec![("Upgrade", Answer::Yes)];
    answers.extend(answers_common());
    let code = term.answer_until_exit(run, &answers, Duration::from_secs(600))?;
    if code != 0 {
        run.fail(
            format!("vm upgrade {vm} exited {code}"),
            Some(term.screen()),
        );
    }
    check_healthy(run, &vm);
    Ok(())
}

/// A managed VM deleted from libvirt behind RAMpire's back: the controller
/// still starts and manages the others, and `vm remove` takes it out.
fn vm_gone(run: &mut Run) -> Result<(), String> {
    let vm = System::Rocky.vm();
    if management(&vm).as_deref() != Some("managed") {
        return Err(format!("{vm} isn't managed: run add-off first"));
    }
    let _ = virsh(&["destroy", &vm]);
    virsh(&["undefine", &vm, "--remove-all-storage", "--nvram"])?;
    run.note(format!(
        "deleted {vm} from libvirt while RAMpire manages it"
    ));
    sudo("systemctl", &["restart", "rampire.service"])?;
    std::thread::sleep(Duration::from_secs(10));
    let active = local("systemctl", &["is-active", "rampire.service"]).unwrap_or_default();
    if active.trim() != "active" {
        run.fail(
            "one VM libvirt no longer has stopped the controller for every VM",
            sudo(
                "journalctl",
                &["-u", "rampire.service", "-n", "10", "--no-pager"],
            )
            .ok(),
        );
    }
    let log = sudo(
        "journalctl",
        &[
            "-u",
            "rampire.service",
            "-n",
            "50",
            "--no-pager",
            "-o",
            "cat",
        ],
    )
    .unwrap_or_default();
    if !log.contains(&format!("vm={vm} event=vm_setup_failed")) {
        run.fail(
            format!("the controller didn't log vm_setup_failed for {vm}"),
            Some(log),
        );
    }
    let mut term = Term::start(&format!("rampire vm remove {vm}"));
    let code = term.answer_until_exit(run, &answers_remove(), Duration::from_secs(300))?;
    if code != 0 || listed(&vm).is_some() {
        run.fail(
            format!("vm remove of {vm}, gone from libvirt, exited {code} and left it listed"),
            Some(term.screen()),
        );
    } else {
        run.note(format!("vm remove took {vm} out of RAMpire"));
    }
    Ok(())
}

/// Each test VM removed in the TUI: not managed after, its definition
/// plain again.
fn remove(run: &mut Run) -> Result<(), String> {
    for system in [System::Debian, System::Rocky] {
        let vm = system.vm();
        if management(&vm).as_deref() != Some("managed") {
            continue;
        }
        let mut tui = Term::start("rampire");
        tui.wait_for(run, "VMS", Duration::from_secs(30), true);
        if !choose(run, &tui, &vm) {
            continue;
        }
        tui.keys(&["Enter"]);
        tui.keys(&["x"]);
        let title = format!("rampire vm remove {vm}");
        if tui.wait_for(run, &title, Duration::from_secs(10), true) {
            match tui
                .answer_task(run, &title, &answers_remove(), Duration::from_secs(300))
                .as_deref()
            {
                Some("Done in") => run.note(format!("{vm} removed")),
                Some(end) => run.fail(format!("vm remove {vm} ended {end}"), Some(tui.screen())),
                None => {}
            }
        }
        tui.check_errors_list(run);
        tui.keys(&["q"]);
        match management(&vm).as_deref() {
            Some("not_managed") => {}
            other => run.fail(format!("{vm} is {other:?} after remove"), None),
        }
        let xml = virsh(&["dumpxml", "--inactive", &vm])?;
        if xml.contains("virtio-mem") || xml.contains("org.rampire.control") {
            run.fail(format!("{vm}'s definition still has RAMpire's parts"), None);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------

fn write_report(run: &Run) -> Result<PathBuf, String> {
    let dir = run.repo.join("target/live");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join("report.md");
    let mut text = format!("# Live suite\n\n{} failure(s)\n\n", run.failures.len());
    for failure in &run.failures {
        let _ = writeln!(text, "## {}: {}\n", failure.scenario, failure.what);
        if let Some(screen) = &failure.screen {
            let _ = writeln!(text, "```text\n{}\n```\n", screen.trim_end());
        }
    }
    let _ = writeln!(text, "## Log\n\n```text\n{}```", run.log);
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    Ok(path)
}

fn text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} isn't UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_run_is_refused_while_one_runs() {
        let dir = std::env::temp_dir().join(format!("rampire-live-lock-{}", std::process::id()));
        let path = dir.join("lock");
        let first = Lock::take(&path).unwrap();
        // This process is alive: a second run is refused.
        assert!(Lock::take(&path).is_err());
        drop(first);
        assert!(!path.exists());
        // A lock left by a process that is gone is taken over.
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "999999999").unwrap();
        drop(Lock::take(&path).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scenarios_and_options_are_read() {
        let options = parse(&["--keep".to_owned(), "add-running".to_owned()]).unwrap();
        assert!(options.keep && options.install);
        assert_eq!(options.only, ["add-running"]);
        assert!(parse(&["nonsense".to_owned()]).is_err());
    }

    #[test]
    fn a_command_question_is_its_last_line() {
        // capture-pane -J has joined what the terminal wrapped.
        let screen = "  tv101: 1524 MiB (boot RAM)\nRemove RAMpire from web? It stops managing it, and its definition has plain RAM again, for Cockpit or virsh to change. [y/N] \n\n";
        assert_eq!(
            cli_question(screen).as_deref(),
            Some("Remove RAMpire from web? It stops managing it, and its definition has plain RAM again, for Cockpit or virsh to change.")
        );
        assert_eq!(cli_question("working...\n"), None);
    }

    #[test]
    fn a_tui_question_is_read_off_the_screen() {
        // As capture-pane gives the task window, with the status bar's
        // gauge, marked the same way, below it.
        let screen = "│ │ ▍ Add rampire-test-debian? RAMpire then grows it.     │ │───────╮\n\
                      │ │    ▶ No (n) ◀       Yes (y)      Enter answers No    │ │\n\
                        y  yes   n  no                       ━━━━ ▍ 1 asks (w)";
        assert_eq!(
            tui_question(screen).as_deref(),
            Some("Add rampire-test-debian? RAMpire then grows it.")
        );
    }
}
