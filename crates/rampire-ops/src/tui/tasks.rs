//! Actions run as tasks, in the background, so the screen stays free: one
//! at a time in the order they were started, since each changes the host's
//! configuration; the others wait their turn. A task's question comes
//! forward when nothing else is open, and waits if it is closed. The status
//! bar shows how far the tasks have got, `w` lists them, and Enter shows
//! one's output. The layout of the list and of the status bar's gauge
//! follows Yazi's task manager.

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::actions::Action;
use super::input::{History, Input};
use super::jobs::{Answer, Question, TaskId};
use super::scroll::{Pager, Scroll};
use super::theme::{Icon, Theme};
use super::widgets::{self, raised_panel};
use crate::tone::Tone;

/// Where a task has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Started after another that is still running.
    Waiting,
    Running,
    Done(String),
    Failed(String),
    /// Taken off the queue before it ran.
    Cancelled,
}

impl State {
    pub fn finished(&self) -> bool {
        matches!(self, Self::Done(_) | Self::Failed(_) | Self::Cancelled)
    }
}

/// A question a task waits on.
pub struct Pending {
    pub question: Question,
    pub reply: Sender<Answer>,
    /// What has been typed, for a text question.
    pub input: Input,
    /// For a yes-or-no question: the Yes button is chosen. No is the
    /// default.
    pub yes: bool,
    /// Closed to answer later: it doesn't come forward again by itself.
    pub later: bool,
}

pub struct Task {
    pub id: TaskId,
    pub action: Action,
    /// The command line that does the same.
    pub title: String,
    pub lines: Vec<(String, Tone)>,
    pub pending: Option<Pending>,
    pub state: State,
    /// Cancelled while running: its next question is answered with a stop.
    pub cancel: bool,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    pub pager: Pager,
}

impl Task {
    /// A line it said, toned by what it says.
    pub fn say(&mut self, line: String) {
        let tone = line_tone(&line);
        self.lines.push((line, tone));
    }

    /// A change it showed, such as a diff: additions good, removals bad.
    pub fn show(&mut self, text: &str) {
        for line in text.replace('\t', "    ").lines() {
            let tone = if line.starts_with('+') && !line.starts_with("+++") {
                Tone::Good
            } else if line.starts_with('-') && !line.starts_with("---") {
                Tone::Bad
            } else {
                Tone::Dim
            };
            self.lines.push((line.to_owned(), tone));
        }
    }

    /// How long it ran, or has run.
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started
            .map(|started| self.ended.unwrap_or(now).saturating_duration_since(started))
    }

    fn status(&self, now: Instant) -> (String, Tone) {
        let seconds = self.elapsed(now).map_or(0, |elapsed| elapsed.as_secs());
        match (&self.state, &self.pending) {
            (State::Running, Some(pending)) => (
                format!("Asks: {} (Enter to answer)", pending.question.text()),
                Tone::Accent,
            ),
            (State::Running, None) if self.cancel => (
                format!("Running for {seconds}s; it stops at its next question"),
                Tone::Warn,
            ),
            (State::Running, None) => (format!("Running for {seconds}s"), Tone::Info),
            (State::Waiting, _) => ("Waiting for the task before it".to_owned(), Tone::Dim),
            (State::Done(result), _) => (format!("Done in {seconds}s: {result}"), Tone::Good),
            (State::Failed(error), _) => (
                format!("Failed after {seconds}s: {error} (Enter for its output)"),
                Tone::Bad,
            ),
            (State::Cancelled, _) => ("Cancelled before it ran".to_owned(), Tone::Dim),
        }
    }
}

/// What cancelling a task did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    /// It hadn't started: it won't.
    Dropped,
    /// Its question was answered with a stop.
    Stopped,
    /// It is running between questions: it stops at its next one.
    AtNextQuestion,
    /// It had finished.
    Finished,
}

/// How the tasks are doing, for the status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Started since the tasks were last all finished.
    pub total: usize,
    pub finished: usize,
    pub failed: usize,
    pub asking: usize,
}

#[derive(Default)]
pub struct Tasks {
    pub list: Vec<Task>,
    last_id: TaskId,
    pub scroll: Scroll,
    /// The tasks started since all were last finished, and how they went.
    batch: Summary,
}

