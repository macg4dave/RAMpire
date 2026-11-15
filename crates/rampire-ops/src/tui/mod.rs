//! The TUI: one full-screen program on the KVM host, on its console or over
//! `ssh -t HOST sudo rampire`. It shows the pool and each VM, adds and
//! removes VMs, and runs the doctor, through the same flows as the
//! commands. `rampire top` and `rampire watch` draw its dashboard and VM
//! details alone ([`live`]).
//!
//! Keys are plain letters, arrows, Enter, Esc and Tab, so they work in any
//! terminal, tmux and screen. Every action names the command that does the
//! same, and is logged to the journal with the person and `via=tui`.

pub mod actions;
pub mod app;
pub mod jobs;
pub mod live;
pub mod notify;
pub mod pool;
pub mod screens;
pub mod theme;
pub mod vm;
pub mod widgets;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event};

use crate::context::Context;
use theme::{Depth, Icons, Palette, Theme};

/// The look for this terminal: the midnight palette at the terminal's depth,
/// with Unicode icons where the locale allows.
pub fn theme_from_environment() -> Theme {
    Theme::new(
        Palette::Terminal,
        Depth::from_environment(),
        Icons::from_environment(),
    )
}

/// Run the TUI until the person quits or the terminal goes away. The
/// terminal is given back as it was however it ends; a failure is printed
/// and logged to the journal.
pub fn run(context: Context) -> Result<(), String> {
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_app(context.clone())));
    let failure = match result {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(panic) => panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_else(|| "it panicked".to_owned()),
    };
    // The terminal is restored by now (ratatui's panic hook, or below).
    crate::audit::record(&context, "tui_failed", None, &[("reason", &failure)]);
    Err(format!(
        "the TUI stopped: {failure}\nThis is logged in the journal: journalctl -t {} --since today",
        crate::audit::IDENTIFIER
    ))
}

/// A flag set by the first of `signals`; a second one ends the process at
/// once, so a TUI that can't stop cleanly can still be stopped.
pub(crate) fn stop_on_signals(signals: &[i32]) -> Result<Arc<AtomicBool>, String> {
    let stop = Arc::new(AtomicBool::new(false));
    for signal in signals {
        signal_hook::flag::register_conditional_shutdown(*signal, 1, Arc::clone(&stop))
            .and_then(|_| signal_hook::flag::register(*signal, Arc::clone(&stop)))
            .map_err(|error| format!("install signal handler: {error}"))?;
    }
    Ok(stop)
}

/// Whether the terminal on stdin has hung up or gone.
pub(crate) fn hung_up() -> bool {
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
    }
    const POLLERR: i16 = 0x8;
    const POLLHUP: i16 = 0x10;
    const POLLNVAL: i16 = 0x20;
    let mut descriptor = PollFd {
        fd: 0,
        events: 0,
        revents: 0,
    };
    // SAFETY: one caller-owned pollfd, and a zero timeout.
    let ready = unsafe { poll(&mut descriptor, 1, 0) };
    ready < 0 || descriptor.revents & (POLLERR | POLLHUP | POLLNVAL) != 0
}

fn run_app(context: Context) -> Result<(), String> {
    let stop = stop_on_signals(&[signal_hook::consts::SIGHUP, signal_hook::consts::SIGTERM])?;
    let (tx, rx) = jobs::channel();
    let control = jobs::ReaderControl::new();
    jobs::spawn_reader(
        context.clone(),
        control.clone(),
        tx.clone(),
        Duration::from_secs(1),
    );
    let mut app = app::App::new(context, theme_from_environment(), tx, control.clone());
    let mut terminal =
        ratatui::try_init().map_err(|error| format!("open the terminal: {error}"))?;
    let result = (|| -> Result<(), String> {
        let mut dirty = true;
        while !app.quit && !stop.load(Ordering::Acquire) {
            // A terminal that has gone away (a dropped SSH connection
            // under sudo's own pty sends no SIGHUP) ends the TUI.
            if hung_up() {
                return Ok(());
            }
            // Notices slide in and out, so keep drawing while any show.
            dirty |= app.notices.busy();
            if dirty {
                terminal
                    .draw(|frame| screens::draw(frame, &app))
                    .map_err(|error| format!("draw: {error}"))?;
                dirty = false;
            }
            // Short polls: keys answer at once, and readings show as they come.
            let wait = if app.notices.busy() { 40 } else { 100 };
            if event::poll(Duration::from_millis(wait))
                .map_err(|error| format!("read a key: {error}"))?
            {
                if let Event::Key(key) =
                    event::read().map_err(|error| format!("read a key: {error}"))?
                {
                    app.on_key(key);
                }
                dirty = true;
            }
            app.tick();
            while let Ok(msg) = rx.try_recv() {
                app.on_msg(msg);
                dirty = true;
            }
        }
        Ok(())
    })();
    control.stop.store(true, Ordering::Release);
    ratatui::try_restore().map_err(|error| format!("restore the terminal: {error}"))?;
    result
}
