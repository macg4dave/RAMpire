//! Every key the TUI answers, in one table: what it does ([`Cmd`]), where
//! ([`Layer`]), its description for the help and the palette, and its hint
//! for the status bar. Keys are dispatched, listed and hinted from here
//! only, so they can't disagree.
//!
//! A binding of more than one key, such as `g g`, is a chord: its first key
//! opens a popup of what can follow ([`super::which`]).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::list::{Linemode, SortBy};
use super::scroll::Step;

/// Where keys go: the view at the bottom, or what is open over it. The
/// first in [`super::app::App::layer`] order takes a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    /// The pool and every VM.
    List,
    /// One VM live.
    Details,
    Doctor,
    Logs,
    /// The tasks list.
    Tasks,
    /// One task's output and its question.
    Task,
    /// A VM's facts.
    Spot,
    Errors,
    Confirm,
    /// The settings form.
    Form,
    Palette,
    Help,
    /// A filter or a find being typed.
    Prompt,
    /// A chord's or a VM menu's popup.
    Which,
}

impl Layer {
    /// A screen under everything else, which the keys for anywhere also
    /// reach.
    pub fn is_view(self) -> bool {
        matches!(self, Self::List | Self::Details | Self::Doctor | Self::Logs)
    }

    /// Something with rows to move through.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            Self::List
                | Self::Doctor
                | Self::Logs
                | Self::Tasks
                | Self::Task
                | Self::Spot
                | Self::Errors
        )
    }

    /// Its name in the help.
    pub fn name(self) -> &'static str {
        match self {
            Self::List => "the VMs",
            Self::Details => "a VM's details",
            Self::Doctor => "the doctor",
            Self::Logs => "the logs",
            Self::Tasks => "the tasks",
            Self::Task => "a task",
            Self::Spot => "a VM's facts",
            Self::Errors => "the errors",
            Self::Confirm => "a question",
            Self::Form => "the settings",
            Self::Palette => "the commands",
            Self::Help => "the keys",
            Self::Prompt => "a filter",
            Self::Which => "a menu",
        }
    }
}

/// Which layers a binding is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every view: the keys for anywhere.
    Views,
    /// Every layer with rows: moving.
    Lists,
    Only(Layer),
}

impl Scope {
    fn covers(self, layer: Layer) -> bool {
        match self {
            Self::Views => layer.is_view(),
            Self::Lists => layer.is_list(),
            Self::Only(only) => only == layer,
        }
    }
}

/// What a key does. Each layer gives the general ones their meaning there:
/// [`Cmd::Back`] closes a dialog or goes back to the list, [`Cmd::Copy`]
/// copies what the layer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Quit,
    Palette,
    Help,
    Errors,
    Tasks,
    NextTheme,
    Icons,
    Reload,
    Back,
    Move(Step),
    /// The doctor for the host and every VM.
    Doctor,
    /// Every journal line.
    Logs,
    /// The chosen VM's menu.
    Menu,
    Spot,
    /// Type a filter (the VMs, the doctor) or a find (the logs).
    Filter,
    Mark,
    MarkAll,
    Copy,
    /// A VM menu's letter, done for the chosen VM (or the marked ones).
    Letter(char),
    Sort(SortBy, bool),
    Linemode(Linemode),
    /// The VM before (-1) or after (1).
    Sibling(isize),
    RunFix,
    ProblemsOnly,
    FindNext,
    FindPrev,
    /// The logs of the next managed VM, then all of them again.
    NextVmLogs,
    Open,
    Cancel,
    ClearDone,
    Yes,
    No,
    /// Move between a question's buttons.
    Toggle,
    /// Enter: what is chosen.
    Choose,
    NextField,
    PrevField,
}