impl Tasks {
    /// Queue `action`; it runs when the tasks before it have finished.
    pub fn add(&mut self, action: Action, title: String) -> TaskId {
        if self.active() == 0 {
            self.batch = Summary::default();
        }
        self.last_id += 1;
        self.batch.total += 1;
        self.list.push(Task {
            id: self.last_id,
            action,
            lines: vec![(format!("$ {title}"), Tone::Dim)],
            title,
            pending: None,
            state: State::Waiting,
            cancel: false,
            started: None,
            ended: None,
            pager: Pager::default(),
        });
        self.last_id
    }

    pub fn get(&self, id: TaskId) -> Option<&Task> {
        self.list.iter().find(|task| task.id == id)
    }

    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.list.iter_mut().find(|task| task.id == id)
    }

    /// Running or waiting.
    pub fn active(&self) -> usize {
        self.list
            .iter()
            .filter(|task| !task.state.finished())
            .count()
    }

    /// The next task to start, now running, when none is.
    pub fn start_next(&mut self, now: Instant) -> Option<(TaskId, Action)> {
        if self.list.iter().any(|task| task.state == State::Running) {
            return None;
        }
        let task = self
            .list
            .iter_mut()
            .find(|task| task.state == State::Waiting)?;
        task.state = State::Running;
        task.started = Some(now);
        Some((task.id, task.action.clone()))
    }

    /// Task `id` ended with `result`.
    pub fn finish(&mut self, id: TaskId, result: Result<String, String>, now: Instant) {
        let Some(task) = self.get_mut(id) else {
            return;
        };
        task.pending = None;
        task.ended = Some(now);
        let failed = result.is_err();
        task.state = match result {
            Ok(done) => State::Done(done),
            Err(error) => State::Failed(error),
        };
        self.batch.finished += 1;
        if failed {
            self.batch.failed += 1;
        }
    }

    pub fn cancel(&mut self, id: TaskId, now: Instant) -> Cancelled {
        let Some(task) = self.get_mut(id) else {
            return Cancelled::Finished;
        };
        match task.state {
            State::Waiting => {
                task.state = State::Cancelled;
                task.ended = Some(now);
                self.batch.finished += 1;
                Cancelled::Dropped
            }
            State::Running => match task.pending.take() {
                Some(pending) => {
                    task.lines.push((
                        format!("{} (cancelled)", pending.question.text()),
                        Tone::Warn,
                    ));
                    // A flow that has gone can't be told; it has stopped.
                    let _ = pending.reply.send(Answer::Cancelled);
                    Cancelled::Stopped
                }
                None => {
                    task.cancel = true;
                    Cancelled::AtNextQuestion
                }
            },
            _ => Cancelled::Finished,
        }
    }

    /// The first task whose question should come forward.
    pub fn asking(&self) -> Option<TaskId> {
        self.list
            .iter()
            .find(|task| task.pending.as_ref().is_some_and(|pending| !pending.later))
            .map(|task| task.id)
    }

    pub fn summary(&self) -> Summary {
        Summary {
            asking: self
                .list
                .iter()
                .filter(|task| task.pending.is_some())
                .count(),
            ..self.batch
        }
    }

    pub fn clear_finished(&mut self) {
        self.list.retain(|task| !task.state.finished());
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

/// The status bar's gauge while tasks run: how many of those started have
/// finished, and how many are left; red once one has failed.
pub fn gauge(theme: &Theme, tasks: &Tasks, width: usize) -> Option<Line<'static>> {
    let summary = tasks.summary();
    if tasks.active() == 0 {
        return None;
    }
    let left = summary.total.saturating_sub(summary.finished);
    let label = if summary.asking > 0 {
        format!(
            " {} {} asks (w) ",
            theme.icon(Icon::Pointer),
            summary.asking
        )
    } else {
        format!(" {left} left ")
    };
    let fraction = summary.finished as f64 / summary.total.max(1) as f64;
    let tone = if summary.failed > 0 {
        Tone::Bad
    } else {
        Tone::Info
    };
    let cells = width.saturating_sub(label.chars().count()).clamp(4, 12);
    let mut spans = widgets::bar(theme, fraction, cells, tone);
    spans.insert(0, Span::styled(" ", theme.bar()));
    spans.push(Span::styled(
        label,
        theme.bar().patch(theme.tone(if summary.asking > 0 {
            Tone::Accent
        } else {
            tone
        })),
    ));
    Some(Line::from(spans))
}

