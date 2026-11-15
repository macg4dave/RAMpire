//! Where a key goes, as Yazi routes them: a few keys work everywhere, then
//! text being typed takes what it can, then the topmost layer's keys from
//! [`super::keymap`], then the keys of every list and of anywhere. A
//! command then does what it means in that layer.

use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use super::app::{App, Effect, Loaded, Overlay, Prompt, Run, View};
use super::dialog::{Confirm, Dialog, OnYes};
use super::help::Help;
use super::input::Input;
use super::jobs::Answer;
use super::keymap::{self, Cmd, Key, Layer, Lookup};
use super::scroll::{Scroll, Step};
use super::spot::Spot;
use super::tasks::Cancelled;
use super::which::{Typed, Which};
use crate::tone::Tone;

impl App {
    pub fn on_key(&mut self, event: KeyEvent) {
        if event.kind == KeyEventKind::Release {
            return;
        }
        let key = Key::from_event(&event);
        match key {
            _ if key == Key::ctrl('c') => self.ask_quit(true),
            _ if key == Key::ctrl('z') => self.effects.push(Effect::Suspend),
            _ if key == Key::of(KeyCode::F(1)) => self.open_help(),
            _ => self.route(key, &event),
        }
        self.follow_selection();
        self.surface_question();
    }

    fn route(&mut self, key: Key, event: &KeyEvent) {
        let layer = self.layer();
        if layer == Layer::Which {
            return self.type_which(key);
        }
        if self.type_text(layer, event) {
            return self.after_text(layer);
        }
        match keymap::lookup(layer, key) {
            Lookup::Run(cmd) => self.dispatch(cmd),
            Lookup::Chord(bindings) => self.which = Some(Which::chord(key, &bindings)),
            Lookup::None => {}
        }
    }