/// A key with its Ctrl and Alt; Shift is in the character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Key {
    pub const fn of(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    pub const fn ch(character: char) -> Self {
        Self::of(KeyCode::Char(character))
    }

    pub const fn ctrl(character: char) -> Self {
        Self {
            code: KeyCode::Char(character),
            modifiers: KeyModifiers::CONTROL,
        }
    }

    /// A key as pressed: a character's Shift is part of the character.
    pub fn from_event(event: &KeyEvent) -> Self {
        let mut modifiers = event
            .modifiers
            .intersection(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if matches!(event.code, KeyCode::Char(_) | KeyCode::BackTab) {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        let code = match event.code {
            // Ctrl with a letter arrives as the letter in either case.
            KeyCode::Char(character) if modifiers.contains(KeyModifiers::CONTROL) => {
                KeyCode::Char(character.to_ascii_lowercase())
            }
            code => code,
        };
        Self { code, modifiers }
    }

    /// As the help writes it: `q`, `Ctrl+P`, `Enter`, `Space`.
    pub fn label(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(character) if self.modifiers.contains(KeyModifiers::CONTROL) => {
                character.to_ascii_uppercase().to_string()
            }
            KeyCode::Char(character) => character.to_string(),
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::BackTab => "Shift+Tab".to_owned(),
            KeyCode::Backspace => "Backspace".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::Left => "Left".to_owned(),
            KeyCode::Right => "Right".to_owned(),
            KeyCode::Home => "Home".to_owned(),
            KeyCode::End => "End".to_owned(),
            KeyCode::PageUp => "PgUp".to_owned(),
            KeyCode::PageDown => "PgDn".to_owned(),
            KeyCode::Delete => "Del".to_owned(),
            KeyCode::F(number) => format!("F{number}"),
            _ => "?".to_owned(),
        };
        let mut label = String::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            label.push_str("Ctrl+");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            label.push_str("Alt+");
        }
        label.push_str(&base);
        label
    }
}

/// One key or chord and what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub scope: Scope,
    pub on: &'static [Key],
    pub cmd: Cmd,
    pub desc: &'static str,
    /// Its word in the status bar, if it has a place there.
    pub hint: Option<&'static str>,
}

impl Binding {
    /// Its keys as the help writes them: `g g`, `Ctrl+P`.
    pub fn label(&self) -> String {
        self.on.iter().map(Key::label).collect::<Vec<_>>().join(" ")
    }
}

const fn bind(scope: Scope, on: &'static [Key], cmd: Cmd, desc: &'static str) -> Binding {
    Binding {
        scope,
        on,
        cmd,
        desc,
        hint: None,
    }
}

const fn hinted(
    scope: Scope,
    on: &'static [Key],
    cmd: Cmd,
    desc: &'static str,
    hint: &'static str,
) -> Binding {
    Binding {
        scope,
        on,
        cmd,
        desc,
        hint: Some(hint),
    }
}

use Cmd as C;
use KeyCode as K;
use Layer as L;
use Scope::{Lists, Only, Views};

const fn ch(character: char) -> Key {
    Key::ch(character)
}

const fn of(code: KeyCode) -> Key {
    Key::of(code)
}

const fn ctrl(character: char) -> Key {
    Key::ctrl(character)
}

