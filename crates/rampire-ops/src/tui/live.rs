//! `rampire top` and `rampire watch`: the TUI's dashboard or a VM's details
//! alone, refreshed until `q`, Esc or Ctrl+C, read-only. With `--once`, or
//! when stdout isn't a terminal, one plain snapshot is printed instead; on
//! leaving, the last view is left on the normal screen as plain text to
//! copy.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

/// `rampire watch`.
pub fn watch(command: crate::watch::Command, cwd: &Path) -> Result<(), String> {
    let (once, interval) = (command.once, command.interval);
    let details = live_details(&command);
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
            super::vm::render(body, buf, theme, snapshot, details.as_ref());
        },
        |snapshot| crate::watch::full_events(&snapshot.journal.events),
    )
}

/// The details `watch` shows beside the readings: the VM's state.
fn live_details(command: &crate::watch::Command) -> Option<super::vm::VmDetails> {
    let libvirt = crate::attest::Libvirt {
        connection: command.connect.clone(),
        timeout: Duration::from_secs(10),
    };
    let boot_bytes = libvirt
        .virsh(&["dumpxml", "--inactive", &command.vm])
        .ok()
        .and_then(|xml| crate::vm::inspect(&xml).ok())
        .map(|inspection| inspection.boot_bytes)
        .filter(|bytes| *bytes > 0);
    Some(super::vm::VmDetails {
        settings: None,
        boot_bytes,
        state: libvirt
            .state(&command.vm)
            .unwrap_or_else(|_| "unknown".to_owned()),
    })
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
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&stop))
            .map_err(|error| format!("install signal handler: {error}"))?;
    }
    let theme = super::theme_from_environment();
    let mut terminal =
        ratatui::try_init().map_err(|error| format!("open the terminal: {error}"))?;
    let mut last: Option<S> = None;
    let result = (|| -> Result<(), String> {
        while !stop.load(Ordering::Acquire) {
            let started = Instant::now();
            let snapshot = read()?;
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    draw(area, frame.buffer_mut(), &theme, &snapshot);
                })
                .map_err(|error| format!("draw: {error}"))?;
            last = Some(snapshot);
            while started.elapsed() < interval && !stop.load(Ordering::Acquire) {
                let wait = interval
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(100));
                if event::poll(wait).map_err(|error| format!("read a key: {error}"))? {
                    if let Event::Key(key) =
                        event::read().map_err(|error| format!("read a key: {error}"))?
                    {
                        let quit = key.kind == KeyEventKind::Press
                            && (matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                                || (key.code == KeyCode::Char('c')
                                    && key.modifiers.contains(KeyModifiers::CONTROL)));
                        if quit {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    })();
    ratatui::try_restore().map_err(|error| format!("restore the terminal: {error}"))?;
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
