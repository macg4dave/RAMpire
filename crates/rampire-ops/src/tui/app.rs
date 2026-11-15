//! The TUI's state: what has been read, what is shown, what is open over
//! it, and the tasks. Keys reach it through [`super::router`]; it is drawn
//! by [`super::views`]; the work off the screen's thread is in
//! [`super::jobs`].
//!
//! Everything starts from one screen: the pool and every VM on the host,
//! managed or not. Enter on a VM opens its menu, which offers what can be
//! done with it: its details, settings, doctor and logs, restarting it,
//! removing it, or adding it when RAMpire doesn't manage it yet. What it
//! does runs as a task, so the screen stays free.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::actions::Action;
use super::dialog::{Confirm, Dialog, OnYes};
use super::form::{Field, Form};
use super::help::Help;
use super::input::{History, Input};
use super::jobs::{self, Answer, DoctorSection, FlowEvent, Msg, Question, ReaderControl, TaskId};
use super::keymap::{self, Cmd, Key, Layer, Scope};
use super::list::{ListState, SortBy};
use super::notify::{Message, Notify};
use super::palette::{CommandPalette, Item};
use super::scroll::Scroll;
use super::spot::Spot;
use super::tasks::{Pending, Tasks};
use super::theme::{Palette, Theme};
use super::vm::VmDetails;
use super::which::{Cand, Which};
use crate::context::Context;
use crate::reports::{ControllerUnit, ListedVm, Management, VmListReport};
use crate::tone::Tone;
use crate::top::PoolSnapshot;
use crate::watch::VmSnapshot;
use rampire_core::format_byte_size;

/// What fills the screen below the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// The pool and every VM.
    Main,
    /// One managed VM live, as `rampire watch` shows it.
    Details(String),
    /// The doctor: for the host and every VM, or for one VM.
    Doctor(Option<String>),
    /// The journal, all of it or one VM's lines.
    Logs(Option<String>),
}

/// Something read once, on request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded<T> {
    NotYet,
    Loading,
    Done(Result<T, String>),
}

impl<T> Loaded<T> {
    pub fn ok(&self) -> Option<&T> {
        match self {
            Self::Done(Ok(value)) => Some(value),
            _ => None,
        }
    }
}

/// What choosing a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Run an action as a task.
    Run(Action),
    /// Show a view.
    Open(View),
    /// The settings form for these VMs.
    Settings(Vec<String>),
}

/// One item of a VM's menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub key: char,
    pub label: String,
    /// The command that does the same.
    pub command: String,
    pub pick: Pick,
}

/// Anything a key, a menu, the palette or the help can lead to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Cmd(Cmd),
    Pick(Pick),
    /// Copy this to the clipboard.
    Copy(String),
    Theme(Palette),
}

/// What is open over the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    Tasks,
    /// One task, opened from the tasks list or come forward by itself.
    Task {
        id: TaskId,
        from_list: bool,
    },
    Spot(Spot),
    Errors(Scroll),
}

/// A filter or a find being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub input: Input,
    /// What it was before, put back on Esc.
    pub before: Option<String>,
}

/// An error kept until a person has seen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEntry {
    pub time: String,
    pub what: String,
    pub error: String,
}

/// What the screen's loop does for the app, which can't from inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Put this on the clipboard.
    Copy(String),
    /// Ctrl+Z.
    Suspend,
}

