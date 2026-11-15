//! `rampire top` and `rampire watch`: the TUI's dashboard or a VM's details
//! alone, refreshed until `q`, Esc or Ctrl+C, read-only; Ctrl+Z goes back
//! to the shell until `fg`. With `--once`, or
//! when stdout isn't a terminal, one plain snapshot is printed instead; on
//! leaving, the last view is left on the normal screen as plain text to
//! copy.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::theme::{Depth, Icon, Theme};
use crate::top::{PoolReader, PoolSnapshot};
use crate::watch::{VmReader, VmSnapshot};

/// `rampire top`.
pub fn top(command: crate::top::Command, cwd: &Path) -> Result<(), String> {
    let (once, interval) = (command.once, command.interval);
    let mut reader = PoolReader::new(command);
    run(
        once,
        interval,
        140,
        || reader.read(cwd),
        |snapshot: &PoolSnapshot| super::pool::fixed_height(snapshot) + 10,
        |area, buf, theme, snapshot| {
            let [header, body] = header_layout(area);
            bar(
                header,
                buf,
                theme,
                &format!(
                    "{} RAMPIRE POOL  {}  {} VMs   refresh {:.1}s  samples {}",
                    theme.icon(Icon::Pool),
                    crate::dashboard::host_name(),
                    snapshot.command.vms.len(),
                    snapshot.command.interval.as_secs_f64(),
                    snapshot.samples
                ),
            );
            super::pool::render(body, buf, theme, snapshot, None);
        },
        |_| String::new(),
    )
}

/// `rampire watch`, with the VM's settings, boot RAM and state as the
/// TUI's details show them.
pub fn watch(
    context: &crate::context::Context,
    command: crate::watch::Command,
    cwd: &Path,
) -> Result<(), String> {
    let (once, interval) = (command.once, command.interval);
    let details = super::jobs::details(context, &command.vm);
    let mut reader = VmReader::new(command);
    run(
        once,
        interval,
        120,
        || reader.read(cwd),
        |snapshot: &VmSnapshot| super::vm::fixed_height(snapshot) + 8,
        |area, buf, theme, snapshot| {
            let [header, body] = header_layout(area);
            bar(
                header,
                buf,
                theme,
                &format!(
                    "{} RAMPIRE VM  {}  {}   refresh {:.1}s  guest every {:.1}s  samples {}",
                    theme.icon(Icon::Vm),
                    crate::dashboard::host_name(),
                    snapshot.command.vm,
                    snapshot.command.interval.as_secs_f64(),
                    snapshot.command.guest_interval.as_secs_f64(),
                    snapshot.samples
                ),
            );
            super::vm::render(body, buf, theme, snapshot, Some(&details));
        },
        |snapshot| crate::watch::full_events(&snapshot.journal.events),
    )
}

fn header_layout(area: Rect) -> [Rect; 2] {
    Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area)
}

fn bar(area: Rect, buf: &mut Buffer, theme: &Theme, left: &str) {
    buf.set_style(area, theme.bar());
    Paragraph::new(Line::from(vec![
        Span::styled(format!(" {left}"), theme.bar()),
        Span::styled("   q quit", theme.bar()),
    ]))
    .style(theme.bar())
    .render(area, buf);
    let clock = format!("{} ", crate::dashboard::clock());
    let width = clock.len() as u16;
    if area.width > width + 60 {
        buf.set_string(area.right() - width, area.y, clock, theme.bar());
    }
}

/// A plain snapshot of `snapshot`: `width` wide and as tall as it needs,
/// without colour.
fn plain<S>(
    snapshot: &S,
    width: u16,
    height: impl Fn(&S) -> u16,
    draw: &impl Fn(Rect, &mut Buffer, &Theme, &S),
) -> String {
    let mut theme = super::theme_from_environment();
    theme.depth = Depth::None;
    let area = Rect::new(0, 0, width, height(snapshot) + 1);
    let mut buf = Buffer::empty(area);
    draw(area, &mut buf, &theme, snapshot);
    super::widgets::plain_text(&buf)
}

/// Refresh a view until quit, or print one plain snapshot.
fn run<S>(
    once: bool,
    interval: Duration,
    max_width: u16,
    mut read: impl FnMut() -> Result<S, String>,
    height: impl Fn(&S) -> u16,
    draw: impl Fn(Rect, &mut Buffer, &Theme, &S),
    after: impl Fn(&S) -> String,
) -> Result<(), String> {
    let interactive = !once && std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    if !interactive {
        let width = ratatui::crossterm::terminal::size()
            .map_or(100, |(columns, _)| columns.saturating_sub(1))
            .clamp(80, max_width);
        let snapshot = read()?;
        print!(
            "{}{}",
            plain(&snapshot, width, &height, &draw),
            after(&snapshot)
        );
        return Ok(());
    }
    let stop = super::stop_on_signals(&[
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ])?;
    let theme = super::theme_for_screen();
    let mut screen = super::term::Term::open()?;
    let mut last: Option<S> = None;
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Acquire) {
            let started = Instant::now();
            let snapshot = read()?;
            screen.draw(|frame| {
                let area = frame.area();
                draw(area, frame.buffer_mut(), &theme, &snapshot);
            })?;
            last = Some(snapshot);
            while started.elapsed() < interval && !stop.load(Ordering::Acquire) {
                if super::term::hung_up() {
                    return Ok(());
                }
                let wait = interval
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(100));
                if event::poll(wait).map_err(|error| format!("read a key: {error}"))? {
                    if let Event::Key(key) =
                        event::read().map_err(|error| format!("read a key: {error}"))?
                    {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('c') if ctrl => return Ok(()),
                            // Ctrl+Z: to the shell, and back on fg.
                            KeyCode::Char('z') if ctrl => {
                                screen.suspend()?;
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    screen.close()?;
    if let Some(snapshot) = &last {
        let width = ratatui::crossterm::terminal::size()
            .map_or(100, |(columns, _)| columns.saturating_sub(1))
            .clamp(80, max_width);
        print!(
            "{}{}",
            plain(snapshot, width, &height, &draw),
            after(snapshot)
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_snapshot_keeps_every_line_within_its_width() {
        let snapshot = crate::tui::pool::tests::snapshot(2);
        for width in [80, 140] {
            let text = plain(
                &snapshot,
                width,
                |snapshot: &PoolSnapshot| crate::tui::pool::fixed_height(snapshot) + 10,
                &|area, buf: &mut Buffer, theme: &Theme, snapshot: &PoolSnapshot| {
                    crate::tui::pool::render(area, buf, theme, snapshot, None)
                },
            );
            assert!(text.contains("grow +1024M"), "{text}");
            for line in text.lines() {
                assert!(line.chars().count() <= width as usize, "{width}: {line}");
            }
        }
    }
}
