//! The TUI's state and what each key does. Drawing is in
//! [`super::screens`]; the work off the screen's thread is in
//! [`super::jobs`].
//!
//! Everything starts from one screen: the pool and every VM on the host,
//! managed or not. Enter on a VM opens its menu, which offers what can be
//! done with it: its details, settings, doctor and logs, attesting it,
//! removing it, or adding it when RAMpire doesn't manage it yet.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::actions::{self, Action};
use super::jobs::{self, Answer, DoctorSection, FlowEvent, Msg, Question, ReaderControl};
use super::theme::Theme;
use super::vm::VmDetails;
use crate::context::Context;
use crate::reports::{ControllerUnit, ListedVm, Management, VmListReport};
use crate::tone::Tone;
use crate::top::PoolSnapshot;
use crate::watch::VmSnapshot;

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

/// A question a flow is waiting on.
pub struct Pending {
    pub question: Question,
    pub reply: Sender<Answer>,
    /// What has been typed, for a text question.
    pub input: String,
}

/// A flow running in a dialog: what it said, the question it waits on, and
/// how it ended.
pub struct FlowDialog {
    pub action: Action,
    /// The command line that does the same.
    pub title: String,
    pub lines: Vec<(String, Tone)>,
    pub pending: Option<Pending>,
    pub done: Option<Result<String, String>>,
    /// Lines scrolled back from the newest.
    pub scroll: usize,
}

/// What choosing a menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Run an action in a dialog.
    Run(Action),
    /// Show a view.
    Open(View),
    /// The settings form for a VM.
    Settings(String),
}

/// One item of a menu, with what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub key: char,
    pub label: String,
    /// What it does, or the command that does the same.
    pub detail: String,
    pub pick: Pick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceDialog {
    pub title: String,
    pub text: Vec<String>,
    pub choices: Vec<Choice>,
    pub cursor: usize,
}

/// One setting in the settings form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: &'static str,
    pub value: String,
    pub original: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormDialog {
    pub vm: String,
    pub fields: Vec<Field>,
    pub cursor: usize,
}

impl FormDialog {
    /// The changes, as `KEY=VALUE`.
    pub fn changes(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|field| field.value.trim() != field.original)
            .map(|field| format!("{}={}", field.key, field.value.trim()))
            .collect()
    }
}

pub enum Dialog {
    Flow(FlowDialog),
    Choice(ChoiceDialog),
    Form(FormDialog),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Errors,
}

/// An error kept until a person has seen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorEntry {
    pub time: String,
    pub what: String,
    pub error: String,
}

/// A short notice in the corner, gone after a while.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub tone: Tone,
    pub shown: Instant,
}

impl Toast {
    /// How long it stays: longer for what needs reading.
    pub fn lasts(&self) -> Duration {
        match self.tone {
            Tone::Bad => Duration::from_secs(10),
            Tone::Warn => Duration::from_secs(6),
            _ => Duration::from_secs(4),
        }
    }
}

/// What a command palette item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Pick(Pick),
    Theme(super::theme::Palette),
    Icons,
    Help,
    Errors,
    Reload,
    Quit,
}

/// One command in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// The group it is listed under: the VM's name, View, Theme.
    pub group: String,
    pub label: String,
    /// The key that does the same, if there is one.
    pub key: String,
    pub command: Command,
}

/// The command palette: type a few letters of anything the TUI can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPalette {
    pub query: String,
    pub cursor: usize,
    pub items: Vec<PaletteItem>,
    /// The theme before a theme was previewed, put back on Esc.
    pub theme_before: super::theme::Palette,
}

impl CommandPalette {
    /// The items the query matches, best first; all of them, in their
    /// groups, before anything is typed.
    pub fn matches(&self) -> Vec<&PaletteItem> {
        if self.query.trim().is_empty() {
            return self.items.iter().collect();
        }
        let mut scored: Vec<(i64, usize, &PaletteItem)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let text = format!("{} {}", item.group, item.label);
                fuzzy_score(&self.query, &text).map(|score| (score, index, item))
            })
            .collect();
        scored.sort_by_key(|(score, index, _)| (-score, *index));
        scored.into_iter().map(|(_, _, item)| item).collect()
    }
}

/// How well `query` matches `text`: its letters in order, case aside,
/// scoring letters that follow each other or start a word higher. `None`
/// when they aren't all there.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut at = 0usize;
    let mut last: Option<usize> = None;
    for wanted in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..text.len()).find(|&index| text[index] == wanted)?;
        score += 1;
        if last.is_some_and(|last| last + 1 == found) {
            score += 5;
        }
        if found == 0 || !text[found - 1].is_alphanumeric() {
            score += 3;
        }
        last = Some(found);
        at = found + 1;
    }
    // Shorter texts with the same letters come first.
    Some(score * 100 - text.len() as i64)
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