/// One VM in the main list.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub management: Management,
    /// Its place in the pool, for a managed VM the pool shows.
    pub pool_index: Option<usize>,
    /// What `vm list` says about it, once read.
    pub listed: Option<ListedVm>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorState {
    pub loaded: Loaded<Vec<DoctorSection>>,
    /// Whose doctor is loaded: the host and every VM, or one VM.
    pub of: Option<Option<String>>,
    pub scroll: Scroll,
    pub filter: Option<String>,
    pub problems_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogState {
    pub loaded: Loaded<String>,
    pub of: Option<Option<String>>,
    pub scroll: Scroll,
    /// The text found: its lines are marked, and n and N go between them.
    pub find: Option<String>,
}

/// How often the list of the host's VMs is read again by itself.
const INVENTORY_EVERY: Duration = Duration::from_secs(30);
/// The header's clock ticks.
const CLOCK: Duration = Duration::from_secs(1);

pub struct App {
    pub context: Context,
    pub theme: Theme,
    pub view: View,
    pub(super) tx: Sender<Msg>,
    pub control: ReaderControl,
    pub pool: Option<Result<PoolSnapshot, String>>,
    pub inventory: Loaded<VmListReport>,
    pub(super) inventory_read: Option<Instant>,
    pub list: ListState,
    /// The chosen managed VM's live readings.
    pub vm: Option<(String, Result<VmSnapshot, String>)>,
    pub details: BTreeMap<String, VmDetails>,
    pub controller: Option<ControllerUnit>,
    pub doctor: DoctorState,
    pub logs: LogState,
    pub tasks: Tasks,
    pub dialog: Option<Dialog>,
    pub overlay: Option<Overlay>,
    pub prompt: Option<Prompt>,
    pub palette: Option<CommandPalette>,
    pub which: Option<Which>,
    pub help: Option<Help>,
    pub errors: Vec<ErrorEntry>,
    /// Notices sliding in at the bottom right.
    pub notices: Notify,
    /// Earlier answers, for the text fields.
    pub history: History,
    /// For the screen's loop to do, in order.
    pub effects: Vec<Effect>,
    pub quit: bool,
    /// Tasks are queued but never started: for tests, which mustn't run
    /// the commands.
    pub(crate) hold_tasks: bool,
}

impl App {
    pub fn new(context: Context, theme: Theme, tx: Sender<Msg>, control: ReaderControl) -> Self {
        Self {
            context,
            theme,
            view: View::Main,
            tx,
            control,
            pool: None,
            inventory: Loaded::NotYet,
            inventory_read: None,
            list: ListState::default(),
            vm: None,
            details: BTreeMap::new(),
            controller: None,
            doctor: DoctorState {
                loaded: Loaded::NotYet,
                of: None,
                scroll: Scroll::default(),
                filter: None,
                problems_only: false,
            },
            logs: LogState {
                loaded: Loaded::NotYet,
                of: None,
                scroll: Scroll::default(),
                find: None,
            },
            tasks: Tasks::default(),
            dialog: None,
            overlay: None,
            prompt: None,
            palette: None,
            which: None,
            help: None,
            errors: Vec::new(),
            notices: Notify::default(),
            history: History::default(),
            effects: Vec::new(),
            quit: false,
            hold_tasks: false,
        }
    }

    /// What takes the next key: the topmost of what is open, else the view.
    pub fn layer(&self) -> Layer {
        if self.help.is_some() {
            return Layer::Help;
        }
        if self.which.is_some() {
            return Layer::Which;
        }
        if self.palette.is_some() {
            return Layer::Palette;
        }
        if self.prompt.is_some() {
            return Layer::Prompt;
        }
        match (&self.dialog, &self.overlay) {
            (Some(Dialog::Confirm(_)), _) => Layer::Confirm,
            (Some(Dialog::Form(_)), _) => Layer::Form,
            (None, Some(Overlay::Tasks)) => Layer::Tasks,
            (None, Some(Overlay::Task { .. })) => Layer::Task,
            (None, Some(Overlay::Spot(_))) => Layer::Spot,
            (None, Some(Overlay::Errors(_))) => Layer::Errors,
            (None, None) => match self.view {
                View::Main => Layer::List,
                View::Details(_) => Layer::Details,
                View::Doctor(_) => Layer::Doctor,
                View::Logs(_) => Layer::Logs,
            },
        }
    }

    /// Every VM on the host: the managed ones in the pool's order, then
    /// half-added, parked and unmanaged ones by name.
    pub fn entries(&self) -> Vec<Entry> {
        let listed: Vec<&ListedVm> = self
            .inventory
            .ok()
            .map(|report| report.vms.iter().collect())
            .unwrap_or_default();
        let find = |name: &str| {
            listed
                .iter()
                .find(|vm| vm.vm_name == name)
                .map(|vm| (*vm).clone())
        };
        let mut entries: Vec<Entry> = match &self.pool {
            Some(Ok(snapshot)) => snapshot
                .command
                .vms
                .iter()
                .enumerate()
                .map(|(index, vm)| Entry {
                    name: vm.name.clone(),
                    management: Management::Managed,
                    pool_index: Some(index),
                    listed: find(&vm.name),
                })
                .collect(),
            _ => Vec::new(),
        };
        for management in [
            Management::Managed,
            Management::Unfinished,
            Management::Parked,
            Management::NotManaged,
        ] {
            for vm in listed.iter().filter(|vm| vm.management == management) {
                if !entries.iter().any(|entry| entry.name == vm.vm_name) {
                    entries.push(Entry {
                        name: vm.vm_name.clone(),
                        management,
                        pool_index: None,
                        listed: Some((*vm).clone()),
                    });
                }
            }
        }
        entries
    }

    /// The VMs the list shows: filtered, in its order.
    pub fn visible(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = self
            .entries()
            .into_iter()
            .filter(|entry| self.list.shows(&entry.name))
            .collect();
        if self.list.sort == SortBy::Pool {
            if self.list.reverse {
                entries.reverse();
            }
            return entries;
        }
        // Stable, so VMs alike stay in the pool's order; a VM without the
        // figure goes last either way.
        entries.sort_by(|a, b| {
            let ordering = match self.list.sort {
                SortBy::Name => a.name.cmp(&b.name),
                _ => match (self.figure(a), self.figure(b)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => return Ordering::Less,
                    (None, Some(_)) => return Ordering::Greater,
                    (None, None) => return Ordering::Equal,
                },
            };
            if self.list.reverse {
                ordering.reverse()
            } else {
                ordering
            }
        });
        entries
    }

    /// The figure the list is sorted by, for a VM the pool reads.
    fn figure(&self, entry: &Entry) -> Option<u64> {
        let pool = self.pool.as_ref()?.as_ref().ok()?;
        let index = entry.pool_index?;
        let sample = pool.vms.get(index)?;
        match self.list.sort {
            SortBy::Plugged => sample.device.map(|device| device.current_bytes),
            SortBy::Priority => pool.command.vms.get(index).map(|vm| u64::from(vm.priority)),
            SortBy::GuestUse => sample.guest.as_ref().map(|guest| {
                let memory = &guest.memory;
                let total = memory.physical_total_bytes.max(1);
                total.saturating_sub(memory.physical_available_bytes) * 1000 / total
            }),
            SortBy::Pool | SortBy::Name => None,
        }
    }

    pub fn selected(&self) -> Option<Entry> {
        let visible = self.visible();
        let at = self.list.scroll.cursor(visible.len());
        visible.into_iter().nth(at)
    }

    pub fn entry(&self, name: &str) -> Option<Entry> {
        self.entries().into_iter().find(|entry| entry.name == name)
    }

    /// The managed VMs, in the list's order.
    pub fn managed(&self) -> Vec<String> {
        self.visible()
            .into_iter()
            .filter(|entry| entry.management == Management::Managed)
            .map(|entry| entry.name)
            .collect()
    }

    /// The VMs marked, in the list's order.
    pub fn marked(&self) -> Vec<Entry> {
        self.entries()
            .into_iter()
            .filter(|entry| self.list.marks.contains(&entry.name))
            .collect()
    }

    /// Show a short notice at the bottom right.
    pub fn notify(&mut self, text: impl Into<String>, tone: Tone) {
        self.notices.push(Message::new(text, tone));
    }

    /// Copy `text` to the clipboard of the person's own computer.
    pub fn copy(&mut self, text: String) {
        self.notify(
            format!("Copied: {}", super::widgets::fit(&text, 60)),
            Tone::Good,
        );
        self.effects.push(Effect::Copy(text));
    }

    /// Nothing is open over the view.
    pub fn free(&self) -> bool {
        self.help.is_none()
            && self.which.is_none()
            && self.palette.is_none()
            && self.prompt.is_none()
            && self.dialog.is_none()
            && matches!(self.overlay, None | Some(Overlay::Tasks))
    }

    /// When the screen next changes by itself: a notice moving, the clock,
    /// the list of VMs read again.
    pub fn next_wake(&self, now: Instant) -> Instant {
        let mut wake = now + CLOCK;
        if let Some(notice) = self.notices.next_change(now) {
            wake = wake.min(notice);
        }
        if let Some(read) = self.inventory_read {
            wake = wake.min(read + INVENTORY_EVERY);
        }
        wake.max(now)
    }

    /// Called each time the screen wakes: notices that have gone go, and
    /// the host's VMs are read again when they are old.
    pub fn tick(&mut self, now: Instant) {
        self.notices.tick(now);
        let stale = self
            .inventory_read
            .is_none_or(|read| now.saturating_duration_since(read) >= INVENTORY_EVERY);
        if stale && self.inventory != Loaded::Loading {
            self.load_inventory();
        }
    }

    pub(super) fn load_inventory(&mut self) {
        self.inventory_read = Some(Instant::now());
        if self.inventory.ok().is_none() {
            self.inventory = Loaded::Loading;
        }
        let context = self.context.clone();
        jobs::spawn_load(self.tx.clone(), move || {
            Msg::Inventory(crate::inventory::list(&context))
        });
    }

    /// What the background work said. Keys go to [`App::on_key`].
    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Pool(pool) => self.pool = Some(pool),
            Msg::Vm(name, snapshot) => self.vm = Some((name, *snapshot)),
            Msg::Details(name, details) => {
                self.details.insert(name, details);
            }
            Msg::Controller(unit) => self.controller = Some(unit),
            // A failed re-read keeps the list it had.
            Msg::Inventory(Err(error)) if self.inventory.ok().is_some() => {
                self.notify(format!("Couldn't read the VMs: {error}"), Tone::Warn);
            }
            Msg::Inventory(report) => self.inventory = Loaded::Done(report),
            Msg::Doctor(sections) => self.doctor.loaded = Loaded::Done(sections),
            Msg::Log(text) => self.logs.loaded = Loaded::Done(text),
            Msg::Task(id, event) => self.on_task(id, event),
            Msg::Peers(changes) => {
                for change in changes {
                    self.notices.push(Message::titled(
                        format!("Change by {}", change.user),
                        change.text(),
                        Tone::Info,
                    ));
                }
                // What they changed shows on the next reads.
                self.load_inventory();
                self.control
                    .reload
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            Msg::Term(event) => {
                if let ratatui::crossterm::event::Event::Key(key) = event {
                    self.on_key(key);
                }
            }
            Msg::Gone(_) => self.quit = true,
        }
        self.follow_selection();
        self.surface_question();
    }

    /// The reader follows the chosen VM, so its details and settings are at
    /// hand when its menu, facts or details open.
    pub(super) fn follow_selection(&mut self) {
        let vm = match &self.view {
            View::Details(vm) => Some(vm.clone()),
            _ => self
                .selected()
                .filter(|entry| entry.management == Management::Managed)
                .map(|entry| entry.name),
        };
        self.control.want_vm(vm);
    }

    /// A task's question comes forward when nothing else is open.
    pub(super) fn surface_question(&mut self) {
        if !self.free() {
            return;
        }
        if let Some(id) = self.tasks.asking() {
            let from_list = self.overlay == Some(Overlay::Tasks);
            self.overlay = Some(Overlay::Task { id, from_list });
        }
    }

    fn on_task(&mut self, id: TaskId, event: FlowEvent) {
        let free = self.free();
        let Some(task) = self.tasks.get_mut(id) else {
            // A task that is gone: refuse its question.
            if let FlowEvent::Ask(_, reply) = event {
                let _ = reply.send(Answer::Cancelled);
            }
            return;
        };
        match event {
            FlowEvent::Line(line) => task.say(line),
            FlowEvent::Shown(text) => task.show(&text),
            FlowEvent::Ask(question, reply) if task.cancel => {
                task.lines
                    .push((format!("{} (cancelled)", question.text()), Tone::Warn));
                let _ = reply.send(Answer::Cancelled);
            }
            FlowEvent::Ask(question, reply) => {
                let input = match &question {
                    Question::Text(text) => Input::default().remembering(text.clone()),
                    _ => Input::default(),
                };
                task.pager.follow();
                task.pending = Some(Pending {
                    question,
                    reply,
                    input,
                    yes: false,
                    later: false,
                });
                if !free {
                    let title = task.title.clone();
                    self.notify(
                        format!("A question is waiting (press w): {title}"),
                        Tone::Accent,
                    );
                }
            }
            FlowEvent::Done(result) => {
                let title = task.title.clone();
                match &result {
                    Ok(_) => self
                        .notices
                        .push(Message::titled("Done", title.clone(), Tone::Good)),
                    Err(error) => {
                        self.errors.push(ErrorEntry {
                            time: crate::dashboard::clock(),
                            what: title.clone(),
                            error: error.clone(),
                        });
                        self.notices.push(Message::titled(
                            "Failed: in the errors list (e)",
                            title.clone(),
                            Tone::Bad,
                        ));
                    }
                }
                self.tasks.finish(id, result, Instant::now());
                // What the action changed shows on the next reads.
                self.load_inventory();
                self.refresh_view();
                self.start_tasks();
            }
        }
    }

    /// A doctor or the logs read again after a task, to show what it did.
    fn refresh_view(&mut self) {
        self.doctor.loaded = Loaded::NotYet;
        self.logs.loaded = Loaded::NotYet;
        if matches!(self.view, View::Doctor(_) | View::Logs(_)) {
            let view = self.view.clone();
            self.open(view);
        }
    }

    /// Queue `action` as a task.
    pub fn start(&mut self, action: Action) {
        let title = action.command_line(&self.context);
        let waiting = self.tasks.active() > 0;
        self.tasks.add(action, title.clone());
        if waiting {
            self.notify(
                format!("Queued, after the task running now: {title}"),
                Tone::Dim,
            );
        }
        self.start_tasks();
    }

    /// Start the next task when none is running.
    pub(super) fn start_tasks(&mut self) {
        if self.hold_tasks {
            return;
        }
        if let Some((id, action)) = self.tasks.start_next(Instant::now()) {
            action.spawn(&self.context, &self.control, self.tx.clone(), id);
        }
    }

    /// Answer task `id`'s question.
    pub(super) fn answer(&mut self, id: TaskId, answer: Answer) {
        let Some(task) = self.tasks.get_mut(id) else {
            return;
        };
        let Some(pending) = task.pending.take() else {
            return;
        };
        let shown = match &answer {
            Answer::Yes => "yes".to_owned(),
            Answer::No => "no".to_owned(),
            Answer::Text(text) => text.clone(),
            Answer::Cancelled => "(cancelled)".to_owned(),
        };
        task.lines
            .push((format!("{} {shown}", pending.question.text()), Tone::Value));
        if let (Answer::Text(text), Some(group)) = (&answer, &pending.input.group) {
            self.history.remember(group, text);
        }
        // A flow that has gone can't be told; it has stopped already.
        let _ = pending.reply.send(answer);
    }

    /// Do what a menu item says.
    pub fn pick(&mut self, pick: Pick) {
        match pick {
            Pick::Run(action) => self.start(action),
            Pick::Open(view) => self.open(view),
            Pick::Settings(vms) => self.edit_settings(&vms),
        }
    }

    pub fn open(&mut self, view: View) {
        self.view = view;
        self.overlay = None;
        self.follow_selection();
        let context = self.context.clone();
        match self.view.clone() {
            View::Doctor(vm)
                if self.doctor.of.as_ref() != Some(&vm) || self.doctor.loaded == Loaded::NotYet =>
            {
                self.doctor.loaded = Loaded::Loading;
                if self.doctor.of.as_ref() != Some(&vm) {
                    self.doctor.scroll = Scroll::default();
                }
                self.doctor.of = Some(vm.clone());
                jobs::spawn_load(self.tx.clone(), move || {
                    Msg::Doctor(match vm {
                        None => jobs::doctor(&context),
                        Some(vm) => jobs::doctor_one(&context, &vm),
                    })
                });
            }
            View::Logs(vm)
                if self.logs.of.as_ref() != Some(&vm) || self.logs.loaded == Loaded::NotYet =>
            {
                self.logs.loaded = Loaded::Loading;
                if self.logs.of.as_ref() != Some(&vm) {
                    // The newest line chosen: the cursor is held at the end.
                    self.logs.scroll = Scroll::at(usize::MAX);
                }
                self.logs.of = Some(vm.clone());
                jobs::spawn_load(self.tx.clone(), move || {
                    Msg::Log(crate::journal::read_log(
                        vm.as_deref(),
                        1000,
                        std::path::Path::new("/"),
                    ))
                });
            }
            _ => {}
        }
    }

    pub(super) fn reload(&mut self) {
        match self.view.clone() {
            View::Main | View::Details(_) => {
                self.load_inventory();
                self.control
                    .reload
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            view @ View::Doctor(_) => {
                self.doctor.loaded = Loaded::NotYet;
                self.open(view);
            }
            view @ View::Logs(_) => {
                self.logs.loaded = Loaded::NotYet;
                self.open(view);
            }
        }
    }

    /// The menu for a VM: what can be done with it, each with its key, and
    /// a few lines about it.
    pub fn menu(&self, entry: &Entry) -> (Vec<String>, Vec<Choice>) {
        let vm = entry.name.as_str();
        let line = |words: &[&str]| self.context.command_line(words);
        let item = |key, label: &str, command: String, pick| Choice {
            key,
            label: label.to_owned(),
            command,
            pick,
        };
        let doctor = item(
            'o',
            "Doctor: check everything about it",
            line(&["doctor", vm]),
            Pick::Open(View::Doctor(Some(vm.to_owned()))),
        );
        let forget = item(
            'f',
            "Forget it: stop managing it and delete its settings",
            line(&["vm", "remove", vm, "--forget"]),
            Pick::Run(Action::Remove {
                vm: vm.to_owned(),
                forget: true,
            }),
        );
        let listed = entry.listed.as_ref();
        let mut text = Vec::new();
        if let Some(listed) = listed {
            text.push(listed.detail.clone());
        }
        let choices = match entry.management {
            Management::Managed => vec![
                item(
                    'v',
                    "Live details",
                    line(&["watch", vm]),
                    Pick::Open(View::Details(vm.to_owned())),
                ),
                item(
                    's',
                    "Settings",
                    line(&["set", vm, "KEY=VALUE"]),
                    Pick::Settings(vec![vm.to_owned()]),
                ),
                doctor,
                item(
                    'l',
                    "Logs",
                    line(&["logs", "--vm", vm]),
                    Pick::Open(View::Logs(Some(vm.to_owned()))),
                ),
                item(
                    'c',
                    "Restart it, so its saved definition takes effect",
                    line(&["vm", "restart", vm]),
                    Pick::Run(Action::Restart(vm.to_owned())),
                ),
                item(
                    'k',
                    "Remove it, keeping its settings",
                    line(&["vm", "remove", vm]),
                    Pick::Run(Action::Remove {
                        vm: vm.to_owned(),
                        forget: false,
                    }),
                ),
                forget,
            ],
            Management::Parked => vec![
                item(
                    'b',
                    "Bring it back",
                    line(&["vm", "add", vm]),
                    Pick::Run(Action::Add(vm.to_owned())),
                ),
                doctor,
                forget,
            ],
            Management::Unfinished => vec![
                item(
                    'a',
                    "Finish adding it",
                    line(&["vm", "add", vm]),
                    Pick::Run(Action::Add(vm.to_owned())),
                ),
                doctor,
                forget,
            ],
            Management::NotManaged => {
                if let Some(listed) = listed {
                    for change in &listed.to_fix {
                        text.push(format!("Adding it will {change}."));
                    }
                    for problem in &listed.problems {
                        text.push(format!(
                            "Problem: {}. To fix it: {}",
                            problem.problem, problem.fix
                        ));
                    }
                    if listed.guest == crate::reports::Published::No {
                        text.push(format!(
                            "Install the guest service in the VM first: {}",
                            install_command(listed.os_id.as_deref())
                        ));
                    }
                }
                vec![
                    item(
                        'a',
                        "Add it",
                        line(&["vm", "add", vm, "--fix-xml"]),
                        Pick::Run(Action::Add(vm.to_owned())),
                    ),
                    doctor,
                ]
            }
        };
        (text, choices)
    }

    /// Open the chosen VM's menu beside its row.
    pub(super) fn open_menu(&mut self, entry: &Entry) {
        let (text, choices) = self.menu(entry);
        let cands = choices
            .into_iter()
            .map(|choice| Cand {
                keys: vec![Key::ch(choice.key)],
                desc: choice.label,
                command: Some(choice.command),
                run: Run::Pick(choice.pick),
            })
            .collect();
        self.which = Some(Which::menu(
            format!("{} ({})", entry.name, entry.management.label()),
            text,
            cands,
        ));
    }

    /// A menu's letter for `entry`, from the list or its details: the same
    /// as choosing it in the menu.
    pub(super) fn shortcut(&mut self, entry: &Entry, letter: char) {
        let (_, choices) = self.menu(entry);
        if let Some(choice) = choices.into_iter().find(|choice| choice.key == letter) {
            self.pick(choice.pick);
        }
    }

    /// A menu's letter for every marked VM: the settings form for them all,
    /// or the action for each, asked once.
    pub(super) fn for_marked(&mut self, letter: char) {
        let marked = self.marked();
        let mut actions = Vec::new();
        let mut settings = Vec::new();
        let mut skipped = Vec::new();
        for entry in &marked {
            let (_, choices) = self.menu(entry);
            match choices.into_iter().find(|choice| choice.key == letter) {
                Some(Choice {
                    pick: Pick::Run(action),
                    ..
                }) => actions.push(action),
                Some(Choice {
                    pick: Pick::Settings(vms),
                    ..
                }) => settings.extend(vms),
                _ => skipped.push(entry.name.clone()),
            }
        }
        if !settings.is_empty() {
            return self.edit_settings(&settings);
        }
        if actions.is_empty() {
            return self.notify(
                format!("None of the {} marked VMs can do that.", marked.len()),
                Tone::Warn,
            );
        }
        let mut body = vec![format!(
            "This runs {} commands, one after another, each with its own questions:",
            actions.len()
        )];
        if !skipped.is_empty() {
            body.push(format!(
                "skipped, as their menus don't have it: {}",
                skipped.join(", ")
            ));
        }
        let list = actions
            .iter()
            .map(|action| action.command_line(&self.context))
            .collect();
        self.dialog = Some(Dialog::Confirm(Confirm::new(
            format!("{} marked VMs", marked.len()),
            body,
            list,
            OnYes::Run(actions),
        )));
    }

    /// The settings form for `vms`: one VM's values to change, or for
    /// several, empty fields that leave each its own.
    pub(super) fn edit_settings(&mut self, vms: &[String]) {
        let fields = match vms {
            [vm] => {
                let Some(settings) = self
                    .details
                    .get(vm)
                    .and_then(|details| details.settings.clone())
                else {
                    return self.notify(
                        format!("{vm}'s settings are still being read; try again in a moment."),
                        Tone::Warn,
                    );
                };
                vec![
                    Field::new("priority", Some(settings.priority.to_string())),
                    Field::new("min_ram", Some(format_byte_size(settings.min_ram))),
                    Field::new("max_ram", Some(format_byte_size(settings.max_ram))),
                    Field::new("free_ram", Some(format_byte_size(settings.free_ram))),
                ]
            }
            _ => ["priority", "min_ram", "max_ram", "free_ram"]
                .into_iter()
                .map(|key| Field::new(key, None))
                .collect(),
        };
        self.dialog = Some(Dialog::Form(Form {
            vms: vms.to_vec(),
            fields,
            cursor: 0,
        }));
    }

    /// Open the command palette, with what can be done now.
    pub(super) fn open_palette(&mut self) {
        let mut items = Vec::new();
        if let Some(entry) = self.selected() {
            for choice in self.menu(&entry).1 {
                items.push(Item {
                    group: entry.name.clone(),
                    label: choice.label,
                    key: choice.key.to_string(),
                    run: Run::Pick(choice.pick),
                });
            }
        }
        let layer = self.layer();
        let mut seen: Vec<Cmd> = Vec::new();
        for binding in keymap::bindings(layer) {
            let skip = matches!(
                binding.cmd,
                Cmd::Move(_) | Cmd::Back | Cmd::Letter(_) | Cmd::Palette
            ) || binding.scope == Scope::Lists;
            if skip || seen.contains(&binding.cmd) {
                continue;
            }
            seen.push(binding.cmd);
            let group = match (binding.scope, binding.cmd) {
                (_, Cmd::Sort(..)) => "Sort",
                (_, Cmd::Linemode(_)) => "Columns",
                (Scope::Views, _) => "Anywhere",
                _ => "Here",
            };
            items.push(Item {
                group: group.to_owned(),
                label: capitalised(binding.desc),
                key: binding.label(),
                run: Run::Cmd(binding.cmd),
            });
        }
        for palette in Palette::order(self.theme.light) {
            items.push(Item {
                group: "Theme".to_owned(),
                label: format!("Theme: {}", palette.name()),
                key: String::new(),
                run: Run::Theme(palette),
            });
        }
        self.palette = Some(CommandPalette::new(items, self.theme.palette));
    }

    /// The doctor's findings in the order shown, with the VM each is about,
    /// as the filter and problems-only leave them.
    pub fn doctor_rows(&self) -> Vec<(Option<String>, &crate::doctor::Finding)> {
        let filter = self.doctor.filter.as_deref().map(str::to_lowercase);
        self.doctor
            .loaded
            .ok()
            .map(|sections| {
                sections
                    .iter()
                    .flat_map(|section| {
                        section
                            .findings
                            .iter()
                            .map(move |finding| (section.vm.clone(), finding))
                    })
                    .filter(|(_, finding)| !self.doctor.problems_only || !finding.ok)
                    .filter(|(vm, finding)| {
                        filter.as_deref().is_none_or(|filter| {
                            let text = format!(
                                "{} {} {} {}",
                                vm.as_deref().unwrap_or("host"),
                                finding.check,
                                finding.detail,
                                finding.fix.as_deref().unwrap_or("")
                            );
                            text.to_lowercase().contains(filter)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The journal's lines shown, oldest first.
    pub fn log_lines(&self) -> Vec<&str> {
        self.logs
            .loaded
            .ok()
            .map(|text| text.lines().collect())
            .unwrap_or_default()
    }

    /// Whether a log line holds the text found.
    pub fn found(&self, line: &str) -> bool {
        self.logs.find.as_deref().is_some_and(|find| {
            !find.is_empty() && line.to_lowercase().contains(&find.to_lowercase())
        })
    }
}

/// `text` with a capital first letter, for the palette.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or(String::new(), |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// The install command for a VM's system, from its libosinfo name.
pub fn install_command(os_id: Option<&str>) -> &'static str {
    match os_id {
        Some(id) if id.contains("debian") => "apt install rampire-guest",
        Some(id)
            if ["rhel", "redhat", "fedora", "rocky", "almalinux", "centos"]
                .iter()
                .any(|family| id.contains(family)) =>
        {
            "dnf install rampire-guest"
        }
        _ => "dnf install rampire-guest (or: apt install rampire-guest)",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::reports::{Published, VmListSchema};
    use crate::tui::theme::{Depth, Icons};

    pub(crate) fn listed(name: &str, management: Management) -> ListedVm {
        ListedVm {
            vm_name: name.to_owned(),
            state: "running".to_owned(),
            management,
            status: None,
            detail: format!("{name} detail"),
            to_fix: if management == Management::NotManaged {
                vec!["add a 6144 MiB virtio-mem device".to_owned()]
            } else {
                vec![]
            },
            problems: vec![],
            guest: if management == Management::NotManaged {
                Published::No
            } else {
                Published::Yes { age_seconds: 1 }
            },
            os_id: Some("http://debian.org/debian/13".to_owned()),
        }
    }

    pub(crate) fn app() -> (App, std::sync::mpsc::Receiver<Msg>) {
        let (tx, rx) = jobs::channel();
        let context = Context {
            config: None,
            connect: Some("test:///default".to_owned()),
            build: "0.1.0 (build test)".to_owned(),
            via: "tui",
        };
        let theme = Theme::new(Palette::Midnight, Depth::TrueColor, Icons { unicode: true });
        let mut app = App::new(context, theme, tx, ReaderControl::new());
        app.hold_tasks = true;
        app.on_msg(Msg::Pool(Ok(crate::tui::pool::tests::snapshot(3))));
        app.on_msg(Msg::Inventory(Ok(VmListReport {
            schema: VmListSchema,
            version: 1,
            vms: vec![
                listed("newvm", Management::NotManaged),
                listed("oldvm", Management::Parked),
                listed("vm0", Management::Managed),
            ],
        })));
        app.inventory_read = Some(Instant::now());
        (app, rx)
    }

    #[test]
    fn the_list_has_every_vm_managed_first() {
        let (app, _rx) = app();
        let names: Vec<(String, Management)> = app
            .entries()
            .into_iter()
            .map(|entry| (entry.name, entry.management))
            .collect();
        assert_eq!(
            names,
            [
                ("vm0".to_owned(), Management::Managed),
                ("a-very-long-vm-name".to_owned(), Management::Managed),
                ("vm2".to_owned(), Management::Managed),
                ("oldvm".to_owned(), Management::Parked),
                ("newvm".to_owned(), Management::NotManaged),
            ]
        );
    }

    #[test]
    fn the_list_filters_and_sorts_with_vms_without_the_figure_last() {
        let (mut app, _rx) = app();
        app.list.sort = SortBy::Plugged;
        app.list.reverse = true;
        let names: Vec<String> = app.visible().into_iter().map(|entry| entry.name).collect();
        // vm0 has 1 GiB plugged as vm2 has; the long-named VM's device
        // isn't read, so it goes after them, with the unmanaged VMs.
        assert_eq!(&names[..2], ["vm0", "vm2"]);
        assert_eq!(names.len(), 5);
        app.list.sort = SortBy::Name;
        app.list.reverse = false;
        app.list.filter = Some("VM".to_owned());
        let names: Vec<String> = app.visible().into_iter().map(|entry| entry.name).collect();
        assert_eq!(
            names,
            ["a-very-long-vm-name", "newvm", "oldvm", "vm0", "vm2"]
        );
        app.list.filter = Some("old".to_owned());
        assert_eq!(app.visible().len(), 1);
    }

    #[test]
    fn the_install_command_follows_the_vms_system() {
        assert_eq!(
            install_command(Some("http://debian.org/debian/13")),
            "apt install rampire-guest"
        );
        assert_eq!(
            install_command(Some("http://redhat.com/rhel/10.0")),
            "dnf install rampire-guest"
        );
        assert!(install_command(None).contains("apt"));
    }
}
