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
use super::form::{Field, Form, HostFacts, Purpose};
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
    /// The add form for these VMs: their settings, then `vm add`.
    Add(Vec<String>),
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

/// What a settings form says when the draft it edits isn't what runs.
const NOT_APPLIED: &str = "The draft has changes that aren't applied yet; they are shown here, and saving applies them too. rampire diff shows them.";

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
    /// Quitting once the tasks have stopped.
    pub quitting: bool,
    /// Tasks are queued but never started: for tests, which mustn't run
    /// the commands.
    pub(crate) hold_tasks: bool,
    /// The host isn't set up: its memory, for the setup form.
    pub setup: Option<HostFacts>,
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
            quitting: false,
            hold_tasks: false,
            setup: None,
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
    /// half-added and unmanaged ones by name.
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

    /// Whether `entry`'s guest service runs another build than this host's,
    /// which has the guest package of its own build to upgrade it to.
    pub fn update_available(&self, entry: &Entry) -> bool {
        let sample = self
            .pool
            .as_ref()
            .and_then(|pool| pool.as_ref().ok())
            .zip(entry.pool_index)
            .and_then(|(pool, index)| pool.vms.get(index));
        sample.is_some_and(|sample| {
            crate::guest::update_available(self.context.build_id(), sample.guest.as_ref())
        })
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

    /// Read in the background the disks of the VMs that could be added,
    /// so a system rampire-guest can't be installed in shows before Add.
    /// Never in tests, which mustn't run the commands.
    fn read_disks(&self) {
        if self.hold_tasks {
            return;
        }
        let vms = self
            .entries()
            .into_iter()
            .filter(|entry| entry.management == Management::NotManaged)
            .map(|entry| entry.name)
            .collect();
        let tx = self.tx.clone();
        crate::inventory::read_disks(vms, move || {
            let _ = tx.send(Msg::DiskRead);
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
            Msg::Controller(unit) => {
                // Said once each time it stops, with the key that starts it:
                // a stopped controller resizes nothing, and the header's
                // "inactive" alone doesn't say what to do.
                let was_running = self
                    .controller
                    .as_ref()
                    .is_none_or(|before| before.active_state == "active");
                if was_running && unit.active_state != "active" && self.setup.is_none() {
                    self.notify(
                        format!(
                            "The controller isn't running ({}), so no VM is resized: A (apply) starts it.",
                            unit.active_state
                        ),
                        Tone::Bad,
                    );
                }
                self.controller = Some(unit);
            }
            // A failed re-read keeps the list it had.
            Msg::Inventory(Err(error)) if self.inventory.ok().is_some() => {
                self.notify(format!("Couldn't read the VMs: {error}"), Tone::Warn);
            }
            Msg::Inventory(report) => {
                self.inventory = Loaded::Done(report);
                self.read_disks();
            }
            Msg::DiskRead => self.load_inventory(),
            Msg::Doctor(sections) => self.doctor.loaded = Loaded::Done(sections),
            Msg::Log(text) => self.logs.loaded = Loaded::Done(text),
            Msg::Task(id, event) => self.on_task(id, event),
            Msg::Setup(Ok(None)) => self.setup = None,
            Msg::Setup(Ok(Some((host_bytes, domains)))) => {
                self.setup = Some(HostFacts {
                    host_bytes,
                    set_aside_bytes: crate::init::set_aside(&domains),
                    vms: domains
                        .iter()
                        .filter(|domain| domain.set_aside_bytes() > 0)
                        .count(),
                });
                // Asked first, before anything else is done.
                if self.dialog.is_none() {
                    self.setup_form();
                }
            }
            Msg::Setup(Err(error)) => {
                self.notify(
                    format!("Couldn't read the host's memory: {error}"),
                    Tone::Warn,
                );
            }
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
            Msg::Stderr(line) => self.errors.push(ErrorEntry {
                time: crate::dashboard::clock(),
                what: "a message on stderr".to_owned(),
                error: line,
            }),
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
            FlowEvent::Ask(question, reply) if task.cancel.is_set() => {
                task.lines
                    .push((format!("{} (cancelled)", question.text()), Tone::Warn));
                let _ = reply.send(Answer::Cancelled);
            }
            FlowEvent::Ask(question, reply) => {
                let input = match &question {
                    Question::Text(text) => Input::default().remembering(text.clone()),
                    Question::Edit(_, suggested) => Input::new(suggested),
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
                let init = matches!(task.action, Action::Init { .. });
                let action = task.action.clone();
                match &result {
                    Ok(jobs::Ended::Done(text)) => {
                        self.notices
                            .push(Message::titled("Done", text.clone(), Tone::Good))
                    }
                    Ok(jobs::Ended::NotDone(text)) => {
                        self.notices
                            .push(Message::titled("Not done", text.clone(), Tone::Warn))
                    }
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
                if init {
                    self.check_setup();
                }
                // A VM RAMpire let go of has no live details to show.
                if let (Action::Remove(vm), Ok(jobs::Ended::Done(_))) = (&action, &result) {
                    if self.view == View::Details(vm.clone()) {
                        self.open(View::Main);
                    }
                }
                self.tasks.finish(id, result, Instant::now());
                if self.quitting && self.tasks.active() == 0 {
                    self.quit = true;
                    return;
                }
                // What the action changed shows on the next reads.
                crate::inventory::forget_guest_probes();
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
    pub fn start(&mut self, action: Action) -> TaskId {
        let title = action.command_line(&self.context);
        // The same command already queued or running isn't started again,
        // as when Add is pressed twice: that one goes on.
        if let Some(task) = self.tasks.list.iter().find(|task| {
            !task.state.finished()
                && (task.title == title
                    || matches!(
                        (&task.action, &action),
                        (Action::Add { vm: one, .. }, Action::Add { vm: other, .. }) if one == other
                    ))
        }) {
            let id = task.id;
            self.notify(format!("Already under way: {title}"), Tone::Dim);
            return id;
        }
        let waiting = self.tasks.active() > 0;
        let id = self.tasks.add(action, title.clone());
        if waiting {
            self.notify(
                format!("Queued, after the task running now: {title}"),
                Tone::Dim,
            );
        }
        self.start_tasks();
        id
    }

    /// Start the next task when none is running.
    pub(super) fn start_tasks(&mut self) {
        if self.hold_tasks {
            return;
        }
        if let Some((id, action)) = self.tasks.start_next(Instant::now()) {
            let cancel = self
                .tasks
                .get(id)
                .map(|task| task.cancel.clone())
                .unwrap_or_default();
            action.spawn(&self.context, &self.control, self.tx.clone(), id, cancel);
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
            Pick::Run(action) => {
                self.start(action);
            }
            Pick::Open(view) => self.open(view),
            Pick::Settings(vms) => self.edit_settings(&vms),
            Pick::Add(vms) => self.add_form(&vms),
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
                crate::inventory::forget_guest_probes();
                crate::inventory::forget_disks();
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
        let remove = item(
            'x',
            "Remove RAMpire",
            line(&["vm", "remove", vm]),
            Pick::Run(Action::Remove(vm.to_owned())),
        );
        let listed = entry.listed.as_ref();
        let mut text = Vec::new();
        if let Some(listed) = listed {
            text.push(listed.detail.clone());
        }
        let choices = match entry.management {
            Management::Managed => {
                let mut choices = vec![
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
                ];
                if listed.is_some_and(|listed| listed.runs_behind) {
                    text.push(
                        "Runs an older definition than its saved one: a restart brings it in"
                            .to_owned(),
                    );
                    choices.push(item(
                        'b',
                        "Restart onto its saved definition",
                        line(&["vm", "restart", vm]),
                        Pick::Run(Action::Restart(vm.to_owned())),
                    ));
                }
                let latched = listed
                    .and_then(|listed| listed.status.as_ref())
                    .is_some_and(|status| status.actuation_latched);
                if latched {
                    text.push(
                        "Resizing it has stopped: check the guest, then recover it".to_owned(),
                    );
                    choices.push(item(
                        'c',
                        "Recover: clear its latch",
                        line(&["recover", vm, "REASON"]),
                        Pick::Run(Action::Recover(vm.to_owned())),
                    ));
                }
                if self.update_available(entry) {
                    text.push(
                        "Update available: its guest service runs another build than this host's"
                            .to_owned(),
                    );
                    choices.push(item(
                        'u',
                        "Upgrade the guest service",
                        line(&["vm", "upgrade", vm]),
                        Pick::Run(Action::Upgrade(vm.to_owned())),
                    ));
                }
                choices.push(remove);
                choices
            }
            // An add that stopped part-way: finish it, or undo it, which
            // puts the old definition back.
            Management::Unfinished => vec![
                item(
                    'a',
                    "Finish adding it",
                    line(&["vm", "add", vm]),
                    Pick::Run(Action::add(vm)),
                ),
                doctor,
                item(
                    'x',
                    "Undo the add: remove RAMpire",
                    line(&["vm", "remove", vm]),
                    Pick::Run(Action::Remove(vm.to_owned())),
                ),
            ],
            Management::NotManaged => {
                let problems = listed.map_or(&[][..], |listed| listed.problems.as_slice());
                for problem in problems {
                    text.push(format!(
                        "Problem: {}. To fix it: {}",
                        problem.problem, problem.fix
                    ));
                }
                // Adding would only fail: it is offered once nothing stops it.
                let mut choices = Vec::new();
                if problems.is_empty() {
                    choices.push(item(
                        'a',
                        "Install RAMpire",
                        line(&["vm", "add", vm]),
                        Pick::Add(vec![vm.to_owned()]),
                    ));
                } else {
                    text.push(
                        "Install RAMpire is offered once nothing stops it; r reads it again."
                            .to_owned(),
                    );
                }
                choices.push(doctor);
                // An earlier RAMpire's parts are still in its definition.
                if listed.is_some_and(|listed| listed.rampire_parts) {
                    choices.push(remove);
                }
                choices
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
        let (text, choices) = self.menu(entry);
        match choices.into_iter().find(|choice| choice.key == letter) {
            Some(choice) => self.pick(choice.pick),
            // A key that does nothing would look like a TUI that didn't
            // hear it: say why, with what the menu says about the VM.
            None => {
                let why = text
                    .iter()
                    .find(|line| line.starts_with("Problem:") || line.contains("is offered once"))
                    .cloned()
                    .unwrap_or_else(|| "Enter shows what its menu offers.".to_owned());
                self.notify(
                    format!("{letter} isn't offered for {}: {why}", entry.name),
                    Tone::Warn,
                );
            }
        }
    }

    /// A menu's letter for every marked VM: the settings form for them all,
    /// or the action for each, asked once.
    pub(super) fn for_marked(&mut self, letter: char) {
        let marked = self.marked();
        let mut actions = Vec::new();
        let mut settings = Vec::new();
        let mut adding = Vec::new();
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
                Some(Choice {
                    pick: Pick::Add(vms),
                    ..
                }) => adding.extend(vms),
                _ => skipped.push(entry.name.clone()),
            }
        }
        if !settings.is_empty() {
            return self.edit_settings(&settings);
        }
        if !adding.is_empty() {
            self.add_form(&adding);
            if !actions.is_empty() {
                let left: Vec<String> = actions
                    .iter()
                    .filter_map(|action| action.vm().map(str::to_owned))
                    .collect();
                self.notify(
                    format!(
                        "Left out: {} stopped part-way through an earlier add; finish each with a on its own.",
                        left.join(", ")
                    ),
                    Tone::Warn,
                );
            }
            return;
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

    /// The global settings form: `global.toml` in the draft, every setting
    /// filled in with its value now, defaults included. A host that isn't
    /// set up gets the setup form instead.
    pub(super) fn global_form(&mut self) {
        if self.setup.is_some() {
            return self.setup_form();
        }
        let globals = match rampire_core::settings::load_globals(&self.context.draft()) {
            Ok(globals) => globals,
            Err(error) => {
                return self.notify(
                    format!("global.toml can't be read: {error}; fix it, then try again"),
                    Tone::Bad,
                )
            }
        };
        let running = rampire_core::settings::load_globals(&self.context.running()).ok();
        let warning = running
            .is_some_and(|running| running != globals)
            .then(|| NOT_APPLIED.to_owned());
        let fields = vec![
            Field::new("pool", globals.pool.map(format_byte_size)),
            Field::new("host_reserve", Some(format_byte_size(globals.host_reserve))),
            Field::new("observe_only", Some(globals.observe_only.to_string())),
            Field::new(
                "poll_interval_seconds",
                Some(globals.poll_interval_seconds.to_string()),
            ),
            Field::new(
                "command_timeout_seconds",
                Some(globals.command_timeout_seconds.to_string()),
            ),
            Field::new(
                "convergence_timeout_seconds",
                Some(globals.convergence_timeout_seconds.to_string()),
            ),
            Field::new("libvirt_uri", Some(globals.libvirt_uri.clone())),
            Field::new("retention", Some(globals.retention.to_string())),
        ];
        self.dialog = Some(Dialog::Form(Form {
            purpose: Purpose::Global,
            vms: Vec::new(),
            fields,
            cursor: 0,
            warning,
            host: None,
        }));
    }

    /// The settings form for `vms`: one VM's values to change, or for
    /// several, empty fields that leave each its own. The values are the
    /// draft's, which `set` changes, not the applied copy's; the form says
    /// when the two differ.
    pub(super) fn edit_settings(&mut self, vms: &[String]) {
        let mut warning = None;
        let fields = match vms {
            [vm] => {
                let settings = match crate::attest::vm_settings(&self.context.draft(), vm) {
                    Ok(settings) => settings,
                    Err(error) => {
                        return self
                            .notify(format!("{vm}'s settings can't be read: {error}"), Tone::Bad)
                    }
                };
                let running = crate::attest::vm_settings(&self.context.running(), vm).ok();
                if running.is_some_and(|running| running != settings) {
                    warning = Some(NOT_APPLIED.to_owned());
                }
                vec![
                    Field::new("priority", Some(settings.priority.to_string())),
                    Field::new("min_ram", Some(format_byte_size(settings.min_ram))),
                    Field::new("max_ram", Some(format_byte_size(settings.max_ram))),
                ]
            }
            _ => ["priority", "min_ram", "max_ram"]
                .into_iter()
                .map(|key| Field::new(key, None))
                .collect(),
        };
        self.dialog = Some(Dialog::Form(Form {
            purpose: Purpose::Set,
            vms: vms.to_vec(),
            fields,
            cursor: 0,
            warning,
            host: None,
        }));
    }

    /// The add form for `vms`: one VM's suggested settings, or for several,
    /// empty fields that give each its own; and how to log in to it.
    pub(super) fn add_form(&mut self, vms: &[String]) {
        let entries = self.entries();
        let listed = |vm: &String| {
            entries
                .iter()
                .find(|entry| &entry.name == vm)
                .and_then(|entry| entry.listed.clone())
        };
        let suggested = match vms {
            [vm] => listed(vm).and_then(|listed| listed.suggested),
            _ => None,
        };
        let running: Vec<(String, bool)> = vms
            .iter()
            .filter_map(|vm| {
                let listed = listed(vm)?;
                (listed.state == "running").then(|| (vm.clone(), listed.add_restarts))
            })
            .collect();
        let warning = running_warning(&running);
        let one = vms.len() == 1;
        let mut fields = vec![Field::new("priority", one.then(|| "100".to_owned()))];
        for (key, bytes) in [
            ("min_ram", suggested.map(|sizes| sizes.min_ram)),
            ("max_ram", suggested.map(|sizes| sizes.max_ram)),
        ] {
            fields.push(Field::new(key, bytes.map(format_byte_size)));
        }
        fields.push(Field::new("guest_login", None));
        self.dialog = Some(Dialog::Form(Form {
            purpose: Purpose::Add,
            vms: vms.to_vec(),
            fields,
            cursor: 0,
            warning,
            host: None,
        }));
    }

    /// Read whether the host is set up, and its memory if it isn't, for
    /// the setup form.
    pub fn check_setup(&mut self) {
        let context = self.context.clone();
        jobs::spawn_load(self.tx.clone(), move || {
            Msg::Setup(if crate::flows::controller::is_set_up(&context) {
                Ok(None)
            } else {
                crate::flows::controller::host_facts(&context).map(Some)
            })
        });
    }

    /// The setup form, for a host that isn't set up: the reserve to keep,
    /// filled in with the recommended one.
    pub(super) fn setup_form(&mut self) {
        let Some(host) = self.setup else {
            return self.notify(
                "This host is set up already: S changes its global settings.",
                Tone::Dim,
            );
        };
        let recommended = format_byte_size(crate::init::recommended_reserve(host.host_bytes));
        self.dialog = Some(Dialog::Form(Form {
            purpose: Purpose::Init,
            vms: Vec::new(),
            fields: vec![Field::new("host_reserve", Some(recommended))],
            cursor: 0,
            warning: None,
            host: Some(host),
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

/// What the add form says about the running VMs among those added, each
/// with whether its definition changes: shut down and started again, or
/// only when its guest package can't be installed with a login.
fn running_warning(running: &[(String, bool)]) -> Option<String> {
    let names = |restart: bool| -> Vec<&str> {
        running
            .iter()
            .filter(|(_, restarts)| *restarts == restart)
            .map(|(vm, _)| vm.as_str())
            .collect()
    };
    let list = |names: &[&str]| match names {
        [one] => format!("{one} is running"),
        names => format!("{} are running", names.join(", ")),
    };
    let restarted = names(true);
    let maybe = names(false);
    let mut parts = Vec::new();
    if !restarted.is_empty() {
        let it = if restarted.len() == 1 { "it" } else { "each" };
        parts.push(format!(
            "{}. Adding shuts {it} down and starts {it} again, so its new memory setup takes effect.",
            list(&restarted)
        ));
    }
    if !maybe.is_empty() {
        parts.push(format!(
            "{}. If RAMpire can't log in to install rampire-guest, it shuts the VM down for a moment.",
            list(&maybe)
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// `text` with a capital first letter, for the palette.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or(String::new(), |first| {
        first.to_uppercase().chain(characters).collect()
    })
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
            suggested: None,
            rampire_parts: false,
            add_restarts: false,
            runs_behind: false,
        }
    }

    pub(crate) fn app() -> (App, std::sync::mpsc::Receiver<Msg>) {
        let (tx, rx) = jobs::channel();
        let context = Context {
            config: None,
            connect: Some("test:///default".to_owned()),
            // The build of the fixture's guest: no update is available.
            build: "0.5.0 (build ab447c26ff67)".to_owned(),
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
                listed("oldvm", Management::Unfinished),
                listed("vm0", Management::Managed),
            ],
        })));
        app.inventory_read = Some(Instant::now());
        (app, rx)
    }

    #[test]
    fn a_stopped_controller_is_told_once_with_how_to_start_it() {
        let (mut app, _rx) = app();
        let unit = |state: &str| ControllerUnit {
            unit: "rampire.service".to_owned(),
            active_state: state.to_owned(),
            sub_state: "dead".to_owned(),
            unit_file_state: "enabled".to_owned(),
        };
        app.on_msg(Msg::Controller(unit("failed")));
        let told = |app: &App| {
            format!("{:?}", app.notices)
                .matches("A (apply) starts it")
                .count()
        };
        assert_eq!(told(&app), 1);
        app.on_msg(Msg::Controller(unit("failed")));
        assert_eq!(told(&app), 1, "not again while it stays stopped");
    }

    #[test]
    fn the_lists_note_fits_beside_a_vms_details() {
        // 150 columns: the details pane beside the list leaves it narrow.
        let (mut app, _rx) = app();
        let mut blocked = listed("newvm", Management::NotManaged);
        blocked.problems = vec![crate::reports::Problem {
            problem: "it runs Home Assistant OS".to_owned(),
            fix: "none".to_owned(),
        }];
        app.on_msg(Msg::Inventory(Ok(VmListReport {
            schema: VmListSchema,
            version: 1,
            vms: vec![blocked, listed("vm0", Management::Managed)],
        })));
        let area = ratatui::layout::Rect::new(0, 0, 150, 45);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        crate::tui::views::render(area, &mut buf, &app);
        let text = crate::tui::widgets::plain_text(&buf);
        assert!(text.contains(" blocked "), "{text}");
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
                ("oldvm".to_owned(), Management::Unfinished),
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
    fn adding_a_running_vm_warns_it_is_shut_down() {
        assert_eq!(running_warning(&[]), None);
        assert_eq!(
            running_warning(&[("web".to_owned(), true)]).as_deref(),
            Some("web is running. Adding shuts it down and starts it again, so its new memory setup takes effect.")
        );
        let both = running_warning(&[("web".to_owned(), true), ("db".to_owned(), false)]).unwrap();
        assert!(
            both.contains("db is running. If RAMpire can't log in"),
            "{both}"
        );
    }
}