/// How often the list of the host's VMs is read again by itself.
const INVENTORY_EVERY: Duration = Duration::from_secs(30);

pub struct App {
    pub context: Context,
    pub theme: Theme,
    pub view: View,
    tx: Sender<Msg>,
    pub control: ReaderControl,
    pub pool: Option<Result<PoolSnapshot, String>>,
    pub inventory: Loaded<VmListReport>,
    inventory_read: Option<Instant>,
    /// The chosen VM, by its place in [`App::entries`].
    pub cursor: usize,
    pub vm: Option<(String, Result<VmSnapshot, String>)>,
    pub details: BTreeMap<String, VmDetails>,
    pub controller: Option<ControllerUnit>,
    pub doctor: Loaded<Vec<DoctorSection>>,
    /// Whose doctor is loaded: the host and every VM, or one VM.
    doctor_for: Option<Option<String>>,
    pub doctor_cursor: usize,
    pub log: Loaded<String>,
    log_for: Option<Option<String>>,
    /// Lines scrolled back from the newest.
    pub log_scroll: usize,
    pub dialog: Option<Dialog>,
    pub overlay: Option<Overlay>,
    pub errors: Vec<ErrorEntry>,
    /// Short notices in the corner, newest last.
    pub toasts: Vec<Toast>,
    /// The command palette, while it is open.
    pub palette: Option<CommandPalette>,
    pub quit: bool,
    /// `q` was pressed while an action ran: a second `q` quits.
    quit_armed: bool,
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
            cursor: 0,
            vm: None,
            details: BTreeMap::new(),
            controller: None,
            doctor: Loaded::NotYet,
            doctor_for: None,
            doctor_cursor: 0,
            log: Loaded::NotYet,
            log_for: None,
            log_scroll: 0,
            dialog: None,
            overlay: None,
            errors: Vec::new(),
            toasts: Vec::new(),
            palette: None,
            quit: false,
            quit_armed: false,
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

    pub fn selected(&self) -> Option<Entry> {
        let entries = self.entries();
        let at = self.cursor.min(entries.len().saturating_sub(1));
        entries.into_iter().nth(at)
    }

    /// The managed VMs, in the pool's order.
    pub fn managed(&self) -> Vec<String> {
        self.entries()
            .into_iter()
            .filter(|entry| entry.management == Management::Managed)
            .map(|entry| entry.name)
            .collect()
    }

    /// Show a short notice in the corner. At most four are shown.
    pub fn notify(&mut self, text: impl Into<String>, tone: Tone) {
        self.toasts.push(Toast {
            text: text.into(),
            tone,
            shown: Instant::now(),
        });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
    }

    /// Open the command palette, with what can be done now.
    pub fn open_palette(&mut self) {
        let mut items = Vec::new();
        if let Some(entry) = self.selected() {
            for choice in self.menu(&entry).choices {
                items.push(PaletteItem {
                    group: entry.name.clone(),
                    label: choice.label,
                    key: choice.key.to_string(),
                    command: Command::Pick(choice.pick),
                });
            }
        }
        let item = |group: &str, label: &str, key: &str, command| PaletteItem {
            group: group.to_owned(),
            label: label.to_owned(),
            key: key.to_owned(),
            command,
        };
        items.extend([
            item(
                "View",
                "The VMs and the pool",
                "Esc",
                Command::Pick(Pick::Open(View::Main)),
            ),
            item(
                "View",
                "Doctor: the host and every VM",
                "d",
                Command::Pick(Pick::Open(View::Doctor(None))),
            ),
            item(
                "View",
                "Logs",
                "l",
                Command::Pick(Pick::Open(View::Logs(None))),
            ),
            item("View", "Read everything again", "r", Command::Reload),
            item("View", "Keys", "?", Command::Help),
        ]);
        if !self.errors.is_empty() {
            items.push(item("View", "Errors", "e", Command::Errors));
        }
        for palette in super::theme::Palette::ALL {
            items.push(item(
                "Theme",
                &format!("Theme: {}", palette.name()),
                "",
                Command::Theme(palette),
            ));
        }
        items.push(item("Theme", "Unicode or ASCII icons", "i", Command::Icons));
        items.push(item("RAMpire", "Quit", "q", Command::Quit));
        self.palette = Some(CommandPalette {
            query: String::new(),
            cursor: 0,
            items,
            theme_before: self.theme.palette,
        });
    }

    /// Show the theme the palette's cursor is on, as a preview.
    fn preview_theme(&mut self) {
        let Some(palette) = &self.palette else {
            return;
        };
        let chosen = palette
            .matches()
            .get(palette.cursor)
            .and_then(|item| match item.command {
                Command::Theme(theme) => Some(theme),
                _ => None,
            });
        self.theme.palette = chosen.unwrap_or(palette.theme_before);
    }