    fn type_which(&mut self, key: Key) {
        let Some(which) = self.which.as_mut() else {
            return;
        };
        if let Typed::Done(run) = which.r#type(key) {
            self.which = None;
            if let Some(run) = run {
                self.run(run);
            }
        }
    }

    /// Give `event` to the text field of `layer`, if it has one; whether
    /// the field used it.
    fn type_text(&mut self, layer: Layer, event: &KeyEvent) -> bool {
        let history = &self.history;
        let field = match layer {
            Layer::Help => self.help.as_mut().map(|help| &mut help.input),
            Layer::Palette => self.palette.as_mut().map(|palette| &mut palette.input),
            Layer::Prompt => self.prompt.as_mut().map(|prompt| &mut prompt.input),
            Layer::Form => match &mut self.dialog {
                Some(Dialog::Form(form)) => {
                    let cursor = form.cursor;
                    form.fields.get_mut(cursor).map(|field| &mut field.input)
                }
                _ => None,
            },
            Layer::Task => match &self.overlay {
                Some(Overlay::Task { id, .. }) => {
                    let id = *id;
                    self.tasks
                        .get_mut(id)
                        .and_then(|task| task.pending.as_mut())
                        .filter(|pending| pending.question.typed())
                        .map(|pending| &mut pending.input)
                }
                _ => None,
            },
            _ => None,
        };
        // The palette and the help move with Up and Down; their fields
        // don't recall earlier text.
        let arrows = matches!(event.code, KeyCode::Up | KeyCode::Down)
            && matches!(layer, Layer::Palette | Layer::Help | Layer::Form);
        match field {
            Some(field) if !arrows => field.handle(event, history),
            _ => false,
        }
    }

    /// After the text of `layer` changed: what it does as it is typed.
    fn after_text(&mut self, layer: Layer) {
        match layer {
            Layer::Help => {
                if let Some(help) = self.help.as_mut() {
                    help.scroll = Scroll::default();
                }
            }
            Layer::Palette => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.scroll = Scroll::default();
                }
                self.preview_theme();
            }
            Layer::Prompt => self.apply_prompt(),
            _ => {}
        }
    }

    /// Do what `run` says, in the layer now on top.
    pub(super) fn run(&mut self, run: Run) {
        match run {
            Run::Cmd(cmd) => self.dispatch(cmd),
            Run::Pick(pick) => self.pick(pick),
            Run::Copy(text) => self.copy(text),
            Run::Theme(palette) => {
                self.theme.palette = palette;
                self.notify(format!("Theme: {}", palette.name()), Tone::Dim);
            }
        }
    }

    /// Do `cmd` in the layer now on top.
    pub fn dispatch(&mut self, cmd: Cmd) {
        let layer = self.layer();
        match layer {
            Layer::List => self.on_list(cmd),
            Layer::Details => self.on_details(cmd),
            Layer::Doctor => self.on_doctor(cmd),
            Layer::Logs => self.on_logs(cmd),
            Layer::Tasks => self.on_tasks(cmd),
            Layer::Task => self.on_task_view(cmd),
            Layer::Spot => self.on_spot(cmd),
            Layer::Errors => self.on_errors(cmd),
            Layer::Confirm => self.on_confirm(cmd),
            Layer::Form => self.on_form(cmd),
            Layer::Palette => self.on_palette(cmd),
            Layer::Help => self.on_help(cmd),
            Layer::Prompt => self.on_prompt(cmd),
            Layer::Which => {}
        }
    }

    /// The keys for anywhere, which every view shares.
    fn on_anywhere(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Quit => self.ask_quit(false),
            Cmd::Palette => self.open_palette(),
            Cmd::Help => self.open_help(),
            Cmd::Errors if self.errors.is_empty() => self.notify("No errors so far.", Tone::Dim),
            Cmd::Errors => self.overlay = Some(Overlay::Errors(Scroll::at(usize::MAX))),
            Cmd::Tasks => {
                let at = self.tasks.list.len().saturating_sub(1);
                self.tasks.scroll = Scroll::at(at);
                self.overlay = Some(Overlay::Tasks);
            }
            Cmd::NextTheme => {
                self.theme.next_palette();
                self.notify(format!("Theme: {}", self.theme.palette.name()), Tone::Dim);
            }
            Cmd::Icons => {
                self.theme.icons.unicode = !self.theme.icons.unicode;
                let set = if self.theme.icons.unicode {
                    "Unicode"
                } else {
                    "ASCII"
                };
                self.notify(format!("Icons: {set}"), Tone::Dim);
            }
            Cmd::Reload => self.reload(),
            Cmd::Doctor => self.open(View::Doctor(None)),
            Cmd::Setup => self.setup_form(),
            Cmd::Global => self.global_form(),
            Cmd::Apply => {
                self.start(super::actions::Action::Apply);
            }
            Cmd::Rollback => {
                self.start(super::actions::Action::Rollback);
            }
            // Shown at once: it asks nothing, so it wouldn't come forward.
            Cmd::Diff => {
                let id = self.start(super::actions::Action::Diff);
                self.overlay = Some(Overlay::Task {
                    id,
                    from_list: false,
                });
            }
            Cmd::Logs => self.open(View::Logs(None)),
            Cmd::Back if self.view != View::Main => self.open(View::Main),
            _ => {}
        }
    }

    fn open_help(&mut self) {
        let layer = match self.layer() {
            Layer::Help | Layer::Which => return,
            layer => layer,
        };
        self.help = Some(Help::new(layer));
    }

    /// `q` asks first while tasks are running or waiting; Yes stops them
    /// at a safe point, then quits. Ctrl+C a second time quits at once.
    pub(super) fn ask_quit(&mut self, hard: bool) {
        let asked = matches!(
            &self.dialog,
            Some(Dialog::Confirm(Confirm {
                on_yes: OnYes::Quit,
                ..
            }))
        );
        if self.tasks.active() == 0 || (hard && (asked || self.quitting)) {
            self.quit = true;
            return;
        }
        let list = self
            .tasks
            .list
            .iter()
            .filter(|task| !task.state.finished())
            .map(|task| task.title.clone())
            .collect();
        self.dialog = Some(Dialog::Confirm(Confirm::new(
            "Quit?",
            vec![
                "These tasks are stopped first: a waiting one at once, a running one at its next question or wait, so no step is cut off half-way. Then the TUI closes. Ctrl+C again quits at once."
                    .to_owned(),
            ],
            list,
            OnYes::Quit,
        )));
    }

    /// Stop every task, then quit once none runs: a running one stops at
    /// its next question or wait.
    pub(super) fn stop_and_quit(&mut self) {
        let unfinished: Vec<super::jobs::TaskId> = self
            .tasks
            .list
            .iter()
            .filter(|task| !task.state.finished())
            .map(|task| task.id)
            .collect();
        for id in unfinished {
            self.tasks.cancel(id, Instant::now());
        }
        if self.tasks.active() == 0 {
            self.quit = true;
            return;
        }
        self.quitting = true;
        self.notify(
            "Stopping: the running task stops at its next question or wait, then the TUI closes. Ctrl+C quits at once.",
            Tone::Warn,
        );
    }

    fn on_list(&mut self, cmd: Cmd) {
        let count = self.visible().len();
        let hovered = self.selected();
        match cmd {
            Cmd::Move(step) => {
                self.list.scroll.step(step, count);
            }
            Cmd::Menu => {
                if let Some(entry) = hovered {
                    self.open_menu(&entry);
                }
            }
            Cmd::Spot => {
                if let Some(entry) = hovered {
                    self.overlay = Some(Overlay::Spot(Spot {
                        vm: entry.name,
                        scroll: Scroll::default(),
                    }));
                }
            }
            Cmd::Filter => {
                let names = self.entries().into_iter().map(|entry| entry.name).collect();
                let current = self.list.filter.clone();
                self.prompt = Some(Prompt {
                    input: Input::new(current.as_deref().unwrap_or(""))
                        .remembering("filter")
                        .offering(names),
                    before: current,
                });
            }
            Cmd::Mark => {
                if let Some(entry) = hovered {
                    if !self.list.marks.remove(&entry.name) {
                        self.list.marks.insert(entry.name);
                    }
                    self.list.scroll.step(Step::Offset(1), count);
                }
            }
            Cmd::MarkAll => {
                let shown: Vec<String> =
                    self.visible().into_iter().map(|entry| entry.name).collect();
                if shown.iter().all(|name| self.list.marks.contains(name)) {
                    self.list.marks.clear();
                } else {
                    self.list.marks.extend(shown);
                }
            }
            Cmd::Copy => {
                if let Some(entry) = hovered {
                    self.copy(entry.name);
                }
            }
            Cmd::Letter(letter) if !self.list.marks.is_empty() && "asxubc".contains(letter) => {
                self.for_marked(letter)
            }
            Cmd::Letter(letter) => {
                if let Some(entry) = hovered {
                    self.shortcut(&entry, letter);
                }
            }
            Cmd::Sort(by, reverse) => {
                self.list.sort = by;
                self.list.reverse = reverse;
                self.notify(format!("Sorted by {}", by.label()), Tone::Dim);
            }
            Cmd::Linemode(mode) => {
                self.list.linemode = mode;
                self.notify(
                    format!("The last columns show: {}", mode.label()),
                    Tone::Dim,
                );
            }
            Cmd::Back if self.list.filter.is_some() || !self.list.marks.is_empty() => {
                self.list.filter = None;
                self.list.marks.clear();
            }
            cmd => self.on_anywhere(cmd),
        }
    }

    fn on_details(&mut self, cmd: Cmd) {
        let View::Details(vm) = self.view.clone() else {
            return;
        };
        let entry = self.entry(&vm);
        match cmd {
            Cmd::Sibling(by) => {
                let managed = self.managed();
                let Some(at) = managed.iter().position(|name| *name == vm) else {
                    return;
                };
                let next = at
                    .saturating_add_signed(by)
                    .min(managed.len().saturating_sub(1));
                if let Some(name) = managed.get(next).cloned() {
                    self.choose(&name);
                    self.open(View::Details(name));
                }
            }
            Cmd::Menu => {
                if let Some(entry) = entry {
                    self.open_menu(&entry);
                }
            }
            Cmd::Spot => {
                self.overlay = Some(Overlay::Spot(Spot {
                    vm,
                    scroll: Scroll::default(),
                }));
            }
            Cmd::Copy => {
                let line = self.context.command_line(&["watch", &vm]);
                self.copy(line);
            }
            Cmd::Letter(letter) => {
                if let Some(entry) = entry {
                    self.shortcut(&entry, letter);
                }
            }
            cmd => self.on_anywhere(cmd),
        }
    }

    /// Put the list's cursor on `name`.
    pub(super) fn choose(&mut self, name: &str) {
        if let Some(at) = self.visible().iter().position(|entry| entry.name == name) {
            self.list.scroll.cursor = at;
        }
    }

    fn on_doctor(&mut self, cmd: Cmd) {
        let rows = self.doctor_rows();
        let count = rows.len();
        let chosen = rows.get(self.doctor.scroll.cursor(count));
        let fix = chosen.and_then(|(_, finding)| finding.fix.clone());
        let run = chosen.and_then(|(_, finding)| finding.run.clone());
        match cmd {
            Cmd::Move(step) => {
                self.doctor.scroll.step(step, count);
            }
            Cmd::RunFix => match run.as_deref() {
                // One VM's doctor, from the doctor for every VM.
                Some([doctor, vm]) if doctor == "doctor" => {
                    self.open(View::Doctor(Some(vm.clone())));
                }
                _ => match run.as_deref().and_then(super::actions::from_words) {
                    Some(action) => {
                        let line = action.command_line(&self.context);
                        self.dialog = Some(Dialog::Confirm(Confirm::new(
                            "Run the fix",
                            vec!["This runs in the background; its own questions come next:"
                                .to_owned()],
                            vec![line],
                            OnYes::Run(vec![action]),
                        )));
                    }
                    None => self.notify(
                        "This fix is done by hand: it is shown under the problem.",
                        Tone::Dim,
                    ),
                },
            },
            Cmd::Copy => match fix {
                Some(fix) => self.copy(fix),
                None => self.notify("This finding has no fix to copy.", Tone::Dim),
            },
            Cmd::Filter => {
                let current = self.doctor.filter.clone();
                self.prompt = Some(Prompt {
                    input: Input::new(current.as_deref().unwrap_or(""))
                        .remembering("doctor filter"),
                    before: current,
                });
            }
            Cmd::ProblemsOnly => {
                self.doctor.problems_only = !self.doctor.problems_only;
                self.doctor.scroll = Scroll::default();
            }
            Cmd::Back if self.doctor.filter.is_some() => self.doctor.filter = None,
            cmd => self.on_anywhere(cmd),
        }
    }

    fn on_logs(&mut self, cmd: Cmd) {
        let count = self.log_lines().len();
        match cmd {
            Cmd::Move(step) => {
                self.logs.scroll.step(step, count);
            }
            Cmd::Filter => {
                let current = self.logs.find.clone();
                self.prompt = Some(Prompt {
                    input: Input::new(current.as_deref().unwrap_or("")).remembering("find"),
                    before: current,
                });
            }
            Cmd::FindNext => self.find(true),
            Cmd::FindPrev => self.find(false),
            Cmd::NextVmLogs => {
                let View::Logs(vm) = self.view.clone() else {
                    return;
                };
                let names = self.managed();
                let next = match &vm {
                    None => names.first().cloned(),
                    Some(current) => names
                        .iter()
                        .position(|name| name == current)
                        .and_then(|at| names.get(at + 1))
                        .cloned(),
                };
                self.logs.loaded = Loaded::NotYet;
                self.open(View::Logs(next));
            }
            Cmd::Copy => {
                let lines = self.log_lines();
                if let Some(line) = lines.get(self.logs.scroll.cursor(lines.len())) {
                    let line = (*line).to_owned();
                    self.copy(line);
                }
            }
            Cmd::Back if self.logs.find.is_some() => self.logs.find = None,
            cmd => self.on_anywhere(cmd),
        }
    }

    /// The next (or previous) line with the text found, round the end.
    fn find(&mut self, forward: bool) {
        let lines = self.log_lines();
        let count = lines.len();
        if count == 0 || self.logs.find.is_none() {
            return self.notify("Nothing to find yet: press / and type a text.", Tone::Dim);
        }
        let at = self.logs.scroll.cursor(count);
        let found = (1..=count)
            .map(|step| {
                if forward {
                    (at + step) % count
                } else {
                    (at + count - step % count) % count
                }
            })
            .find(|index| lines.get(*index).is_some_and(|line| self.found(line)));
        match found {
            Some(index) => {
                let delta = index as isize - at as isize;
                self.logs.scroll.step(Step::Offset(delta), count);
            }
            None => self.notify("No line has that text.", Tone::Dim),
        }
    }

    fn on_tasks(&mut self, cmd: Cmd) {
        let count = self.tasks.list.len();
        let chosen = self
            .tasks
            .list
            .get(self.tasks.scroll.cursor(count))
            .map(|task| (task.id, task.title.clone()));
        match cmd {
            Cmd::Move(step) => {
                self.tasks.scroll.step(step, count);
            }
            Cmd::Open => {
                if let Some((id, _)) = chosen {
                    if let Some(pending) = self
                        .tasks
                        .get_mut(id)
                        .and_then(|task| task.pending.as_mut())
                    {
                        pending.later = false;
                    }
                    self.overlay = Some(Overlay::Task {
                        id,
                        from_list: true,
                    });
                }
            }
            Cmd::Cancel => {
                if let Some((id, _)) = chosen {
                    self.cancel_task(id);
                }
            }
            Cmd::ClearDone => self.tasks.clear_finished(),
            Cmd::Copy => {
                if let Some((_, title)) = chosen {
                    self.copy(title);
                }
            }
            Cmd::Back => self.overlay = None,
            _ => {}
        }
    }

    fn cancel_task(&mut self, id: super::jobs::TaskId) {
        let (text, tone) = match self.tasks.cancel(id, Instant::now()) {
            Cancelled::Dropped => ("Cancelled: it won't run.", Tone::Good),
            Cancelled::Stopped => (
                "Cancelled at its question: nothing more is changed.",
                Tone::Good,
            ),
            Cancelled::AtNextQuestion => (
                "It is running: it stops at its next question or wait, changing nothing more.",
                Tone::Warn,
            ),
            Cancelled::Finished => ("It has already finished.", Tone::Dim),
        };
        self.notify(text, tone);
    }

    fn on_task_view(&mut self, cmd: Cmd) {
        let Some(Overlay::Task { id, from_list }) = self.overlay.clone() else {
            return;
        };
        let Some(task) = self.tasks.get_mut(id) else {
            self.overlay = None;
            return;
        };
        let text = task
            .pending
            .as_ref()
            .map(|pending| pending.question.typed());
        let back = |app: &mut App| {
            app.overlay = if from_list {
                Some(Overlay::Tasks)
            } else {
                None
            };
        };
        match (cmd, text) {
            (Cmd::Move(step), _) => task.pager.step(step),
            (Cmd::Yes, Some(false)) => self.answer(id, Answer::Yes),
            (Cmd::No, Some(false)) => self.answer(id, Answer::No),
            (Cmd::Toggle, Some(false)) => {
                if let Some(pending) = task.pending.as_mut() {
                    pending.yes = !pending.yes;
                }
            }
            (Cmd::Choose, Some(false)) => {
                let yes = task.pending.as_ref().is_some_and(|pending| pending.yes);
                self.answer(id, if yes { Answer::Yes } else { Answer::No });
            }
            (Cmd::Choose, Some(true)) => {
                let value = task
                    .pending
                    .as_ref()
                    .map(|pending| pending.input.value().to_owned())
                    .unwrap_or_default();
                self.answer(id, Answer::Text(value));
            }
            (Cmd::Choose, None) => back(self),
            (Cmd::Cancel, _) => self.cancel_task(id),
            (Cmd::Copy, _) => {
                let title = task.title.clone();
                self.copy(title);
            }
            (Cmd::Back, _) => {
                if let Some(pending) = task.pending.as_mut() {
                    pending.later = true;
                    self.notify(
                        "The question is waiting: press w, then Enter, to answer it.",
                        Tone::Dim,
                    );
                }
                back(self);
            }
            _ => {}
        }
    }

    fn on_spot(&mut self, cmd: Cmd) {
        let Some(Overlay::Spot(spot)) = &self.overlay else {
            return;
        };
        let Some(entry) = self.entry(&spot.vm) else {
            self.overlay = None;
            return;
        };
        let facts = super::spot::facts(self, &entry);
        match cmd {
            Cmd::Move(step) => {
                if let Some(Overlay::Spot(spot)) = self.overlay.as_mut() {
                    spot.scroll.step(step, facts.len());
                }
            }
            Cmd::Sibling(by) => {
                let visible = self.visible();
                let at = visible
                    .iter()
                    .position(|other| other.name == entry.name)
                    .unwrap_or(0);
                let next = at
                    .saturating_add_signed(by)
                    .min(visible.len().saturating_sub(1));
                if let Some(next) = visible.get(next) {
                    let name = next.name.clone();
                    self.choose(&name);
                    self.overlay = Some(Overlay::Spot(Spot {
                        vm: name,
                        scroll: Scroll::default(),
                    }));
                }
            }
            Cmd::Copy => {
                let at = match &self.overlay {
                    Some(Overlay::Spot(spot)) => spot.scroll.cursor(facts.len()),
                    _ => 0,
                };
                if let Some((_, value)) = facts.get(at) {
                    let value = value.clone();
                    self.copy(value);
                }
            }
            Cmd::Back => self.overlay = None,
            _ => {}
        }
    }

    fn on_errors(&mut self, cmd: Cmd) {
        let count = self.errors.len();
        let Some(Overlay::Errors(scroll)) = self.overlay.as_mut() else {
            return;
        };
        match cmd {
            Cmd::Move(step) => {
                scroll.step(step, count);
            }
            Cmd::Copy => {
                let at = scroll.cursor(count);
                if let Some(entry) = self.errors.get(at) {
                    let text = format!("{}: {}", entry.what, entry.error);
                    self.copy(text);
                }
            }
            Cmd::Back => self.overlay = None,
            _ => {}
        }
    }

    fn on_confirm(&mut self, cmd: Cmd) {
        let Some(Dialog::Confirm(confirm)) = self.dialog.as_mut() else {
            return;
        };
        let yes = match cmd {
            Cmd::Move(step) => {
                let count = confirm.list.len();
                confirm.scroll.step(step, count);
                return;
            }
            Cmd::Toggle => {
                confirm.yes = !confirm.yes;
                return;
            }
            Cmd::Yes => true,
            Cmd::No | Cmd::Back => false,
            Cmd::Choose => confirm.yes,
            _ => return,
        };
        let Some(Dialog::Confirm(confirm)) = self.dialog.take() else {
            return;
        };
        if !yes {
            return;
        }
        match confirm.on_yes {
            OnYes::Quit => self.stop_and_quit(),
            OnYes::Run(actions) => {
                for action in actions {
                    self.start(action);
                }
            }
        }
    }

    fn on_form(&mut self, cmd: Cmd) {
        let Some(Dialog::Form(form)) = self.dialog.as_mut() else {
            return;
        };
        let last = form.fields.len().saturating_sub(1);
        match cmd {
            Cmd::NextField => {
                form.cursor = if form.cursor >= last {
                    0
                } else {
                    form.cursor + 1
                }
            }
            Cmd::PrevField => {
                form.cursor = if form.cursor == 0 {
                    last
                } else {
                    form.cursor - 1
                }
            }
            Cmd::Back => self.dialog = None,
            Cmd::Choose if !form.valid() => {
                // The first setting that is wrong, to fix.
                if let Some(at) = form.problems().iter().position(Option::is_some) {
                    form.cursor = at;
                }
                self.notify("Fix the settings marked in red first.", Tone::Warn);
            }
            Cmd::Choose => {
                let actions = form.actions();
                for field in &form.fields {
                    if let Some(group) = &field.input.group {
                        let value = field.input.value().trim().to_owned();
                        self.history.remember(group, &value);
                    }
                }
                self.dialog = None;
                if actions.is_empty() {
                    self.notify("Nothing changed.", Tone::Dim);
                }
                for action in actions {
                    self.start(action);
                }
            }
            _ => {}
        }
    }

    fn on_palette(&mut self, cmd: Cmd) {
        let Some(palette) = self.palette.as_mut() else {
            return;
        };
        match cmd {
            Cmd::Move(step) => {
                let count = palette.matches().len();
                palette.scroll.step(step, count);
                self.preview_theme();
            }
            Cmd::Back => {
                self.theme.palette = palette.theme_before;
                self.palette = None;
            }
            Cmd::Choose => {
                let run = palette.chosen().map(|item| item.run.clone());
                self.theme.palette = palette.theme_before;
                self.palette = None;
                if let Some(run) = run {
                    self.run(run);
                }
            }
            _ => {}
        }
    }

    /// Show the theme the palette's cursor is on, as a preview.
    fn preview_theme(&mut self) {
        let Some(palette) = &self.palette else {
            return;
        };
        let chosen = palette.chosen().and_then(|item| match item.run {
            Run::Theme(theme) => Some(theme),
            _ => None,
        });
        self.theme.palette = chosen.unwrap_or(palette.theme_before);
    }

    fn on_help(&mut self, cmd: Cmd) {
        let Some(help) = self.help.as_mut() else {
            return;
        };
        match cmd {
            Cmd::Move(step) => {
                let count = help.bindings().len();
                help.scroll.step(step, count);
            }
            Cmd::Back => self.help = None,
            Cmd::Choose => {
                let chosen = help.chosen().map(|binding| binding.cmd);
                self.help = None;
                if let Some(cmd) = chosen {
                    self.dispatch(cmd);
                }
            }
            _ => {}
        }
    }

    fn on_prompt(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Choose => {
                if let Some(prompt) = self.prompt.take() {
                    let value = prompt.input.value().to_owned();
                    if let Some(group) = &prompt.input.group {
                        self.history.remember(group, &value);
                    }
                    if value.is_empty() {
                        self.set_prompt(None);
                    }
                }
            }
            Cmd::Back => {
                if let Some(prompt) = self.prompt.take() {
                    self.set_prompt(prompt.before);
                }
            }
            _ => {}
        }
    }

    /// What is typed in the prompt, as it is typed.
    fn apply_prompt(&mut self) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let value = prompt.input.value().to_owned();
        self.set_prompt(Some(value).filter(|value| !value.is_empty()));
        if let (View::Logs(_), Some(_)) = (&self.view, &self.logs.find) {
            // The newest line with it.
            let lines = self.log_lines();
            if let Some(index) = lines.iter().rposition(|line| self.found(line)) {
                self.logs.scroll.cursor = index;
            }
        }
    }

    /// The filter or find of the view under the prompt.
    fn set_prompt(&mut self, value: Option<String>) {
        match self.view {
            View::Main => {
                self.list.filter = value;
                self.list.scroll = Scroll::default();
            }
            View::Doctor(_) => {
                self.doctor.filter = value;
                self.doctor.scroll = Scroll::default();
            }
            View::Logs(_) => self.logs.find = value,
            View::Details(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::actions::Action;
    use crate::tui::app::tests::app;
    use crate::tui::app::Pick;
    use crate::tui::jobs::Question;
    use crate::tui::jobs::{FlowEvent, Msg};
    use crate::tui::list::SortBy;
    use ratatui::crossterm::event::KeyModifiers;

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(app: &mut App, text: &str) {
        for character in text.chars() {
            press(app, KeyCode::Char(character));
        }
    }

    #[test]
    fn a_guest_of_another_build_can_be_upgraded_from_its_menu_and_the_list() {
        let (mut app, _rx) = app();
        app.context.build = "0.5.0 (build newer)".to_owned();
        let entry = app.selected().expect("a VM");
        assert!(app.update_available(&entry));
        press(&mut app, KeyCode::Enter);
        let which = app.which.as_ref().expect("a menu");
        let keys: String = which
            .cands
            .iter()
            .map(|cand| cand.keys[0].label())
            .collect();
        assert_eq!(keys, "vsolux");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('u'));
        let queued: Vec<&Action> = app.tasks.list.iter().map(|task| &task.action).collect();
        assert_eq!(queued, [&Action::Upgrade("vm0".to_owned())]);
    }

    #[test]
    fn enter_opens_a_menu_of_what_fits_the_vm_and_its_letters_do_it() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Enter);
        let which = app.which.as_ref().expect("a menu");
        let keys: String = which
            .cands
            .iter()
            .map(|cand| cand.keys[0].label())
            .collect();
        assert_eq!(keys, "vsolx");
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.view, View::Details("vm0".to_owned()));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view, View::Main);

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        let which = app.which.as_ref().expect("a menu");
        assert_eq!(
            which.cands[0].run,
            Run::Pick(Pick::Add(vec!["newvm".to_owned()]))
        );
        // Shift and the letter copies its command instead.
        press(&mut app, KeyCode::Char('A'));
        assert!(app.which.is_none());
        assert_eq!(
            app.effects,
            [Effect::Copy(
                "rampire --connect test:///default vm add newvm".to_owned()
            )]
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
        typed(&mut app, "gl");
        assert_eq!(app.view, View::Logs(None));
        press(&mut app, KeyCode::Esc);
        // The form edits the draft: a VM whose file can't be read gets
        // the reason, not an empty form.
        let draft = crate::context::TempDir::new("tui-settings").unwrap();
        app.context.config = Some(draft.path().to_path_buf());
        press(&mut app, KeyCode::Char('s'));
        assert!(app.dialog.is_none());
        let vms = draft.path().join(rampire_core::settings::VMS_DIRECTORY);
        std::fs::create_dir_all(&vms).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/config/linux-guest.toml"),
            vms.join("vm0.toml"),
        )
        .unwrap();
        press(&mut app, KeyCode::Char('s'));
        let Some(Dialog::Form(form)) = &app.dialog else {
            panic!("the settings form opens");
        };
        // The draft's values, not the ones the reader last saw running.
        assert_eq!(form.fields[0].input.value(), "50");
        assert_eq!(form.fields[2].input.value(), "5GB");
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn a_half_added_vm_can_be_finished_or_undone_from_its_menu() {
        let (mut app, _rx) = app();
        let oldvm = app.entry("oldvm").expect("the half-added VM");
        let keys: String = app.menu(&oldvm).1.iter().map(|choice| choice.key).collect();
        assert_eq!(keys, "aox");
        app.choose("oldvm");
        press(&mut app, KeyCode::Char('x'));
        let queued: Vec<&Action> = app.tasks.list.iter().map(|task| &task.action).collect();
        assert_eq!(queued, [&Action::Remove("oldvm".to_owned())]);
    }

    #[test]
    fn a_vm_removed_from_its_details_goes_back_to_the_list() {
        let (mut app, _rx) = app();
        app.open(View::Details("vm0".to_owned()));
        press(&mut app, KeyCode::Char('x'));
        let id = app.tasks.list[0].id;
        app.tasks.start_next(Instant::now());
        let remove = Action::Remove("vm0".to_owned());
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Done(Ok(remove.ended(crate::flows::Outcome::Done))),
        ));
        assert_eq!(app.view, View::Main);
    }

    #[test]
    fn marked_vms_are_upgraded_each_not_the_hovered_one_alone() {
        let (mut app, _rx) = app();
        app.context.build = "0.5.0 (build newer)".to_owned();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('u'));
        assert!(
            matches!(&app.dialog, Some(Dialog::Confirm(confirm)) if !confirm.list.is_empty()),
            "one question for the marked VMs, not the hovered one alone"
        );
    }

    #[test]
    fn a_vms_menu_offers_a_restart_a_recovery_or_no_add_as_its_state_needs() {
        let (mut app, _rx) = app();
        let Loaded::Done(Ok(report)) = &mut app.inventory else {
            panic!("the fixture's list");
        };
        let fixture = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/json/status-report.json"),
        )
        .unwrap();
        let mut status: rampire_core::ControllerStatusSnapshot = serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(&fixture).unwrap()["vms"][0].clone(),
        )
        .unwrap();
        status.actuation_latched = true;
        for vm in &mut report.vms {
            match vm.vm_name.as_str() {
                "vm0" => {
                    vm.runs_behind = true;
                    vm.status = Some(status.clone());
                }
                "newvm" => vm.problems.push(crate::reports::Problem {
                    problem: "it runs Home Assistant OS, which has no dnf or apt".to_owned(),
                    fix: "none yet".to_owned(),
                }),
                _ => {}
            }
        }
        let keys = |app: &App, vm: &str| -> String {
            let entry = app.entry(vm).unwrap();
            app.menu(&entry).1.iter().map(|choice| choice.key).collect()
        };
        assert_eq!(keys(&app, "vm0"), "vsolbcx");
        // Adding would only fail, so it isn't offered; the problem is said.
        assert_eq!(keys(&app, "newvm"), "o");
        let newvm = app.entry("newvm").unwrap();
        assert!(app
            .menu(&newvm)
            .0
            .iter()
            .any(|line| line.contains("Home Assistant")));
        press(&mut app, KeyCode::Char('b'));
        press(&mut app, KeyCode::Char('c'));
        let queued: Vec<&Action> = app.tasks.list.iter().map(|task| &task.action).collect();
        assert_eq!(
            queued,
            [
                &Action::Restart("vm0".to_owned()),
                &Action::Recover("vm0".to_owned())
            ]
        );
    }

    #[test]
    fn quitting_stops_the_tasks_at_a_safe_point_first() {
        let (mut app, _rx) = app();
        let running = app.tasks.add(Action::Apply, "rampire apply".to_owned());
        app.tasks.start_next(Instant::now());
        let waiting = app.tasks.add(Action::Diff, "rampire diff".to_owned());
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('y'));
        assert!(!app.quit, "not while a task runs");
        assert!(app.tasks.get(running).unwrap().cancel.is_set());
        assert!(app.tasks.get(waiting).unwrap().state.finished());
        app.on_msg(Msg::Task(
            running,
            FlowEvent::Done(Err("cancelled while waiting for vm0 to boot".to_owned())),
        ));
        assert!(app.quit, "once it has stopped");
    }

    #[test]
    fn the_diff_shows_at_once_and_rollback_is_asked_by_its_flow() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(app.layer(), Layer::Task);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('B'));
        let queued: Vec<&Action> = app.tasks.list.iter().map(|task| &task.action).collect();
        assert_eq!(queued, [&Action::Diff, &Action::Rollback]);
    }

    #[test]
    fn a_doctor_finding_for_one_vm_opens_its_doctor() {
        let (mut app, _rx) = app();
        app.open(View::Doctor(None));
        app.on_msg(Msg::Doctor(Ok(vec![crate::tui::jobs::DoctorSection {
            vm: None,
            findings: vec![crate::doctor::Finding::problem(
                "vm vm0",
                "latch: resizing stopped",
                "rampire doctor vm0",
            )
            .runs(&["doctor", "vm0"])],
        }])));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.view, View::Doctor(Some("vm0".to_owned())));
    }

    #[test]
    fn a_draft_saved_but_not_applied_can_be_applied_from_anywhere() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('A'));
        let queued: Vec<&Action> = app.tasks.list.iter().map(|task| &task.action).collect();
        assert_eq!(queued, [&Action::Apply]);
    }

    #[test]
    fn a_task_that_was_declined_or_left_part_done_is_not_reported_done() {
        use crate::flows::Outcome;
        use crate::tui::jobs::Ended;
        let remove = Action::Remove("vm0".to_owned());
        assert_eq!(
            remove.ended(Outcome::Done),
            Ended::Done("RAMpire is removed from vm0".to_owned())
        );
        let Ended::NotDone(text) = remove.ended(Outcome::declined()) else {
            panic!("No is not done");
        };
        assert!(
            text.contains("not removed") && text.contains("answer was No"),
            "{text}"
        );
        let set = Action::Set {
            target: "vm0".to_owned(),
            changes: vec!["priority=150".to_owned()],
        };
        assert!(matches!(
            set.ended(Outcome::Partly("saved, not applied".to_owned())),
            Ended::NotDone(_)
        ));

        // The notice says what happened, not only which command ran.
        let (mut app, _rx) = app();
        let id = app
            .tasks
            .add(remove.clone(), "rampire vm remove vm0".to_owned());
        app.tasks.start_next(Instant::now());
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Done(Ok(remove.ended(Outcome::declined()))),
        ));
        assert!(app.errors.is_empty());
        let shown = format!("{:?}", app.notices);
        assert!(
            shown.contains("Not done") && shown.contains("not removed"),
            "{shown}"
        );
    }

    #[test]
    fn chords_sort_and_change_the_columns() {
        let (mut app, _rx) = app();
        typed(&mut app, ",n");
        assert_eq!(app.list.sort, SortBy::Name);
        assert!(app.which.is_none());
        typed(&mut app, "ml");
        assert_eq!(app.list.linemode, crate::tui::list::Linemode::Limits);
        typed(&mut app, "G");
        assert_eq!(app.selected().unwrap().name, "vm2");
        typed(&mut app, "gg");
        assert_eq!(app.selected().unwrap().name, "a-very-long-vm-name");
        // A chord's popup closes on a key that isn't in it.
        typed(&mut app, ",");
        assert!(app.which.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.which.is_none());
    }

    #[test]
    fn the_filter_narrows_the_list_as_it_is_typed_and_esc_clears_it() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.layer(), Layer::Prompt);
        typed(&mut app, "old");
        assert_eq!(app.visible().len(), 1);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.layer(), Layer::List);
        assert_eq!(app.list.filter.as_deref(), Some("old"));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.list.filter, None);
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "zz");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.list.filter, None, "Esc puts back what was there");
    }

    #[test]
    fn marked_vms_get_one_question_and_a_task_each() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.list.marks.len(), 2);
        press(&mut app, KeyCode::Char('x'));
        let Some(Dialog::Confirm(confirm)) = &app.dialog else {
            panic!("one question for the marked VMs");
        };
        assert_eq!(confirm.list.len(), 2);
        assert!(
            confirm.list[0].contains("vm remove vm0"),
            "{:?}",
            confirm.list
        );
        // No is the default.
        press(&mut app, KeyCode::Enter);
        assert!(app.dialog.is_none());
        assert!(app.tasks.list.is_empty());
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.tasks.list.len(), 2);
        assert_eq!(app.tasks.active(), 2);
    }

    #[test]
    fn a_task_answered_stays_open_and_says_it_is_working() {
        let (mut app, _rx) = app();
        let id = app
            .tasks
            .add(Action::add("newvm"), "rampire vm add newvm".to_owned());
        app.tasks.start_next(Instant::now());
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Shown("Adding newvm to RAMpire".to_owned()),
        ));
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Review("Add newvm?".to_owned()), reply),
        ));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(answer.recv().unwrap(), Answer::Yes);
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Line("logging in to newvm to install rampire-guest".to_owned()),
        ));
        assert_eq!(app.layer(), Layer::Task, "the window stays open");
        let area = ratatui::layout::Rect::new(0, 0, 120, 30);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        crate::tui::views::render(area, &mut buf, &app);
        let text = crate::tui::widgets::plain_text(&buf);
        assert!(
            text.contains("Working: logging in to newvm to install rampire-guest"),
            "{text}"
        );
        assert!(!text.contains("y  yes"), "no buttons to press:\n{text}");
        assert!(text.contains("x  cancel"), "{text}");
    }

    #[test]
    fn a_tasks_question_comes_forward_and_is_answered_there() {
        let (mut app, _rx) = app();
        let id = app.tasks.add(Action::Apply, "rampire apply".to_owned());
        app.tasks.start_next(Instant::now());
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Confirm("Apply this?".to_owned()), reply),
        ));
        assert_eq!(app.layer(), Layer::Task);
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            answer.recv().unwrap(),
            Answer::Cancelled,
            "x cancels at the question"
        );
        // Cancelled, it stays so: a question it asks after is refused too.
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Confirm("Apply this?".to_owned()), reply),
        ));
        assert_eq!(
            answer
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
            Answer::Cancelled
        );
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Done(Ok(crate::tui::jobs::Ended::NotDone("cancelled".to_owned()))),
        ));
        app.overlay = None;
        let id = app.tasks.add(Action::Apply, "rampire apply".to_owned());
        app.tasks.start_next(Instant::now());
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Confirm("Apply this?".to_owned()), reply),
        ));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert_eq!(answer.recv().unwrap(), Answer::Yes);
        // A text question takes letters, y and n among them.
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Text("Why?".to_owned()), reply),
        ));
        typed(&mut app, "only now");
        press(&mut app, KeyCode::Enter);
        assert_eq!(answer.recv().unwrap(), Answer::Text("only now".to_owned()));
        assert_eq!(app.history.get("Why?"), ["only now"]);
        // Closed, a question waits, and the task goes on.
        let (reply, answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(Question::Confirm("Again?".to_owned()), reply),
        ));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.layer(), Layer::List);
        assert!(answer.try_recv().is_err());
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.layer(), Layer::Task);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(answer.recv().unwrap(), Answer::No);
        app.on_msg(Msg::Task(id, FlowEvent::Done(Err("it broke".to_owned()))));
        assert_eq!(app.errors.len(), 1);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('e'));
        assert!(matches!(app.overlay, Some(Overlay::Errors(_))));
    }

    #[test]
    fn quitting_asks_while_tasks_run_and_ctrl_c_twice_quits() {
        let (mut app, _rx) = app();
        app.tasks.add(Action::Apply, "rampire apply".to_owned());
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.quit);
        assert_eq!(app.layer(), Layer::Confirm);
        press(&mut app, KeyCode::Char('n'));
        assert!(!app.quit);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!app.quit);
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.quit);
    }

    #[test]
    fn the_palette_finds_commands_by_a_few_letters_and_runs_them() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char(':'));
        let palette = app.palette.as_ref().expect("the palette opens");
        assert!(palette.matches().len() > 10);
        typed(&mut app, "doct");
        let first = app.palette.as_ref().unwrap().matches()[0].clone();
        assert_eq!(first.label, "Doctor: check everything about it");
        assert_eq!(first.group, "vm0");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.palette.is_none());
        assert!(matches!(app.view, View::Doctor(_)));

        // Moving onto a theme previews it; Esc puts the old one back.
        press(&mut app, KeyCode::Esc);
        app.on_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        typed(&mut app, "light");
        assert_eq!(app.theme.palette, crate::tui::theme::Palette::Light);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.theme.palette, crate::tui::theme::Palette::Midnight);
    }

    #[test]
    fn the_help_finds_a_key_and_enter_does_it() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.layer(), Layer::Help);
        typed(&mut app, "doctor, for");
        press(&mut app, KeyCode::Enter);
        assert!(app.help.is_none());
        assert_eq!(app.view, View::Doctor(None));
    }

    #[test]
    fn spot_shows_a_vms_facts_and_copies_one() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.layer(), Layer::Spot);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.effects, [Effect::Copy("vm0".to_owned())]);
        press(&mut app, KeyCode::Right);
        let Some(Overlay::Spot(spot)) = &app.overlay else {
            panic!("still open");
        };
        assert_eq!(spot.vm, "a-very-long-vm-name");
        assert_eq!(app.selected().unwrap().name, "a-very-long-vm-name");
        press(&mut app, KeyCode::Esc);
        assert!(app.overlay.is_none());
    }

    #[test]
    fn the_logs_find_a_text_and_go_between_its_lines() {
        let (mut app, _rx) = app();
        app.open(View::Logs(None));
        let text: String = (0..20)
            .map(|index| {
                format!(
                    "2026-11-14T10:00:{index:02}+0000 pc101 rampire[1]: rampire: vm=vm{} event=x\n",
                    index % 4
                )
            })
            .collect();
        app.on_msg(Msg::Log(Ok(text)));
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "vm=vm3");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.logs.scroll.cursor, 19, "the newest line with it");
        press(&mut app, KeyCode::Char('N'));
        assert_eq!(app.logs.scroll.cursor, 15);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.logs.scroll.cursor, 19);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.logs.scroll.cursor, 3, "round the end");
        press(&mut app, KeyCode::Char('y'));
        assert!(
            matches!(app.effects.last(), Some(Effect::Copy(line)) if line.contains("10:00:03"))
        );
    }

    #[test]
    fn ctrl_z_suspends_and_f1_opens_the_help_anywhere() {
        let (mut app, _rx) = app();
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(app.effects, [Effect::Suspend]);
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::F(1));
        assert_eq!(app.help.as_ref().map(|help| help.layer), Some(Layer::Tasks));
    }
}
