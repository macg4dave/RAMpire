//! The TUI's state and what each key does. Drawing is in
//! [`super::screens`]; the work off the screen's thread is in
//! [`super::jobs`].

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::actions::{self, Action};
use super::jobs::{self, Answer, DoctorSection, FlowEvent, Msg, Question, ReaderControl};
use super::theme::Theme;
use super::vm::VmDetails;
use crate::context::Context;
use crate::reports::{ControllerUnit, Management, VmListReport};
use crate::tone::Tone;
use crate::top::PoolSnapshot;
use crate::watch::VmSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Vm,
    Add,
    Remove,
    Doctor,
    Logs,
}

impl Screen {
    pub const ALL: [Self; 6] = [
        Self::Dashboard,
        Self::Vm,
        Self::Add,
        Self::Remove,
        Self::Doctor,
        Self::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Vm => "VM",
            Self::Add => "Add",
            Self::Remove => "Remove",
            Self::Doctor => "Doctor",
            Self::Logs => "Logs",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|screen| *screen == self)
            .unwrap_or(0)
    }
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

/// One of several things to do, each with what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub key: char,
    pub label: String,
    pub detail: String,
    pub action: Action,
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

pub struct App {
    pub context: Context,
    pub theme: Theme,
    pub screen: Screen,
    tx: Sender<Msg>,
    pub control: ReaderControl,
    pub pool: Option<Result<PoolSnapshot, String>>,
    /// The chosen VM, by its place in the pool.
    pub selected: usize,
    pub vm: Option<(String, Result<VmSnapshot, String>)>,
    pub details: BTreeMap<String, VmDetails>,
    pub controller: Option<ControllerUnit>,
    pub inventory: Loaded<VmListReport>,
    pub add_cursor: usize,
    pub remove_cursor: usize,
    pub doctor: Loaded<Vec<DoctorSection>>,
    pub doctor_cursor: usize,
    pub log: Loaded<String>,
    /// Lines scrolled back from the newest.
    pub log_scroll: usize,
    pub log_filter: Option<String>,
    pub dialog: Option<Dialog>,
    pub overlay: Option<Overlay>,
    pub errors: Vec<ErrorEntry>,
    pub status: Option<(String, Tone)>,
    pub quit: bool,
    /// `q` was pressed while an action ran: a second `q` quits.
    quit_armed: bool,
}

impl App {
    pub fn new(context: Context, theme: Theme, tx: Sender<Msg>, control: ReaderControl) -> Self {
        Self {
            context,
            theme,
            screen: Screen::Dashboard,
            tx,
            control,
            pool: None,
            selected: 0,
            vm: None,
            details: BTreeMap::new(),
            controller: None,
            inventory: Loaded::NotYet,
            add_cursor: 0,
            remove_cursor: 0,
            doctor: Loaded::NotYet,
            doctor_cursor: 0,
            log: Loaded::NotYet,
            log_scroll: 0,
            log_filter: None,
            dialog: None,
            overlay: None,
            errors: Vec::new(),
            status: None,
            quit: false,
            quit_armed: false,
        }
    }