    fn on_palette_key(&mut self, key: KeyEvent) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        let count = palette.matches().len();
        match key.code {
            KeyCode::Esc => {
                self.theme.palette = palette.theme_before;
                self.palette = None;
                return;
            }
            KeyCode::Up => palette.cursor = palette.cursor.saturating_sub(1),
            KeyCode::Down => palette.cursor = (palette.cursor + 1).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                palette.query.pop();
                palette.cursor = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.query.push(character);
                palette.cursor = 0;
            }
            KeyCode::Enter => {
                let chosen = palette
                    .matches()
                    .get(palette.cursor)
                    .map(|item| item.command.clone());
                let before = palette.theme_before;
                self.palette = None;
                self.theme.palette = before;
                if let Some(command) = chosen {
                    self.run_command(command);
                }
                return;
            }
            _ => {}
        }
        self.preview_theme();
    }

    fn run_command(&mut self, command: Command) {
        match command {
            Command::Pick(pick) => self.pick(pick),
            Command::Theme(palette) => {
                self.theme.palette = palette;
                self.notify(format!("theme: {}", palette.name()), Tone::Dim);
            }
            Command::Icons => self.toggle_icons(),
            Command::Help => self.overlay = Some(Overlay::Help),
            Command::Errors => self.overlay = Some(Overlay::Errors),
            Command::Reload => self.reload(),
            Command::Quit => self.ask_quit(),
        }
    }

    fn toggle_icons(&mut self) {
        self.theme.icons.unicode = !self.theme.icons.unicode;
        let set = if self.theme.icons.unicode {
            "Unicode"
        } else {
            "ASCII"
        };
        self.notify(format!("icons: {set}"), Tone::Dim);
    }

    /// A flow is running in the dialog.
    pub fn busy(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::Flow(flow)) if flow.done.is_none())
    }

    /// Called every turn of the screen's loop: reads the host's VMs again
    /// when they are old.
    pub fn tick(&mut self) {
        self.toasts
            .retain(|toast| toast.shown.elapsed() < toast.lasts());
        let stale = self
            .inventory_read
            .is_none_or(|read| read.elapsed() >= INVENTORY_EVERY);
        if stale && self.inventory != Loaded::Loading {
            self.load_inventory();
        }
    }

    fn load_inventory(&mut self) {
        self.inventory_read = Some(Instant::now());
        if self.inventory.ok().is_none() {
            self.inventory = Loaded::Loading;
        }
        let context = self.context.clone();
        jobs::spawn_load(self.tx.clone(), move || {
            Msg::Inventory(crate::inventory::list(&context))
        });
    }

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
                self.notify(format!("reading the VMs failed: {error}"), Tone::Warn);
            }
            Msg::Inventory(report) => self.inventory = Loaded::Done(report),
            Msg::Doctor(sections) => self.doctor = Loaded::Done(sections),
            Msg::Log(text) => self.log = Loaded::Done(text),
            Msg::Flow(event) => self.on_flow(event),
        }
        self.follow_selection();
    }

    /// The reader follows the chosen VM, so its details and settings are at
    /// hand when its menu or details open.
    fn follow_selection(&mut self) {
        let vm = match &self.view {
            View::Details(vm) => Some(vm.clone()),
            _ => self
                .selected()
                .filter(|entry| entry.management == Management::Managed)
                .map(|entry| entry.name),
        };
        self.control.want_vm(vm);
    }

    fn on_flow(&mut self, event: FlowEvent) {
        let Some(Dialog::Flow(flow)) = &mut self.dialog else {
            // A flow whose dialog was closed: refuse its questions.
            if let FlowEvent::Ask(_, reply) = event {
                let _ = reply.send(Answer::Cancelled);
            }
            return;
        };
        match event {
            FlowEvent::Line(line) => {
                let tone = line_tone(&line);
                flow.lines.push((line, tone));
            }
            FlowEvent::Shown(text) => {
                for line in text.replace('\t', "    ").lines() {
                    let tone = if line.starts_with('+') && !line.starts_with("+++") {
                        Tone::Good
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        Tone::Bad
                    } else {
                        Tone::Dim
                    };
                    flow.lines.push((line.to_owned(), tone));
                }
            }
            FlowEvent::Ask(question, reply) => {
                flow.scroll = 0;
                flow.pending = Some(Pending {
                    question,
                    reply,
                    input: String::new(),
                });
            }
            FlowEvent::Done(result) => {
                let title = flow.title.clone();
                // The dialog is borrowed, so the notice is made after it.
                let notice = match &result {
                    Ok(_) => (format!("ran: {title}"), Tone::Good),
                    Err(error) => {
                        self.errors.push(ErrorEntry {
                            time: crate::dashboard::clock(),
                            what: title.clone(),
                            error: error.clone(),
                        });
                        (format!("failed: {title}"), Tone::Bad)
                    }
                };
                flow.pending = None;
                flow.done = Some(result);
                self.notify(notice.0, notice.1);
                // What the action changed shows on the next reads.
                self.load_inventory();
                self.doctor = Loaded::NotYet;
                self.log = Loaded::NotYet;
            }
        }
    }

    /// Start an action in a dialog that shows what it does.
    pub fn start(&mut self, action: Action) {
        let title = action.command_line(&self.context);
        action.spawn(&self.context, &self.control, self.tx.clone());
        self.dialog = Some(Dialog::Flow(FlowDialog {
            action,
            lines: vec![(format!("$ {title}"), Tone::Dim)],
            title,
            pending: None,
            done: None,
            scroll: 0,
        }));
    }

    /// Do what a menu item says.
    pub fn pick(&mut self, pick: Pick) {
        self.dialog = None;
        match pick {
            Pick::Run(action) => self.start(action),
            Pick::Open(view) => self.open(view),
            Pick::Settings(vm) => self.edit_settings(&vm),
        }
    }

    pub fn open(&mut self, view: View) {
        self.view = view;
        self.follow_selection();
        let context = self.context.clone();
        match self.view.clone() {
            View::Doctor(vm)
                if self.doctor_for.as_ref() != Some(&vm) || self.doctor == Loaded::NotYet =>
            {
                self.doctor = Loaded::Loading;
                self.doctor_for = Some(vm.clone());
                self.doctor_cursor = 0;
                jobs::spawn_load(self.tx.clone(), move || {
                    Msg::Doctor(match vm {
                        None => jobs::doctor(&context),
                        Some(vm) => jobs::doctor_one(&context, &vm),
                    })
                });
            }
            View::Logs(vm) if self.log_for.as_ref() != Some(&vm) || self.log == Loaded::NotYet => {
                self.log = Loaded::Loading;
                self.log_for = Some(vm.clone());
                self.log_scroll = 0;
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

    fn reload(&mut self) {
        match self.view.clone() {
            View::Main | View::Details(_) => {
                self.load_inventory();
                self.control
                    .reload
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            view @ View::Doctor(_) => {
                self.doctor = Loaded::NotYet;
                self.open(view);
            }
            view @ View::Logs(_) => {
                self.log = Loaded::NotYet;
                self.open(view);
            }
        }
    }

    /// The menu for a VM: what can be done with it, each with its key.
    pub fn menu(&self, entry: &Entry) -> ChoiceDialog {
        let vm = entry.name.as_str();
        let line = |words: &[&str]| self.context.command_line(words);
        let item = |key, label: &str, detail: String, pick| Choice {
            key,
            label: label.to_owned(),
            detail,
            pick,
        };
        let doctor = item(
            'o',
            "Doctor",
            format!(
                "everything that can be checked about it: {}",
                line(&["doctor", vm])
            ),
            Pick::Open(View::Doctor(Some(vm.to_owned()))),
        );
        let forget = item(
            'f',
            "Forget it",
            format!(
                "removes its settings, attestation, request state, definition backups and its review: {}",
                line(&["vm", "remove", vm, "--forget"])
            ),
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
                    "Details, live",
                    format!(
                        "its device, guest, history and events: {}",
                        line(&["watch", vm])
                    ),
                    Pick::Open(View::Details(vm.to_owned())),
                ),
                item(
                    's',
                    "Settings",
                    format!(
                        "priority, min_ram, max_ram, free_ram: {}",
                        line(&["set", vm, "KEY=VALUE"])
                    ),
                    Pick::Settings(vm.to_owned()),
                ),
                doctor,
                item(
                    'l',
                    "Logs",
                    format!("its lines in the journal: {}", line(&["logs", "--vm", vm])),
                    Pick::Open(View::Logs(Some(vm.to_owned()))),
                ),
                item(
                    'a',
                    "Attest again",
                    format!(
                        "after a change to its definition: {}",
                        line(&["vm", "attest", vm])
                    ),
                    Pick::Run(Action::Attest {
                        vm: vm.to_owned(),
                        restart: false,
                    }),
                ),
                item(
                    'c',
                    "Restart it, then attest",
                    format!(
                        "a cold restart, so a changed definition takes effect: {}",
                        line(&["vm", "attest", vm, "--restart"])
                    ),
                    Pick::Run(Action::Attest {
                        vm: vm.to_owned(),
                        restart: true,
                    }),
                ),
                item(
                    'k',
                    "Remove it, keeping its settings",
                    format!(
                        "it keeps its memory until its next cold start; vm add brings it back: {}",
                        line(&["vm", "remove", vm])
                    ),
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
                    format!("with the settings it had: {}", line(&["vm", "add", vm])),
                    Pick::Run(Action::Add(vm.to_owned())),
                ),
                doctor,
                forget,
            ],
            Management::Unfinished => vec![
                item(
                    'a',
                    "Finish adding it",
                    format!(
                        "an earlier vm add didn't apply: {}",
                        line(&["vm", "add", vm])
                    ),
                    Pick::Run(Action::Add(vm.to_owned())),
                ),
                doctor,
                forget,
            ],
            Management::NotManaged => {
                if let Some(listed) = listed {
                    for change in &listed.to_fix {
                        text.push(format!("vm add will {change}"));
                    }
                    for problem in &listed.problems {
                        text.push(format!(
                            "problem: {} (fix: {})",
                            problem.problem, problem.fix
                        ));
                    }
                    if listed.guest == crate::reports::Published::No {
                        text.push(format!(
                            "Its guest service isn't installed. In the VM: {}",
                            install_command(listed.os_id.as_deref())
                        ));
                    }
                }
                vec![
                    item(
                        'a',
                        "Add it",
                        format!(
                            "shows each change, asks the review question, then applies: {}",
                            line(&["vm", "add", vm, "--fix-xml"])
                        ),
                        Pick::Run(Action::Add(vm.to_owned())),
                    ),
                    doctor,
                ]
            }
        };
        ChoiceDialog {
            title: format!("{vm} ({})", entry.management.label()),
            text,
            choices,
            cursor: 0,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        if ctrl_c {
            return self.ask_quit();
        }
        if self.overlay.is_some() {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | '?' | 'e')
            ) {
                self.overlay = None;
            }
            return;
        }
        if self.palette.is_some() {
            return self.on_palette_key(key);
        }
        if self.dialog.is_some() {
            return self.on_dialog_key(key);
        }
        let ctrl_p =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p');
        if ctrl_p || key.code == KeyCode::Char(':') {
            return self.open_palette();
        }
        match key.code {
            KeyCode::Char('q') => return self.ask_quit(),
            KeyCode::Char('?') => return self.overlay = Some(Overlay::Help),
            KeyCode::Char('e') if !self.errors.is_empty() => {
                return self.overlay = Some(Overlay::Errors)
            }
            KeyCode::Char('t') => {
                self.theme.palette = self.theme.palette.next();
                self.notify(format!("theme: {}", self.theme.palette.name()), Tone::Dim);
                return;
            }
            KeyCode::Char('i') => return self.toggle_icons(),
            KeyCode::Char('r') => return self.reload(),
            KeyCode::Esc if self.view != View::Main => return self.open(View::Main),
            _ => {}
        }
        self.quit_armed = false;
        match self.view.clone() {
            View::Main => self.on_main_key(key),
            View::Details(vm) => self.on_details_key(&vm, key),
            View::Doctor(_) => self.on_doctor_key(key),
            View::Logs(vm) => self.on_logs_key(vm, key),
        }
    }

    fn ask_quit(&mut self) {
        if self.busy() && !self.quit_armed {
            self.quit_armed = true;
            self.notify(
                "an action is running: press q again to quit; a vm add finishes when run again"
                    .to_owned(),
                Tone::Warn,
            );
            return;
        }
        self.quit = true;
    }

    fn move_cursor(cursor: &mut usize, count: usize, key: KeyCode) {
        if count == 0 {
            *cursor = 0;
            return;
        }
        *cursor = match key {
            KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => (*cursor + 1).min(count - 1),
            KeyCode::Home => 0,
            KeyCode::End => count - 1,
            KeyCode::PageUp => cursor.saturating_sub(10),
            KeyCode::PageDown => (*cursor + 10).min(count - 1),
            _ => (*cursor).min(count - 1),
        };
    }

    /// A menu item's letter, for the chosen VM, from the list or its
    /// details: the same as choosing it in the menu.
    fn shortcut(&mut self, entry: &Entry, letter: char) {
        let menu = self.menu(entry);
        if let Some(choice) = menu.choices.into_iter().find(|choice| choice.key == letter) {
            self.pick(choice.pick);
        }
    }

    fn on_main_key(&mut self, key: KeyEvent) {
        let count = self.entries().len();
        match key.code {
            KeyCode::Enter => {
                if let Some(entry) = self.selected() {
                    self.dialog = Some(Dialog::Choice(self.menu(&entry)));
                }
            }
            KeyCode::Char('d') => self.open(View::Doctor(None)),
            KeyCode::Char('l') => self.open(View::Logs(None)),
            // `j` and `k` move in a list; the menu's `k` is in the menu.
            KeyCode::Char(letter) if letter != 'k' && letter != 'j' => {
                if let Some(entry) = self.selected() {
                    self.shortcut(&entry, letter);
                }
            }
            code => {
                Self::move_cursor(&mut self.cursor, count, code);
                self.follow_selection();
            }
        }
    }

    fn on_details_key(&mut self, vm: &str, key: KeyEvent) {
        let managed = self.managed();
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let Some(at) = managed.iter().position(|name| name == vm) else {
                    return;
                };
                let next = match key.code {
                    KeyCode::Left | KeyCode::Up => at.saturating_sub(1),
                    _ => (at + 1).min(managed.len() - 1),
                };
                if let Some(name) = managed.get(next) {
                    if let Some(cursor) =
                        self.entries().iter().position(|entry| &entry.name == name)
                    {
                        self.cursor = cursor;
                    }
                    self.open(View::Details(name.clone()));
                }
            }
            KeyCode::Enter => {
                if let Some(entry) = self.entries().into_iter().find(|entry| entry.name == vm) {
                    self.dialog = Some(Dialog::Choice(self.menu(&entry)));
                }
            }
            KeyCode::Char(letter) if letter != 'v' => {
                if let Some(entry) = self.entries().into_iter().find(|entry| entry.name == vm) {
                    self.shortcut(&entry, letter);
                }
            }
            _ => {}
        }
    }

    fn edit_settings(&mut self, vm: &str) {
        let Some(settings) = self
            .details
            .get(vm)
            .and_then(|details| details.settings.clone())
        else {
            self.notify(
                format!("{vm}'s settings aren't read yet; try again in a moment"),
                Tone::Warn,
            );
            return;
        };
        let field = |key, value: String| Field {
            key,
            original: value.clone(),
            value,
        };
        self.dialog = Some(Dialog::Form(FormDialog {
            vm: vm.to_owned(),
            fields: vec![
                field("priority", settings.priority.to_string()),
                field("min_ram", byte_text(settings.min_ram)),
                field("max_ram", byte_text(settings.max_ram)),
                field("free_ram", byte_text(settings.free_ram)),
            ],
            cursor: 0,
        }));
    }

    /// The doctor's findings in the order shown, with the VM each is about.
    pub fn doctor_rows(&self) -> Vec<(Option<String>, &crate::doctor::Finding)> {
        self.doctor
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
                    .collect()
            })
            .unwrap_or_default()
    }

    fn on_doctor_key(&mut self, key: KeyEvent) {
        let count = self.doctor_rows().len();
        match key.code {
            KeyCode::Enter => {
                let action = self
                    .doctor_rows()
                    .get(self.doctor_cursor)
                    .and_then(|(_, finding)| finding.fix.as_deref())
                    .and_then(actions::from_fix);
                match action {
                    Some(action) => {
                        let title = action.command_line(&self.context);
                        self.dialog = Some(Dialog::Choice(ChoiceDialog {
                            title: "Run the fix".to_owned(),
                            text: vec![format!("This runs: {title}")],
                            choices: vec![Choice {
                                key: 'y',
                                label: "Run it".to_owned(),
                                detail: "its own questions come next".to_owned(),
                                pick: Pick::Run(action),
                            }],
                            cursor: 0,
                        }));
                    }
                    None => self.notify(
                        "this fix is done by hand: it is shown beneath the problem".to_owned(),
                        Tone::Dim,
                    ),
                }
            }
            code => Self::move_cursor(&mut self.doctor_cursor, count, code),
        }
    }

    fn on_logs_key(&mut self, vm: Option<String>, key: KeyEvent) {
        let lines = self.log.ok().map_or(0, |text| text.lines().count());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.log_scroll = (self.log_scroll + 1).min(lines),
            KeyCode::Down | KeyCode::Char('j') => {
                self.log_scroll = self.log_scroll.saturating_sub(1)
            }
            KeyCode::PageUp => self.log_scroll = (self.log_scroll + 15).min(lines),
            KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_sub(15),
            KeyCode::Home => self.log_scroll = lines,
            KeyCode::End => self.log_scroll = 0,
            KeyCode::Char('f') => {
                let names = self.managed();
                let next = match &vm {
                    None => names.first().cloned(),
                    Some(current) => names
                        .iter()
                        .position(|name| name == current)
                        .and_then(|at| names.get(at + 1))
                        .cloned(),
                };
                self.log = Loaded::NotYet;
                self.open(View::Logs(next));
            }
            _ => {}
        }
    }

    fn on_dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        match dialog {
            Dialog::Flow(flow) => {
                if let Some(pending) = flow.pending.as_mut() {
                    let answer = match (&pending.question, key.code) {
                        (Question::Text(_), KeyCode::Enter) => {
                            Some(Answer::Text(pending.input.clone()))
                        }
                        (Question::Text(_), KeyCode::Backspace) => {
                            pending.input.pop();
                            None
                        }
                        (Question::Text(_), KeyCode::Char(character)) => {
                            pending.input.push(character);
                            None
                        }
                        (_, KeyCode::Esc) => Some(Answer::Cancelled),
                        (Question::Confirm(_) | Question::Review(_), KeyCode::Char('y' | 'Y')) => {
                            Some(Answer::Yes)
                        }
                        (
                            Question::Confirm(_) | Question::Review(_),
                            KeyCode::Char('n' | 'N') | KeyCode::Enter,
                        ) => Some(Answer::No),
                        _ => None,
                    };
                    if let Some(answer) = answer {
                        let shown = match &answer {
                            Answer::Yes => "yes".to_owned(),
                            Answer::No => "no".to_owned(),
                            Answer::Text(text) => text.clone(),
                            Answer::Cancelled => "(closed)".to_owned(),
                        };
                        if let Some(pending) = flow.pending.take() {
                            flow.lines.push((
                                format!("{} {shown}", pending.question.text()),
                                Tone::Value,
                            ));
                            let _ = pending.reply.send(answer);
                        }
                    }
                    return;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        flow.scroll = (flow.scroll + 1).min(flow.lines.len())
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        flow.scroll = flow.scroll.saturating_sub(1)
                    }
                    KeyCode::PageUp => flow.scroll = (flow.scroll + 10).min(flow.lines.len()),
                    KeyCode::PageDown => flow.scroll = flow.scroll.saturating_sub(10),
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') if flow.done.is_some() => {
                        self.dialog = None;
                        // A doctor is checked again, to show what the fix did.
                        if matches!(self.view, View::Doctor(_)) {
                            let view = self.view.clone();
                            self.open(view);
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.notify(
                            "the action is still running; its dialog closes when it ends"
                                .to_owned(),
                            Tone::Warn,
                        );
                    }
                    _ => {}
                }
            }
            Dialog::Choice(choice) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.dialog = None,
                // Letters choose, so only arrows move.
                KeyCode::Up => choice.cursor = choice.cursor.saturating_sub(1),
                KeyCode::Down => {
                    choice.cursor = (choice.cursor + 1).min(choice.choices.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(picked) = choice.choices.get(choice.cursor).cloned() {
                        self.pick(picked.pick);
                    }
                }
                KeyCode::Char(character) => {
                    if let Some(picked) = choice
                        .choices
                        .iter()
                        .find(|option| option.key == character)
                        .cloned()
                    {
                        self.pick(picked.pick);
                    }
                }
                _ => {}
            },
            Dialog::Form(form) => match key.code {
                KeyCode::Esc => self.dialog = None,
                KeyCode::Up | KeyCode::BackTab => form.cursor = form.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => {
                    form.cursor = (form.cursor + 1).min(form.fields.len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    if let Some(field) = form.fields.get_mut(form.cursor) {
                        field.value.pop();
                    }
                }
                KeyCode::Char(character) if !character.is_control() => {
                    if let Some(field) = form.fields.get_mut(form.cursor) {
                        field.value.push(character);
                    }
                }
                KeyCode::Enter => {
                    let changes = form.changes();
                    let vm = form.vm.clone();
                    if changes.is_empty() {
                        self.dialog = None;
                        self.notify("nothing changed".to_owned(), Tone::Dim);
                    } else {
                        self.start(Action::Set {
                            target: vm,
                            changes,
                        });
                    }
                }
                _ => {}
            },
        }
    }
}

