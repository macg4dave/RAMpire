//! The TUI's work off the screen's thread, so keys answer at once however
//! slow libvirt or the journal is: a reader that refreshes the pool and the
//! chosen VM and watches for other people's changes, one-off loads (the VM
//! list, the doctor, the journal), the keys, and the guided flows, each run
//! as a task whose questions come back to the screen.
//!
//! A send fails only once the screen has closed, when nothing is left to
//! tell; those failures are let go.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::Event;

use super::peers::{self, PeerChange};
use super::vm::VmDetails;
use crate::context::Context;
use crate::doctor::Finding;
use crate::reports::{ControllerUnit, VmListReport};
use crate::top::{PoolReader, PoolSnapshot};
use crate::ui::Ui;
use crate::watch::{VmReader, VmSnapshot};

/// A task's number, from 1 in the order they were started.
pub type TaskId = u64;

/// Everything the background work tells the screen.
pub enum Msg {
    Pool(Result<PoolSnapshot, String>),
    Vm(String, Box<Result<VmSnapshot, String>>),
    Details(String, VmDetails),
    Controller(ControllerUnit),
    Inventory(Result<VmListReport, String>),
    /// The host's findings and each managed VM's.
    Doctor(Result<Vec<DoctorSection>, String>),
    Log(Result<String, String>),
    Task(TaskId, FlowEvent),
    /// Changes other people made.
    Peers(Vec<PeerChange>),
    /// Whether the host is set up: if not, its memory and its VMs'.
    Setup(Result<Option<(u64, Vec<crate::init::DomainMemory>)>, String>),
    /// A key, a paste or a resize.
    Term(Event),
    /// The terminal can't be read any more.
    Gone(String),
}

/// One part of the doctor screen: the host (`vm` is `None`) or one VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorSection {
    pub vm: Option<String>,
    pub findings: Vec<Finding>,
}

/// What a running flow says.
pub enum FlowEvent {
    Line(String),
    Shown(String),
    Ask(Question, Sender<Answer>),
    /// The flow ended: what it last said, or its error.
    Done(Result<Ended, String>),
}

/// How a flow that didn't fail ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// It did what it was started for.
    Done(String),
    /// It stopped without doing all of it: No was answered, or a part is
    /// left. The text says what was and wasn't changed.
    NotDone(String),
}

impl Ended {
    pub fn text(&self) -> &str {
        match self {
            Self::Done(text) | Self::NotDone(text) => text,
        }
    }
}