/// The tasks list: each task's command line, then where it has got to,
/// with a rail beside the chosen one.
pub fn render_list(area: Rect, buf: &mut Buffer, theme: &Theme, tasks: &Tasks, now: Instant) {
    let rect = widgets::centred(
        area,
        (area.width * 4 / 5).max(60),
        (area.height * 4 / 5).max(10),
    );
    Clear.render(rect, buf);
    let block = raised_panel(
        theme,
        Icon::Running,
        "TASKS",
        &format!(
            "{} running or waiting, one at a time; {} finished",
            tasks.active(),
            tasks.list.len() - tasks.active()
        ),
    );
    let inner = block.inner(rect);
    block.render(rect, buf);
    if tasks.list.is_empty() {
        Paragraph::new(widgets::line(
            theme,
            "no tasks yet: what is done from a VM's menu runs here",
            Tone::Dim,
        ))
        .render(inner, buf);
        return;
    }
    // Three rows a task: its command line, its state, a gap.
    let per_page = (inner.height as usize / 3).max(1);
    let window = tasks.scroll.window(tasks.list.len(), per_page);
    let cursor = tasks.scroll.cursor(tasks.list.len());
    for (row, index) in window.enumerate() {
        let Some(task) = tasks.list.get(index) else {
            continue;
        };
        let y = inner.y + row as u16 * 3;
        if y + 1 >= inner.bottom() {
            break;
        }
        let chosen = index == cursor;
        let (status, tone) = task.status(now);
        let icon = match task.state {
            State::Done(_) => Icon::Ok,
            State::Failed(_) => Icon::Problem,
            State::Waiting | State::Cancelled => Icon::Stopped,
            State::Running => Icon::Running,
        };
        let rail = if chosen {
            Span::styled(
                if theme.icons.unicode { "┃ " } else { "| " },
                theme.accent(),
            )
        } else {
            Span::styled("  ", theme.raised())
        };
        let title_style = if chosen {
            theme.selected()
        } else {
            theme.value()
        };
        let width = inner.width.saturating_sub(4) as usize;
        let lines = [
            Line::from(vec![
                rail.clone(),
                Span::styled(format!("{} ", theme.icon(icon)), theme.tone(tone)),
                Span::styled(widgets::fit(&task.title, width), title_style),
            ]),
            Line::from(vec![
                rail,
                Span::styled("  ", theme.raised()),
                Span::styled(widgets::fit(&status, width), theme.tone(tone)),
            ]),
        ];
        for (offset, line) in lines.into_iter().enumerate() {
            let line_area = Rect {
                y: y + offset as u16,
                height: 1,
                ..inner
            };
            Paragraph::new(line)
                .style(theme.raised())
                .render(line_area, buf);
        }
    }
}