/// A size as `set` takes it: whole GiB or MiB where it divides.
pub fn byte_text(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes.is_multiple_of(GIB) {
        format!("{}GiB", bytes / GIB)
    } else if bytes.is_multiple_of(MIB) {
        format!("{}MiB", bytes / MIB)
    } else {
        bytes.to_string()
    }
}

/// A flow's line, toned by what it says.
fn line_tone(line: &str) -> Tone {
    if line.starts_with("problem:") || line.contains("failed") {
        Tone::Bad
    } else if line.starts_with("  fix:") || line.starts_with("to fix:") {
        Tone::Warn
    } else if line.starts_with("applied")
        || line.starts_with("attested")
        || line.starts_with("wrote")
        || line.contains(" published")
        || line.ends_with("started")
    {
        Tone::Good
    } else {
        Tone::Normal
    }
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
    use crate::tui::theme::{Depth, Icons, Palette};

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

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
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
    fn enter_opens_the_menu_for_what_the_vm_is() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Enter);
        let Some(Dialog::Choice(menu)) = &app.dialog else {
            panic!("a menu");
        };
        let keys: String = menu.choices.iter().map(|choice| choice.key).collect();
        assert_eq!(keys, "vsolackf");
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.view, View::Details("vm0".to_owned()));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Main);

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        let Some(Dialog::Choice(menu)) = &app.dialog else {
            panic!("a menu");
        };
        assert_eq!(
            menu.choices[0].pick,
            Pick::Run(Action::Add("newvm".to_owned()))
        );
        assert!(menu
            .text
            .iter()
            .any(|line| line.contains("apt install rampire-guest")));
        press(&mut app, KeyCode::Esc);
        assert!(app.dialog.is_none());

        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        let Some(Dialog::Choice(menu)) = &app.dialog else {
            panic!("a menu");
        };
        assert_eq!(
            menu.choices[0].pick,
            Pick::Run(Action::Add("oldvm".to_owned()))
        );
    }

    #[test]
    fn the_menus_letters_work_from_the_list() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.view, View::Doctor(Some("vm0".to_owned())));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.view, View::Doctor(None));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.view, View::Logs(None));
        press(&mut app, KeyCode::Esc);
        // No settings read yet: it says so rather than showing an empty form.
        press(&mut app, KeyCode::Char('s'));
        assert!(app.dialog.is_none());
        app.on_msg(Msg::Details(
            "vm0".to_owned(),
            crate::tui::vm::tests::details(),
        ));
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.dialog, Some(Dialog::Form(_))));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn the_palette_finds_commands_by_a_few_letters_and_runs_them() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char(':'));
        let palette = app.palette.as_ref().expect("the palette opens");
        assert!(palette.matches().len() > 10);
        for character in "doct".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        let first = app.palette.as_ref().unwrap().matches()[0].clone();
        assert_eq!(first.label, "Doctor");
        assert_eq!(first.group, "vm0");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.palette.is_none());
        assert_eq!(app.view, View::Doctor(None));

        // Moving onto a theme previews it; Esc puts the old one back.
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        for character in "light".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        assert_eq!(app.theme.palette, crate::tui::theme::Palette::Light);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.theme.palette, crate::tui::theme::Palette::Midnight);
    }

    #[test]
    fn fuzzy_matching_prefers_letters_together_and_word_starts() {
        assert!(fuzzy_score("dr", "Doctor").is_some());
        assert!(fuzzy_score("xyz", "Doctor").is_none());
        let together = fuzzy_score("log", "Logs").unwrap();
        let apart = fuzzy_score("log", "Leave our garden").unwrap();
        assert!(together > apart);
    }

    #[test]
    fn toasts_go_after_a_while_and_errors_stay_longest() {
        let (mut app, _rx) = app();
        app.notify("done", Tone::Good);
        app.notify("broke", Tone::Bad);
        assert_eq!(app.toasts.len(), 2);
        app.toasts[0].shown -= Duration::from_secs(5);
        app.toasts[1].shown -= Duration::from_secs(5);
        app.tick();
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].text, "broke");
    }

    #[test]
    fn ctrl_c_quits_and_q_asks_twice_while_an_action_runs() {
        let (mut app, _rx) = app();
        app.dialog = Some(Dialog::Flow(FlowDialog {
            action: Action::Apply,
            title: "rampire apply".to_owned(),
            lines: vec![],
            pending: None,
            done: None,
            scroll: 0,
        }));
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.quit);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn a_question_is_answered_from_the_dialog_and_the_answer_recorded() {
        let (mut app, _rx) = app();
        let (reply, answer) = std::sync::mpsc::channel();
        app.dialog = Some(Dialog::Flow(FlowDialog {
            action: Action::Apply,
            title: "rampire apply".to_owned(),
            lines: vec![],
            pending: None,
            done: None,
            scroll: 0,
        }));
        app.on_msg(Msg::Flow(FlowEvent::Ask(
            Question::Confirm("Apply this?".to_owned()),
            reply,
        )));
        press(&mut app, KeyCode::Char('x'));
        assert!(answer.try_recv().is_err());
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(answer.recv().unwrap(), Answer::Yes);
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Flow(FlowEvent::Ask(
            Question::Text("Why?".to_owned()),
            reply,
        )));
        for character in "ok now".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        assert_eq!(answer.recv().unwrap(), Answer::Text("ok no".to_owned()));
        app.on_msg(Msg::Flow(FlowEvent::Done(Err("it broke".to_owned()))));
        assert_eq!(app.errors.len(), 1);
        let Some(Dialog::Flow(flow)) = &app.dialog else {
            panic!("the dialog stays to show the error");
        };
        assert!(flow.lines.iter().any(|(line, _)| line == "Apply this? yes"));
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.overlay, Some(Overlay::Errors));
    }

    #[test]
    fn the_settings_form_changes_only_what_was_edited() {
        let form = FormDialog {
            vm: "web".to_owned(),
            fields: vec![
                Field {
                    key: "priority",
                    value: "150".to_owned(),
                    original: "100".to_owned(),
                },
                Field {
                    key: "max_ram",
                    value: "8GiB".to_owned(),
                    original: "8GiB".to_owned(),
                },
            ],
            cursor: 0,
        };
        assert_eq!(form.changes(), ["priority=150"]);
        assert_eq!(byte_text(8 << 30), "8GiB");
        assert_eq!(byte_text(512 << 20), "512MiB");
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