impl From<String> for Ended {
    fn from(text: String) -> Self {
        Self::Done(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// Yes or no; `--yes` would answer it.
    Confirm(String),
    /// Yes or no, for a person only: a review question.
    Review(String),
    /// A short text.
    Text(String),
    /// A value, filled in with the suggested one to keep or change.
    Edit(String, String),
}

impl Question {
    pub fn text(&self) -> &str {
        match self {
            Self::Confirm(text) | Self::Review(text) | Self::Text(text) | Self::Edit(text, _) => {
                text
            }
        }
    }

    /// Answered by typing, not yes or no.
    pub fn typed(&self) -> bool {
        matches!(self, Self::Text(_) | Self::Edit(..))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Text(String),
    /// The dialog was closed without an answer.
    Cancelled,
}

/// A [`Ui`] whose answers come from the TUI, for one task.
pub struct ChannelUi {
    tx: Sender<Msg>,
    id: TaskId,
}

impl ChannelUi {
    pub fn new(tx: Sender<Msg>, id: TaskId) -> Self {
        Self { tx, id }
    }

    fn send(&self, event: FlowEvent) {
        let _ = self.tx.send(Msg::Task(self.id, event));
    }

    fn ask(&mut self, question: Question) -> Result<Answer, String> {
        let (reply, answer) = mpsc::channel();
        self.tx
            .send(Msg::Task(self.id, FlowEvent::Ask(question, reply)))
            .map_err(|_| "the TUI closed".to_owned())?;
        match answer.recv() {
            Ok(Answer::Cancelled) | Err(_) => {
                Err("cancelled in the TUI; nothing more was changed".to_owned())
            }
            Ok(answer) => Ok(answer),
        }
    }
}

impl Ui for ChannelUi {
    fn say(&mut self, line: &str) {
        self.send(FlowEvent::Line(line.to_owned()));
    }

    fn show(&mut self, text: &str) {
        self.send(FlowEvent::Shown(text.to_owned()));
    }

    fn confirm(&mut self, question: &str) -> Result<bool, String> {
        Ok(self.ask(Question::Confirm(question.to_owned()))? == Answer::Yes)
    }

    fn review(&mut self, question: &str) -> Result<bool, String> {
        Ok(self.ask(Question::Review(question.to_owned()))? == Answer::Yes)
    }

    fn text(&mut self, question: &str) -> Result<String, String> {
        match self.ask(Question::Text(question.to_owned()))? {
            Answer::Text(text) => Ok(text),
            _ => Ok(String::new()),
        }
    }

    fn edit(&mut self, question: &str, suggested: &str) -> Result<String, String> {
        match self.ask(Question::Edit(question.to_owned(), suggested.to_owned()))? {
            Answer::Text(text) if !text.trim().is_empty() => Ok(text),
            _ => Ok(suggested.to_owned()),
        }
    }
}

/// Run task `id`'s flow on its own thread, its questions and lines sent to
/// the screen, and its end as [`FlowEvent::Done`].
pub fn spawn_flow(
    tx: Sender<Msg>,
    id: TaskId,
    flow: impl FnOnce(&mut ChannelUi) -> Result<Ended, String> + Send + 'static,
) {
    std::thread::spawn(move || {
        let mut ui = ChannelUi::new(tx, id);
        let result = flow(&mut ui);
        ui.send(FlowEvent::Done(result));
    });
}

/// Read the keys on their own thread, so the screen sleeps until there is
/// something to do. It ends when the terminal goes.
pub fn spawn_keys(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        match ratatui::crossterm::event::read() {
            Ok(event) => {
                if tx.send(Msg::Term(event)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(Msg::Gone(error.to_string()));
                return;
            }
        }
    });
}

/// Run `load` on its own thread and send what it gives.
pub fn spawn_load(tx: Sender<Msg>, load: impl FnOnce() -> Msg + Send + 'static) {
    std::thread::spawn(move || {
        let _ = tx.send(load());
    });
}

/// The doctor for the host and every managed VM, problems first within
/// each part.
pub fn doctor(context: &Context) -> Result<Vec<DoctorSection>, String> {
    let mut host = crate::doctor::run(
        &context.connection()?,
        crate::context::SERVICE_ACCOUNT,
        crate::context::applied_root(),
    );
    host.sort_by_key(|finding| finding.ok);
    let mut sections = vec![DoctorSection {
        vm: None,
        findings: host,
    }];
    let known = crate::inventory::Known::read(context)?;
    for vm in &known.managed {
        let mut findings = crate::doctor_vm::run(context, vm).unwrap_or_else(|error| {
            vec![Finding::problem(
                "doctor",
                error,
                format!("rampire doctor {vm}"),
            )]
        });
        findings.sort_by_key(|finding| finding.ok);
        sections.push(DoctorSection {
            vm: Some(vm.clone()),
            findings,
        });
    }
    // Sections with problems first; the host stays first among equals.
    sections.sort_by_key(|section| section.findings.iter().all(|finding| finding.ok));
    Ok(sections)
}

/// The doctor for one VM, managed or not, problems first.
pub fn doctor_one(context: &Context, vm: &str) -> Result<Vec<DoctorSection>, String> {
    let mut findings = crate::doctor_vm::run(context, vm)?;
    findings.sort_by_key(|finding| finding.ok);
    Ok(vec![DoctorSection {
        vm: Some(vm.to_owned()),
        findings,
    }])
}

/// What the reader is asked to read besides the pool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wanted {
    /// The VM whose details are shown.
    pub vm: Option<String>,
}

/// The reader's handles: what to read, and when to stop or reload.
#[derive(Clone)]
pub struct ReaderControl {
    pub wanted: Arc<Mutex<Wanted>>,
    pub reload: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
}

impl ReaderControl {
    pub fn new() -> Self {
        Self {
            wanted: Arc::new(Mutex::new(Wanted::default())),
            reload: Arc::new(AtomicBool::new(true)),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn want_vm(&self, vm: Option<String>) {
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.vm = vm;
        }
    }
}

impl Default for ReaderControl {
    fn default() -> Self {
        Self::new()
    }
}

/// How often the slower facts are read again: the configuration, the
/// controller's state and a VM's settings.
const SLOW_EVERY: Duration = Duration::from_secs(5);

/// Refresh the pool and the wanted VM every `interval`, and look for other
/// people's changes with the slower facts, until stopped.
pub fn spawn_reader(context: Context, control: ReaderControl, tx: Sender<Msg>, interval: Duration) {
    std::thread::spawn(move || {
        let cwd = Path::new("/");
        let mut pool: Option<PoolReader> = None;
        let mut vm: Option<VmReader> = None;
        let mut slow: Option<Instant> = None;
        let mut watch = peers::Watch::new();
        while !control.stop.load(Ordering::Acquire) {
            let started = Instant::now();
            let reload = control.reload.swap(false, Ordering::AcqRel);
            let slow_due = reload || slow.is_none_or(|at| at.elapsed() >= SLOW_EVERY);
            if slow_due {
                slow = Some(started);
                let news = watch.poll();
                if !news.is_empty() {
                    let _ = tx.send(Msg::Peers(news));
                }
                let _ = tx.send(Msg::Controller(crate::systemd_unit::controller_state()));
                match crate::top::Command::load(&context) {
                    Ok(command) => {
                        if pool
                            .as_ref()
                            .is_none_or(|reader| reader.command() != &command)
                        {
                            pool = Some(PoolReader::new(command));
                        }
                    }
                    Err(error) => {
                        pool = None;
                        let _ = tx.send(Msg::Pool(Err(error)));
                    }
                }
            }
            if let Some(reader) = pool.as_mut() {
                let _ = tx.send(Msg::Pool(reader.read(cwd)));
            }
            let wanted = control
                .wanted
                .lock()
                .map(|wanted| wanted.clone())
                .unwrap_or_default();
            match &wanted.vm {
                None => vm = None,
                Some(name) => {
                    let changed = vm
                        .as_ref()
                        .is_none_or(|reader| &reader.command().vm != name);
                    if changed || slow_due {
                        match crate::watch::Command::load(&context, Some(name)) {
                            Ok(command) => {
                                if vm
                                    .as_ref()
                                    .is_none_or(|reader| reader.command() != &command)
                                {
                                    vm = Some(VmReader::new(command));
                                }
                            }
                            Err(error) => {
                                vm = None;
                                let _ = tx.send(Msg::Vm(name.clone(), Box::new(Err(error))));
                            }
                        }
                        let _ = tx.send(Msg::Details(name.clone(), details(&context, name)));
                    }
                    if let Some(reader) = vm.as_mut() {
                        let _ = tx.send(Msg::Vm(name.clone(), Box::new(reader.read(cwd))));
                    }
                }
            }
            while !control.stop.load(Ordering::Acquire)
                && !control.reload.load(Ordering::Acquire)
                && started.elapsed() < interval
            {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    });
}

/// A VM's settings as the controller runs them, its boot RAM and its state.
pub fn details(context: &Context, vm: &str) -> VmDetails {
    let settings = crate::attest::vm_settings(&context.running(), vm).ok();
    let libvirt = context.libvirt().ok();
    let boot_bytes = libvirt
        .as_ref()
        .and_then(|libvirt| libvirt.virsh(&["dumpxml", "--inactive", vm]).ok())
        .and_then(|xml| crate::vm::inspect(&xml).ok())
        .map(|inspection| inspection.boot_bytes)
        .filter(|bytes| *bytes > 0);
    let state = libvirt
        .and_then(|libvirt| libvirt.state(vm).ok())
        .unwrap_or_else(|| "unknown".to_owned());
    VmDetails {
        settings,
        boot_bytes,
        state,
    }
}

/// A channel for the TUI's messages.
pub fn channel() -> (Sender<Msg>, Receiver<Msg>) {
    mpsc::channel()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flows_questions_come_back_as_dialogs_and_their_answers_go_on() {
        let (tx, rx) = channel();
        spawn_flow(tx, 1, |ui| {
            ui.say("checking");
            if !ui.confirm("Apply this?")? {
                return Ok("nothing changed".to_owned().into());
            }
            let kernel = ui.text("The guest's kernel release:")?;
            Ok(format!("kernel {kernel}").into())
        });
        let mut said = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Msg::Task(1, FlowEvent::Line(line)) => said.push(line),
                Msg::Task(1, FlowEvent::Ask(Question::Confirm(question), reply)) => {
                    said.push(question);
                    reply.send(Answer::Yes).unwrap();
                }
                Msg::Task(1, FlowEvent::Ask(Question::Text(question), reply)) => {
                    said.push(question);
                    reply.send(Answer::Text("6.12".to_owned())).unwrap();
                }
                Msg::Task(1, FlowEvent::Done(result)) => {
                    said.push(result.unwrap().text().to_owned());
                    break;
                }
                _ => panic!("unexpected message"),
            }
        }
        assert_eq!(
            said,
            [
                "checking",
                "Apply this?",
                "The guest's kernel release:",
                "kernel 6.12"
            ]
        );
    }

    #[test]
    fn closing_a_question_stops_the_flow_with_nothing_more_changed() {
        let (tx, rx) = channel();
        spawn_flow(tx, 1, |ui| {
            ui.confirm("Apply this?")
                .map(|_| "applied".to_owned().into())
        });
        let error = loop {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Msg::Task(1, FlowEvent::Ask(_, reply)) => reply.send(Answer::Cancelled).unwrap(),
                Msg::Task(1, FlowEvent::Done(result)) => break result.unwrap_err(),
                _ => {}
            }
        };
        assert!(error.contains("nothing more was changed"), "{error}");
    }
}