/// Every key, in the order the help and the status bar list them; one line
/// a key, so the table reads as one.
#[rustfmt::skip]
pub static BINDINGS: &[Binding] = &[
    // The VMs.
    hinted(Only(L::List), &[of(K::Enter)], C::Menu, "the chosen VM's menu", "menu"),
    hinted(Only(L::List), &[of(K::Tab)], C::Spot, "the chosen VM's facts", "facts"),
    hinted(Only(L::List), &[ch('/')], C::Filter, "show only the VMs whose name has what is typed", "filter"),
    hinted(Only(L::List), &[ch(' ')], C::Mark, "mark the chosen VM, for an action on several", "mark"),
    bind(Only(L::List), &[ctrl('a')], C::MarkAll, "mark every VM shown, or none"),
    bind(Only(L::List), &[ch('y')], C::Copy, "copy the chosen VM's name"),
    bind(Only(L::List), &[ch('v')], C::Letter('v'), "its details, live"),
    bind(Only(L::List), &[ch('s')], C::Letter('s'), "its settings: priority and its RAM limits"),
    bind(Only(L::List), &[ch('o')], C::Letter('o'), "its doctor"),
    bind(Only(L::List), &[ch('a')], C::Letter('a'), "install RAMpire on it, or finish installing"),
    bind(Only(L::List), &[ch('c')], C::Letter('c'), "restart it, so its saved definition takes effect"),
    hinted(Only(L::List), &[ch('d')], C::Doctor, "the doctor, for the host and every VM", "doctor"),
    bind(Only(L::List), &[ch('l')], C::Logs, "the logs"),
    bind(Only(L::List), &[ch(','), ch('o')], C::Sort(SortBy::Pool, false), "sort: as the pool lists them"),
    bind(Only(L::List), &[ch(','), ch('n')], C::Sort(SortBy::Name, false), "sort: by name"),
    bind(Only(L::List), &[ch(','), ch('N')], C::Sort(SortBy::Name, true), "sort: by name, reversed"),
    bind(Only(L::List), &[ch(','), ch('r')], C::Sort(SortBy::Plugged, true), "sort: most RAM plugged first"),
    bind(Only(L::List), &[ch(','), ch('R')], C::Sort(SortBy::Plugged, false), "sort: least RAM plugged first"),
    bind(Only(L::List), &[ch(','), ch('p')], C::Sort(SortBy::Priority, true), "sort: highest priority first"),
    bind(Only(L::List), &[ch(','), ch('P')], C::Sort(SortBy::Priority, false), "sort: lowest priority first"),
    bind(Only(L::List), &[ch(','), ch('u')], C::Sort(SortBy::GuestUse, true), "sort: guest's memory most in use first"),
    bind(Only(L::List), &[ch(','), ch('U')], C::Sort(SortBy::GuestUse, false), "sort: guest's memory least in use first"),
    bind(Only(L::List), &[ch('m'), ch('g')], C::Linemode(Linemode::Guest), "last columns: the guest's memory in use and its request"),
    bind(Only(L::List), &[ch('m'), ch('l')], C::Linemode(Linemode::Limits), "last columns: minimum, maximum and free RAM"),
    bind(Only(L::List), &[ch('m'), ch('d')], C::Linemode(Linemode::Device), "last columns: the memory device: requested, size and block"),
    bind(Only(L::List), &[ch('m'), ch('t')], C::Linemode(Linemode::Detail), "last columns: a line about each VM"),
    // A VM's details.
    hinted(Only(L::Details), &[of(K::Enter)], C::Menu, "its menu", "menu"),
    hinted(Only(L::Details), &[of(K::Left)], C::Sibling(-1), "the VM before", "VM"),
    bind(Only(L::Details), &[of(K::Up)], C::Sibling(-1), "the VM before"),
    bind(Only(L::Details), &[of(K::Right)], C::Sibling(1), "the VM after"),
    bind(Only(L::Details), &[of(K::Down)], C::Sibling(1), "the VM after"),
    hinted(Only(L::Details), &[of(K::Tab)], C::Spot, "its facts", "facts"),
    bind(Only(L::Details), &[ch('y')], C::Copy, "copy the command that shows the same: rampire watch"),
    hinted(Only(L::Details), &[ch('s')], C::Letter('s'), "its settings", "settings"),
    bind(Only(L::Details), &[ch('o')], C::Letter('o'), "its doctor"),
    bind(Only(L::Details), &[ch('l')], C::Letter('l'), "its logs"),
    bind(Only(L::Details), &[ch('c')], C::Letter('c'), "restart it, so its saved definition takes effect"),
    bind(Only(L::Details), &[ch('k')], C::Letter('k'), "remove it: stop managing it and delete what RAMpire kept for it"),
    // The doctor.
    hinted(Only(L::Doctor), &[of(K::Enter)], C::RunFix, "run the chosen fix, when RAMpire can run it", "run the fix"),
    hinted(Only(L::Doctor), &[ch('/')], C::Filter, "show only the findings with what is typed", "filter"),
    hinted(Only(L::Doctor), &[ch('p')], C::ProblemsOnly, "show only the problems, or everything", "problems"),
    bind(Only(L::Doctor), &[ch('y')], C::Copy, "copy the chosen finding's fix"),
    // The logs.
    hinted(Only(L::Logs), &[ch('/')], C::Filter, "find a text: the lines with it are marked", "find"),
    hinted(Only(L::Logs), &[ch('n')], C::FindNext, "the next line found", "next"),
    bind(Only(L::Logs), &[ch('N')], C::FindPrev, "the line found before"),
    hinted(Only(L::Logs), &[ch('f')], C::NextVmLogs, "show one VM's lines: the next VM, then every line", "one VM"),
    bind(Only(L::Logs), &[ch('y')], C::Copy, "copy the chosen line"),
    // The tasks.
    hinted(Only(L::Tasks), &[of(K::Enter)], C::Open, "the chosen task's output, and its question", "open"),
    hinted(Only(L::Tasks), &[ch('x')], C::Cancel, "cancel the chosen task: a waiting one at once, a running one at its next question", "cancel"),
    hinted(Only(L::Tasks), &[ch('D')], C::ClearDone, "clear the finished tasks", "clear done"),
    bind(Only(L::Tasks), &[ch('y')], C::Copy, "copy the chosen task's command line"),
    hinted(Only(L::Tasks), &[of(K::Esc)], C::Back, "close the tasks", "close"),
    bind(Only(L::Tasks), &[ch('w')], C::Back, "close the tasks"),
    bind(Only(L::Tasks), &[ch('q')], C::Back, "close the tasks"),
    // One task.
    hinted(Only(L::Task), &[ch('y')], C::Yes, "answer yes", "yes"),
    hinted(Only(L::Task), &[ch('n')], C::No, "answer no", "no"),
    bind(Only(L::Task), &[of(K::Left)], C::Toggle, "the other button"),
    bind(Only(L::Task), &[of(K::Right)], C::Toggle, "the other button"),
    bind(Only(L::Task), &[of(K::Tab)], C::Toggle, "the other button"),
    hinted(Only(L::Task), &[of(K::Enter)], C::Choose, "the chosen button, or the text typed", "answer"),
    bind(Only(L::Task), &[ch('x')], C::Cancel, "cancel the task, changing nothing more"),
    bind(Only(L::Task), &[ch('c')], C::Copy, "copy the task's command line"),
    hinted(Only(L::Task), &[of(K::Esc)], C::Back, "close; the task goes on, and a question waits", "close"),
    // A VM's facts.
    hinted(Only(L::Spot), &[ch('y')], C::Copy, "copy the chosen fact", "copy"),
    hinted(Only(L::Spot), &[of(K::Left)], C::Sibling(-1), "the VM before", "VM"),
    bind(Only(L::Spot), &[of(K::Right)], C::Sibling(1), "the VM after"),
    hinted(Only(L::Spot), &[of(K::Esc)], C::Back, "close", "close"),
    bind(Only(L::Spot), &[of(K::Tab)], C::Back, "close"),
    bind(Only(L::Spot), &[ch('q')], C::Back, "close"),
    // The errors.
    bind(Only(L::Errors), &[ch('y')], C::Copy, "copy the chosen error"),
    hinted(Only(L::Errors), &[of(K::Esc)], C::Back, "close", "close"),
    bind(Only(L::Errors), &[ch('e')], C::Back, "close"),
    bind(Only(L::Errors), &[ch('q')], C::Back, "close"),
    bind(Only(L::Errors), &[of(K::Enter)], C::Back, "close"),
    // A question.
    hinted(Only(L::Confirm), &[ch('y')], C::Yes, "yes", "yes"),
    hinted(Only(L::Confirm), &[ch('n')], C::No, "no: nothing is done", "no"),
    bind(Only(L::Confirm), &[of(K::Left)], C::Toggle, "the other button"),
    bind(Only(L::Confirm), &[of(K::Right)], C::Toggle, "the other button"),
    bind(Only(L::Confirm), &[of(K::Tab)], C::Toggle, "the other button"),
    hinted(Only(L::Confirm), &[of(K::Enter)], C::Choose, "the chosen button", "choose"),
    bind(Only(L::Confirm), &[of(K::Up)], C::Move(Step::Prev), "scroll the list up"),
    bind(Only(L::Confirm), &[of(K::Down)], C::Move(Step::Next), "scroll the list down"),
    bind(Only(L::Confirm), &[of(K::Esc)], C::No, "no: nothing is done"),
    // The settings form.
    hinted(Only(L::Form), &[of(K::Tab)], C::NextField, "the next setting", "next"),
    bind(Only(L::Form), &[of(K::Down)], C::NextField, "the next setting"),
    bind(Only(L::Form), &[of(K::BackTab)], C::PrevField, "the setting before"),
    bind(Only(L::Form), &[of(K::Up)], C::PrevField, "the setting before"),
    hinted(Only(L::Form), &[of(K::Enter)], C::Choose, "apply the changes", "apply"),
    hinted(Only(L::Form), &[of(K::Esc)], C::Back, "close, changing nothing", "back"),
    // The command palette.
    hinted(Only(L::Palette), &[of(K::Enter)], C::Choose, "run the chosen command", "run"),
    bind(Only(L::Palette), &[of(K::Up)], C::Move(Step::Prev), "the command before"),
    bind(Only(L::Palette), &[of(K::Down)], C::Move(Step::Next), "the next command"),
    bind(Only(L::Palette), &[ctrl('n')], C::Move(Step::Next), "the next command"),
    bind(Only(L::Palette), &[ctrl('p')], C::Move(Step::Prev), "the command before"),
    bind(Only(L::Palette), &[of(K::PageUp)], C::Move(Step::Percent(-100)), "a page up"),
    bind(Only(L::Palette), &[of(K::PageDown)], C::Move(Step::Percent(100)), "a page down"),
    hinted(Only(L::Palette), &[of(K::Esc)], C::Back, "close", "close"),
    // The keys.
    hinted(Only(L::Help), &[of(K::Enter)], C::Choose, "do what the chosen key does", "do it"),
    bind(Only(L::Help), &[of(K::Up)], C::Move(Step::Prev), "the key before"),
    bind(Only(L::Help), &[of(K::Down)], C::Move(Step::Next), "the next key"),
    bind(Only(L::Help), &[of(K::PageUp)], C::Move(Step::Percent(-100)), "a page up"),
    bind(Only(L::Help), &[of(K::PageDown)], C::Move(Step::Percent(100)), "a page down"),
    hinted(Only(L::Help), &[of(K::Esc)], C::Back, "close", "close"),
    // A filter or find being typed.
    hinted(Only(L::Prompt), &[of(K::Enter)], C::Choose, "keep it", "keep"),
    hinted(Only(L::Prompt), &[of(K::Esc)], C::Back, "clear it", "clear"),
    // Moving, wherever there are rows.
    bind(Lists, &[ch('j')], C::Move(Step::Next), "down"),
    bind(Lists, &[of(K::Down)], C::Move(Step::Next), "down"),
    bind(Lists, &[ch('k')], C::Move(Step::Prev), "up"),
    bind(Lists, &[of(K::Up)], C::Move(Step::Prev), "up"),
    bind(Lists, &[ch('g'), ch('g')], C::Move(Step::Top), "to the top"),
    bind(Lists, &[of(K::Home)], C::Move(Step::Top), "to the top"),
    bind(Lists, &[ch('G')], C::Move(Step::Bottom), "to the bottom"),
    bind(Lists, &[of(K::End)], C::Move(Step::Bottom), "to the bottom"),
    bind(Lists, &[ctrl('u')], C::Move(Step::Percent(-50)), "half a page up"),
    bind(Lists, &[ctrl('d')], C::Move(Step::Percent(50)), "half a page down"),
    bind(Lists, &[of(K::PageUp)], C::Move(Step::Percent(-100)), "a page up"),
    bind(Lists, &[of(K::PageDown)], C::Move(Step::Percent(100)), "a page down"),
    bind(Lists, &[ctrl('b')], C::Move(Step::Percent(-100)), "a page up"),
    bind(Lists, &[ctrl('f')], C::Move(Step::Percent(100)), "a page down"),
    // Anywhere.
    hinted(Views, &[ch('w')], C::Tasks, "the tasks: what is running, waiting and done", "tasks"),
    bind(Views, &[ch('e')], C::Errors, "the errors list"),
    hinted(Views, &[ctrl('p')], C::Palette, "the command palette: find anything by a few letters", "commands"),
    bind(Views, &[ch(':')], C::Palette, "the command palette"),
    hinted(Views, &[ch('?')], C::Help, "the keys, for what is shown", "keys"),
    bind(Views, &[ch('g'), ch('d')], C::Doctor, "go to the doctor"),
    bind(Views, &[ch('g'), ch('l')], C::Logs, "go to the logs"),
    bind(Views, &[ch('g'), ch('w')], C::Tasks, "go to the tasks"),
    bind(Views, &[ch('g'), ch('e')], C::Errors, "go to the errors"),
    bind(Views, &[ch('r')], C::Reload, "refresh: read everything again"),
    bind(Views, &[ch('t')], C::NextTheme, "the next colour theme"),
    bind(Views, &[ch('i')], C::Icons, "switch between Unicode and plain ASCII icons"),
    hinted(Views, &[of(K::Esc)], C::Back, "back to the VMs; clear a filter and the marks", "back"),
    hinted(Views, &[ch('q')], C::Quit, "quit", "quit"),
];