    /// The managed VMs, in the pool's order.
    pub fn vm_names(&self) -> Vec<String> {
        match &self.pool {
            Some(Ok(snapshot)) => snapshot
                .command
                .vms
                .iter()
                .map(|vm| vm.name.clone())
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn selected_vm(&self) -> Option<String> {
        let names = self.vm_names();
        names
            .get(self.selected.min(names.len().saturating_sub(1)))
            .cloned()
    }

    /// A flow is running in the dialog.
    pub fn busy(&self) -> bool {
        matches!(&self.dialog, Some(Dialog::Flow(flow)) if flow.done.is_none())
    }

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Pool(pool) => {
                if let Ok(snapshot) = &pool {
                    let count = snapshot.command.vms.len();
                    if count > 0 && self.selected >= count {
                        self.selected = count - 1;
                    }
                }
                self.pool = Some(pool);
            }
            Msg::Vm(name, snapshot) => self.vm = Some((name, *snapshot)),
            Msg::Details(name, details) => {
                self.details.insert(name, details);
            }
            Msg::Controller(unit) => self.controller = Some(unit),
            Msg::Inventory(report) => {
                self.inventory = Loaded::Done(report);
            }
            Msg::Doctor(sections) => self.doctor = Loaded::Done(sections),
            Msg::Log(text) => self.log = Loaded::Done(text),
            Msg::Flow(event) => self.on_flow(event),
        }
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
                for line in text.lines() {
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
                match &result {
                    Ok(_) => {
                        self.status = Some((format!("ran: {title}"), Tone::Good));
                    }
                    Err(error) => {
                        self.status = Some((format!("failed: {title}"), Tone::Bad));
                        self.errors.push(ErrorEntry {
                            time: crate::dashboard::clock(),
                            what: title,
                            error: error.clone(),
                        });
                    }
                }
                flow.pending = None;
                flow.done = Some(result);
                // What the action changed shows on the next reads.
                self.inventory = Loaded::NotYet;
                self.doctor = Loaded::NotYet;
                self.refresh_screen();
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

    fn go(&mut self, screen: Screen) {
        self.screen = screen;
        self.refresh_screen();
    }

    /// Load what the screen shows, if it isn't loaded.
    pub fn refresh_screen(&mut self) {
        let vm = (self.screen == Screen::Vm)
            .then(|| self.selected_vm())
            .flatten();
        self.control.want_vm(vm);
        let context = self.context.clone();
        match self.screen {
            Screen::Add | Screen::Remove if self.inventory == Loaded::NotYet => {
                self.inventory = Loaded::Loading;
                jobs::spawn_load(self.tx.clone(), move || {
                    Msg::Inventory(crate::inventory::list(&context))
                });
            }
            Screen::Doctor if self.doctor == Loaded::NotYet => {
                self.doctor = Loaded::Loading;
                jobs::spawn_load(self.tx.clone(), move || Msg::Doctor(jobs::doctor(&context)));
            }
            Screen::Logs if self.log == Loaded::NotYet => {
                self.log = Loaded::Loading;
                let filter = self.log_filter.clone();
                jobs::spawn_load(self.tx.clone(), move || {
                    Msg::Log(crate::journal::read_log(
                        filter.as_deref(),
                        1000,
                        std::path::Path::new("/"),
                    ))
                });
            }
            _ => {}
        }
    }

    fn reload_screen(&mut self) {
        match self.screen {
            Screen::Add | Screen::Remove => self.inventory = Loaded::NotYet,
            Screen::Doctor => self.doctor = Loaded::NotYet,
            Screen::Logs => self.log = Loaded::NotYet,
            _ => self
                .control
                .reload
                .store(true, std::sync::atomic::Ordering::Release),
        }
        self.refresh_screen();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        if self.overlay.is_some() {
            if ctrl_c {
                return self.ask_quit();
            }
            if matches!(
                key.code,
                KeyCode::Esc
                    | KeyCode::Enter
                    | KeyCode::Char('q')
                    | KeyCode::Char('?')
                    | KeyCode::Char('e')
            ) {
                self.overlay = None;
            }
            return;
        }
        if self.dialog.is_some() {
            if ctrl_c {
                return self.ask_quit();
            }
            return self.on_dialog_key(key);
        }
        if ctrl_c {
            return self.ask_quit();
        }
        match key.code {
            KeyCode::Char('q') => return self.ask_quit(),
            KeyCode::Char('?') => return self.overlay = Some(Overlay::Help),
            KeyCode::Char('e') if !self.errors.is_empty() => {
                return self.overlay = Some(Overlay::Errors)
            }
            KeyCode::Char('t') => {
                self.theme.palette = self.theme.palette.next();
                self.status = Some((format!("theme: {}", self.theme.palette.name()), Tone::Dim));
                return;
            }
            KeyCode::Char('i') => {
                self.theme.icons.unicode = !self.theme.icons.unicode;
                let set = if self.theme.icons.unicode {
                    "Unicode"
                } else {
                    "ASCII"
                };
                self.status = Some((format!("icons: {set}"), Tone::Dim));
                return;
            }
            KeyCode::Tab => {
                return self.go(Screen::ALL[(self.screen.index() + 1) % Screen::ALL.len()])
            }
            KeyCode::BackTab => {
                return self
                    .go(Screen::ALL
                        [(self.screen.index() + Screen::ALL.len() - 1) % Screen::ALL.len()])
            }
            KeyCode::Char(digit @ '1'..='6') => {
                return self.go(Screen::ALL[digit as usize - '1' as usize]);
            }
            KeyCode::Char('r') => return self.reload_screen(),
            _ => {}
        }
        self.quit_armed = false;
        match self.screen {
            Screen::Dashboard => self.on_dashboard_key(key),
            Screen::Vm => self.on_vm_key(key),
            Screen::Add => self.on_add_key(key),
            Screen::Remove => self.on_remove_key(key),
            Screen::Doctor => self.on_doctor_key(key),
            Screen::Logs => self.on_logs_key(key),
        }
    }

    fn ask_quit(&mut self) {
        if self.busy() && !self.quit_armed {
            self.quit_armed = true;
            self.status = Some((
                "an action is running: press q again to quit; a vm add finishes when run again"
                    .to_owned(),
                Tone::Warn,
            ));
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

    fn on_dashboard_key(&mut self, key: KeyEvent) {
        let count = self.vm_names().len();
        match key.code {
            KeyCode::Enter if count > 0 => self.go(Screen::Vm),
            code => Self::move_cursor(&mut self.selected, count, code),
        }
    }

    fn on_vm_key(&mut self, key: KeyEvent) {
        let Some(vm) = self.selected_vm() else {
            if key.code == KeyCode::Esc {
                self.go(Screen::Dashboard);
            }
            return;
        };
        match key.code {
            KeyCode::Esc => self.go(Screen::Dashboard),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                let count = self.vm_names().len();
                let code = match key.code {
                    KeyCode::Left => KeyCode::Up,
                    KeyCode::Right => KeyCode::Down,
                    code => code,
                };
                Self::move_cursor(&mut self.selected, count, code);
                self.refresh_screen();
            }
            KeyCode::Char('s') => self.edit_settings(&vm),
            KeyCode::Char('a') => self.dialog = Some(Dialog::Choice(attest_choice(&vm))),
            KeyCode::Char('x') => {
                self.dialog = Some(Dialog::Choice(remove_choice(&vm, false, &self.context)))
            }
            KeyCode::Char('d') => {
                self.doctor = Loaded::NotYet;
                self.go(Screen::Doctor);
            }
            KeyCode::Char('l') => {
                self.log_filter = Some(vm);
                self.log = Loaded::NotYet;
                self.go(Screen::Logs);
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
            self.status = Some((format!("{vm}'s settings aren't read yet"), Tone::Warn));
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

    /// The VMs the add screen offers: those not managed, or half-added.
    pub fn addable(&self) -> Vec<&crate::reports::ListedVm> {
        self.inventory
            .ok()
            .map(|report| {
                report
                    .vms
                    .iter()
                    .filter(|vm| {
                        matches!(
                            vm.management,
                            Management::NotManaged | Management::Unfinished
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The VMs the remove screen offers: managed ones, and parked ones to
    /// bring back.
    pub fn removable(&self) -> Vec<&crate::reports::ListedVm> {
        self.inventory
            .ok()
            .map(|report| {
                report
                    .vms
                    .iter()
                    .filter(|vm| matches!(vm.management, Management::Managed | Management::Parked))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn on_add_key(&mut self, key: KeyEvent) {
        let count = self.addable().len();
        match key.code {
            KeyCode::Enter => {
                let Some(vm) = self.addable().get(self.add_cursor).map(|vm| (*vm).clone()) else {
                    return;
                };
                if !vm.problems.is_empty() {
                    self.status = Some((
                        format!(
                            "{} can't be added yet: fix the problems shown first",
                            vm.vm_name
                        ),
                        Tone::Warn,
                    ));
                    return;
                }
                self.start(Action::Add(vm.vm_name));
            }
            code => Self::move_cursor(&mut self.add_cursor, count, code),
        }
    }

    fn on_remove_key(&mut self, key: KeyEvent) {
        let count = self.removable().len();
        match key.code {
            KeyCode::Enter => {
                let Some(vm) = self
                    .removable()
                    .get(self.remove_cursor)
                    .map(|vm| (*vm).clone())
                else {
                    return;
                };
                let parked = vm.management == Management::Parked;
                self.dialog = Some(Dialog::Choice(remove_choice(
                    &vm.vm_name,
                    parked,
                    &self.context,
                )));
            }
            code => Self::move_cursor(&mut self.remove_cursor, count, code),
        }
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
                                action,
                            }],
                            cursor: 0,
                        }));
                    }
                    None => {
                        self.status = Some((
                            "this fix is done by hand: it is shown beneath the problem".to_owned(),
                            Tone::Dim,
                        ))
                    }
                }
            }
            code => Self::move_cursor(&mut self.doctor_cursor, count, code),
        }
    }

    fn on_logs_key(&mut self, key: KeyEvent) {
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
                let names = self.vm_names();
                self.log_filter = match &self.log_filter {
                    None => names.first().cloned(),
                    Some(current) => names
                        .iter()
                        .position(|name| name == current)
                        .and_then(|at| names.get(at + 1))
                        .cloned(),
                };
                self.log = Loaded::NotYet;
                self.log_scroll = 0;
                self.refresh_screen();
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
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.status = Some((
                            "the action is still running; its dialog closes when it ends"
                                .to_owned(),
                            Tone::Warn,
                        ));
                    }
                    _ => {}
                }
            }
            Dialog::Choice(choice) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.dialog = None,
                KeyCode::Up | KeyCode::Char('k') => choice.cursor = choice.cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    choice.cursor = (choice.cursor + 1).min(choice.choices.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(picked) = choice.choices.get(choice.cursor).cloned() {
                        self.start(picked.action);
                    }
                }
                KeyCode::Char(character) => {
                    if let Some(picked) = choice
                        .choices
                        .iter()
                        .find(|option| option.key == character)
                        .cloned()
                    {
                        self.start(picked.action);
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
                        self.status = Some(("nothing changed".to_owned(), Tone::Dim));
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

fn attest_choice(vm: &str) -> ChoiceDialog {
    ChoiceDialog {
        title: format!("Attest {vm} again"),
        text: vec![
            "Attest records how the VM is set up now, from its review profile.".to_owned(),
            "Needed after any change to its definition.".to_owned(),
        ],
        choices: vec![
            Choice {
                key: 'a',
                label: "Attest it as it runs".to_owned(),
                detail: format!("rampire vm attest {vm}"),
                action: Action::Attest {
                    vm: vm.to_owned(),
                    restart: false,
                },
            },
            Choice {
                key: 'r',
                label: "Restart it, then attest".to_owned(),
                detail: "a cold restart, so a changed definition takes effect".to_owned(),
                action: Action::Attest {
                    vm: vm.to_owned(),
                    restart: true,
                },
            },
        ],
        cursor: 0,
    }
}

/// Stop managing a VM: keep its settings or forget them; or for a parked
/// one, bring it back or forget it.
pub fn remove_choice(vm: &str, parked: bool, context: &Context) -> ChoiceDialog {
    let draft = context.draft();
    let parked_path = draft
        .join(crate::context::PARKED_DIRECTORY)
        .join(format!("{vm}.toml"));
    let forget = Choice {
        key: 'f',
        label: "Forget it".to_owned(),
        detail: "removes its settings, attestation, request state, definition backups and the review vm add wrote for it; its definition stays as vm add left it".to_owned(),
        action: Action::Remove {
            vm: vm.to_owned(),
            forget: true,
        },
    };
    if parked {
        return ChoiceDialog {
            title: format!("{vm} is parked"),
            text: vec![format!(
                "Its settings are kept in {}.",
                parked_path.display()
            )],
            choices: vec![
                Choice {
                    key: 'b',
                    label: "Bring it back".to_owned(),
                    detail: "vm add restores its settings as they were".to_owned(),
                    action: Action::Add(vm.to_owned()),
                },
                forget,
            ],
            cursor: 0,
        };
    }
    ChoiceDialog {
        title: format!("Stop managing {vm}"),
        text: vec![
            "It keeps the memory it has until its next cold start, which begins on boot RAM alone.".to_owned(),
        ],
        choices: vec![
            Choice {
                key: 'k',
                label: "Keep its settings (park it)".to_owned(),
                detail: format!(
                    "leaves its settings in {}, with its review and attestation, for vm add to bring it back",
                    parked_path.display()
                ),
                action: Action::Remove {
                    vm: vm.to_owned(),
                    forget: false,
                },
            },
            forget,
        ],
        cursor: 0,
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
mod tests {
    use super::*;
    use crate::tui::theme::{Depth, Icons, Palette};

    pub(crate) fn app() -> (App, std::sync::mpsc::Receiver<Msg>) {
        let (tx, rx) = jobs::channel();
        let context = Context {
            config: Some(std::path::PathBuf::from("/nonexistent")),
            connect: Some("test:///default".to_owned()),
            build: "0.1.0 (build test)".to_owned(),
            via: "tui",
        };
        let theme = Theme::new(Palette::Navy, Depth::Colours256, Icons { unicode: true });
        (App::new(context, theme, tx, ReaderControl::new()), rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn tab_digits_and_back_tab_move_between_screens() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.screen, Screen::Vm);
        press(&mut app, KeyCode::BackTab);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.screen, Screen::Logs);
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(app.screen, Screen::Doctor);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.overlay, Some(Overlay::Help));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.overlay, None);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
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
        assert_eq!(app.errors[0].error, "it broke");
        let Some(Dialog::Flow(flow)) = &app.dialog else {
            panic!("the dialog stays to show the error");
        };
        assert!(flow.lines.iter().any(|(line, _)| line == "Apply this? yes"));
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
        // The error stays in the list after the dialog closes.
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