/// One task: what it said, newest at the bottom, its question with its
/// buttons or its text field, and how it ended.
pub fn render_task(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    history: &History,
    task: &Task,
    now: Instant,
) {
    let rect = widgets::centred(
        area,
        area.width.saturating_sub(4).min(120),
        area.height.saturating_sub(2),
    );
    Clear.render(rect, buf);
    let (status, _) = task.status(now);
    let detail = match task.state {
        State::Running if task.pending.is_some() => "asks".to_owned(),
        _ => status.split(':').next().unwrap_or("").to_owned(),
    };
    let block = raised_panel(theme, Icon::Settings, &task.title, &detail);
    let inner = block.inner(rect);
    block.render(rect, buf);
    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = task
        .lines
        .iter()
        .map(|(text, tone)| widgets::line(theme, text.clone(), *tone))
        .collect();
    match (&task.pending, &task.state) {
        (Some(pending), _) => {
            let question = match pending.question {
                Question::Confirm(_) | Question::Review(_) => {
                    format!("{} [y/N]", pending.question.text())
                }
                Question::Text(_) => pending.question.text().to_owned(),
            };
            lines.push(Line::from(widgets::marked(
                theme,
                Icon::Pointer,
                question,
                Tone::Accent,
            )));
        }
        (None, State::Done(result)) => lines.push(Line::from(widgets::marked(
            theme,
            Icon::Ok,
            result.clone(),
            Tone::Good,
        ))),
        (None, State::Failed(error)) => {
            lines.push(Line::from(widgets::marked(
                theme,
                Icon::Problem,
                error.clone(),
                Tone::Bad,
            )));
            lines.push(widgets::line(
                theme,
                "kept in the errors list (e); the journal has the change log: journalctl -t rampire",
                Tone::Dim,
            ));
        }
        (None, State::Waiting) => lines.push(widgets::line(
            theme,
            "Waiting for the task before it to finish.",
            Tone::Dim,
        )),
        _ => {}
    }
    let rows: Vec<Line<'static>> = lines
        .into_iter()
        .flat_map(|line| widgets::wrap(line, width))
        .collect();
    // The question's answer area under the text: the buttons, or the field.
    let answer = match &task.pending {
        Some(pending) => match pending.question {
            Question::Text(_) => Some(Line::from(
                std::iter::once(Span::styled(
                    format!("{} ", theme.icon(Icon::Pointer)),
                    theme.accent(),
                ))
                .chain(
                    pending
                        .input
                        .line(theme, history, width.saturating_sub(2))
                        .spans,
                )
                .collect::<Vec<_>>(),
            )),
            _ => Some(widgets::buttons(theme, inner.width, pending.yes)),
        },
        None => None,
    };
    let [text_area, _, answer_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(u16::from(answer.is_some())),
        Constraint::Length(u16::from(answer.is_some())),
    ])
    .areas(inner);
    let window = task.pager.window(rows.len(), text_area.height as usize);
    Paragraph::new(rows[window].to_vec())
        .style(theme.raised())
        .render(text_area, buf);
    if let Some(answer) = answer {
        Paragraph::new(answer)
            .style(theme.raised())
            .render(answer_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks() -> Tasks {
        let mut tasks = Tasks::default();
        tasks.add(Action::Apply, "rampire apply".to_owned());
        tasks.add(
            Action::Attest {
                vm: "web".to_owned(),
                restart: false,
            },
            "rampire vm attest web".to_owned(),
        );
        tasks
    }

    #[test]
    fn tasks_run_one_at_a_time_in_the_order_started() {
        let mut tasks = tasks();
        let now = Instant::now();
        let (first, action) = tasks.start_next(now).expect("the first starts");
        assert_eq!(action, Action::Apply);
        assert_eq!(tasks.start_next(now), None, "one at a time");
        assert_eq!(tasks.summary().total, 2);
        tasks.finish(first, Ok("done".to_owned()), now);
        let (second, _) = tasks.start_next(now).expect("then the next");
        assert_eq!(second, 2);
        tasks.finish(second, Err("it broke".to_owned()), now);
        let summary = tasks.summary();
        assert_eq!((summary.finished, summary.failed), (2, 1));
        assert_eq!(tasks.active(), 0);
        // A new task starts a new count.
        tasks.add(Action::Apply, "rampire apply".to_owned());
        assert_eq!(tasks.summary().total, 1);
        tasks.clear_finished();
        assert_eq!(tasks.list.len(), 1);
    }

    #[test]
    fn cancelling_drops_a_waiting_task_and_stops_a_running_one_at_its_question() {
        let mut tasks = tasks();
        let now = Instant::now();
        let (first, _) = tasks.start_next(now).unwrap();
        assert_eq!(tasks.cancel(2, now), Cancelled::Dropped);
        assert_eq!(tasks.cancel(first, now), Cancelled::AtNextQuestion);
        assert!(tasks.get(first).unwrap().cancel);
        let (reply, answer) = std::sync::mpsc::channel();
        tasks.get_mut(first).unwrap().pending = Some(Pending {
            question: Question::Confirm("Apply this?".to_owned()),
            reply,
            input: Input::default(),
            yes: false,
            later: false,
        });
        assert_eq!(tasks.asking(), Some(first));
        assert_eq!(tasks.cancel(first, now), Cancelled::Stopped);
        assert_eq!(answer.recv().unwrap(), Answer::Cancelled);
        assert_eq!(tasks.asking(), None);
    }

    #[test]
    fn a_task_and_the_list_draw_with_their_state() {
        let theme = Theme::new(
            super::super::theme::Palette::Terminal,
            super::super::theme::Depth::Colours16,
            super::super::theme::Icons { unicode: true },
        );
        let mut tasks = tasks();
        let now = Instant::now();
        let (first, _) = tasks.start_next(now).unwrap();
        tasks.get_mut(first).unwrap().say("checking".to_owned());
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render_list(area, &mut buf, &theme, &tasks, now);
        let text = widgets::plain_text(&buf);
        assert!(
            text.contains("rampire apply") && text.contains("Running"),
            "{text}"
        );
        assert!(text.contains("Waiting for the task before it"), "{text}");
        let mut buf = Buffer::empty(area);
        render_task(
            area,
            &mut buf,
            &theme,
            &History::default(),
            tasks.get(first).unwrap(),
            now,
        );
        let text = widgets::plain_text(&buf);
        assert!(text.contains("checking"), "{text}");
        let gauge = gauge(&theme, &tasks, 20).unwrap().to_string();
        assert!(gauge.contains("2 left"), "{gauge}");
    }
}