/// What a key does in a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Run(Cmd),
    /// The first key of chords: what can follow.
    Chord(Vec<&'static Binding>),
    None,
}

/// The bindings that work in `layer`, in the table's order.
pub fn bindings(layer: Layer) -> impl Iterator<Item = &'static Binding> {
    BINDINGS
        .iter()
        .filter(move |binding| binding.scope.covers(layer))
}

/// What `key` does in `layer`: a layer's own keys come before the keys for
/// every list, and those before the keys for anywhere.
pub fn lookup(layer: Layer, key: Key) -> Lookup {
    let starting: Vec<&'static Binding> = bindings(layer)
        .filter(|binding| binding.on.first() == Some(&key))
        .collect();
    let chords: Vec<&'static Binding> = starting
        .iter()
        .copied()
        .filter(|binding| binding.on.len() > 1)
        .collect();
    if let Some(single) = starting.iter().find(|binding| binding.on.len() == 1) {
        return Lookup::Run(single.cmd);
    }
    if chords.is_empty() {
        Lookup::None
    } else {
        Lookup::Chord(chords)
    }
}

/// The status bar's hints for `layer`: each hinted key once.
pub fn hints(layer: Layer) -> Vec<(String, &'static str)> {
    let mut seen = Vec::new();
    bindings(layer)
        .filter_map(|binding| binding.hint.map(|hint| (binding.label(), hint)))
        .filter(|(_, hint)| {
            let new = !seen.contains(hint);
            seen.push(*hint);
            new
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYERS: [Layer; 14] = [
        Layer::List,
        Layer::Details,
        Layer::Doctor,
        Layer::Logs,
        Layer::Tasks,
        Layer::Task,
        Layer::Spot,
        Layer::Errors,
        Layer::Confirm,
        Layer::Form,
        Layer::Palette,
        Layer::Help,
        Layer::Prompt,
        Layer::Which,
    ];

    #[test]
    fn no_key_means_two_things_in_one_layer() {
        // A layer's own key may stand in front of a general one; within one
        // scope each key or chord is there once.
        for layer in LAYERS {
            let mut seen: Vec<(Scope, &[Key])> = Vec::new();
            for binding in bindings(layer) {
                assert!(
                    !seen.contains(&(binding.scope, binding.on)),
                    "{layer:?}: {} twice",
                    binding.label()
                );
                seen.push((binding.scope, binding.on));
            }
        }
    }

    #[test]
    fn a_chords_first_key_opens_it_and_a_single_key_runs() {
        assert_eq!(lookup(Layer::List, Key::ch('q')), Lookup::Run(Cmd::Quit));
        assert_eq!(
            lookup(Layer::List, Key::ch('k')),
            Lookup::Run(Cmd::Move(Step::Prev)),
            "k moves in the list; removing is in the menu"
        );
        assert_eq!(
            lookup(Layer::Details, Key::ch('k')),
            Lookup::Run(Cmd::Letter('k'))
        );
        let Lookup::Chord(chords) = lookup(Layer::List, Key::ch('g')) else {
            panic!("g starts chords");
        };
        assert!(chords
            .iter()
            .any(|binding| binding.cmd == Cmd::Move(Step::Top)));
        assert!(chords.iter().any(|binding| binding.cmd == Cmd::Doctor));
        assert_eq!(lookup(Layer::Task, Key::ch('q')), Lookup::None);
        assert_eq!(lookup(Layer::Tasks, Key::ch('q')), Lookup::Run(Cmd::Back));
    }

    #[test]
    fn keys_are_read_as_pressed_and_written_as_the_help_writes_them() {
        let shifted = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(Key::from_event(&shifted), Key::ch('G'));
        let control = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::CONTROL);
        assert_eq!(Key::from_event(&control), Key::ctrl('p'));
        assert_eq!(Key::ctrl('p').label(), "Ctrl+P");
        assert_eq!(Key::ch(' ').label(), "Space");
        let top = bindings(Layer::List)
            .find(|binding| binding.cmd == Cmd::Move(Step::Top))
            .unwrap();
        assert_eq!(top.label(), "g g");
    }

    #[test]
    fn every_view_hints_its_keys_once() {
        let hints = hints(Layer::List);
        let words: Vec<&str> = hints.iter().map(|(_, hint)| *hint).collect();
        assert_eq!(&words[..3], ["menu", "facts", "filter"]);
        assert!(words.contains(&"quit") && words.contains(&"tasks"));
        let backs = words.iter().filter(|word| **word == "back").count();
        assert_eq!(backs, 1);
    }
}
